//! Ignore rules / acknowledgments for findings.
//!
//! Loads `.perf-sentinel-acknowledgments.toml`, computes a canonical
//! signature per [`Finding`], filters findings flagged as acknowledged
//! at the post-processing stage, and re-evaluates the quality gate on
//! the surviving set so an ack can flip a previously failing gate to
//! green.
//!
//! This is the CI / batch-mode side of the ack workflow. The daemon
//! runtime ack store lives at `crate::daemon::ack` and shares the
//! signature format defined here. The two are unioned at query time
//! with TOML winning on conflict (immutable baseline shipped via PR
//! review).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::detect::Finding;
use crate::quality_gate;
use crate::report::{AcknowledgedFinding, Report, Warning, warnings};

/// Hard cap on the size of `.perf-sentinel-acknowledgments.toml`. Mirrors
/// the trace-ingest payload-cap discipline so a stray
/// `--acknowledgments /dev/zero` or a multi-GB malformed TOML cannot
/// silently exhaust process memory.
pub const MAX_ACKNOWLEDGMENTS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Where the report handed to [`apply_to_report`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOrigin {
    /// Traces analyzed by this process, findings unfiltered.
    FreshAnalysis,
    /// A parsed Report JSON (baseline file, daemon snapshot), possibly
    /// already ack-filtered and with foreign or absent I/O op counts.
    Precomputed,
}

/// A single acknowledgment entry deserialized from the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acknowledgment {
    /// Canonical signature: `<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix>`.
    pub signature: String,
    /// Email or identifier of the user who created the ack.
    pub acknowledged_by: String,
    /// ISO 8601 date when the ack was created (`YYYY-MM-DD`).
    pub acknowledged_at: String,
    /// Free-text reason / context for the ack.
    pub reason: String,
    /// Optional ISO 8601 date (`YYYY-MM-DD`) at which the ack expires.
    /// `None` means the ack is permanent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Optional service of the acked finding (`.findings[].service`).
    /// With `source_endpoint`, lets an unmatched ack say whether its
    /// endpoint was exercised by the run at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Optional endpoint of the acked finding (`.findings[].source_endpoint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_endpoint: Option<String>,
}

/// Container for the deserialized TOML file.
///
/// The TOML root is `[[acknowledged]]` blocks. Empty file (no blocks)
/// deserializes to a default value, making "file exists but is empty" a
/// no-op.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcknowledgmentsFile {
    #[serde(default)]
    pub acknowledged: Vec<Acknowledgment>,
}

/// Compute the canonical signature of a finding.
///
/// Format: `<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>`.
/// The `sha256` prefix uses the first 16 bytes (32 hex characters), giving
/// ~128 bits of collision resistance. The triple
/// `(finding_type, service, sanitized_endpoint)` is already part of the
/// signature, so the hash only needs to disambiguate templates within the
/// same triple, an extremely small population in practice. The 32-char
/// prefix is defense in depth against accidental ack masking after a SQL
/// refactor or a service rename.
///
/// Sanitization replaces `/` and ` ` (space) inside `source_endpoint`
/// with `_` so the resulting signature uses `:` as a single, unambiguous
/// separator that operators can split on in shell pipelines. `BiDi`
/// override and invisible-format characters (Trojan Source, CVE-2021-42574)
/// are stripped from both `service` and `source_endpoint` so two visually
/// identical signatures cannot map to distinct ack entries.
#[must_use]
pub fn compute_signature(finding: &Finding) -> String {
    let mut hasher = Sha256::new();
    hasher.update(finding.pattern.template.as_bytes());
    let digest = hasher.finalize();
    let safe_service = crate::text_safety::strip_bidi_and_invisible(&finding.service);
    let sanitized_endpoint = sanitize_endpoint(&finding.source_endpoint);
    let safe_endpoint = crate::text_safety::strip_bidi_and_invisible(&sanitized_endpoint);
    let kind = finding.finding_type.as_str();
    // Pre-size: type + 2 separators + service + endpoint + ':' + 32 hex.
    let mut out = String::with_capacity(kind.len() + safe_service.len() + safe_endpoint.len() + 35);
    out.push_str(kind);
    out.push(':');
    out.push_str(safe_service.as_ref());
    out.push(':');
    out.push_str(safe_endpoint.as_ref());
    out.push(':');
    for byte in &digest[..16] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn sanitize_endpoint(endpoint: &str) -> Cow<'_, str> {
    if endpoint.bytes().any(|b| matches!(b, b'/' | b' ')) {
        Cow::Owned(endpoint.replace(['/', ' '], "_"))
    } else {
        Cow::Borrowed(endpoint)
    }
}

/// Fill in the `signature` field of every finding in place.
///
/// Idempotent: an existing signature is overwritten so re-running this
/// function on a baseline that already carries signatures (e.g. a
/// pre-0.5.17 dump that was just re-emitted) keeps the values fresh
/// against the current signature scheme.
pub fn enrich_with_signatures(findings: &mut [Finding]) {
    for finding in findings.iter_mut() {
        finding.signature = compute_signature(finding);
    }
}

/// True when a symlink resolves to a target under its own directory.
///
/// Refusing every symlink would make the file unusable from a Kubernetes
/// `ConfigMap`, which is the obvious way to ship it: the `kubelet` writes the
/// payload into a timestamped directory, points `..data` at it, and leaves one
/// symlink per key. The target never leaves the mount, so following it grants
/// no reach the caller did not already have by naming that directory.
///
/// A link resolving anywhere else stays refused, which is the case the check
/// guards against: a hostile link dropped in a CI working tree, aimed at a
/// sensitive file elsewhere on the host. Both sides are canonicalized first,
/// so a `..` segment in the target cannot walk back out.
fn symlink_stays_in_its_directory(path: &Path) -> bool {
    // A bare filename has an empty parent, which canonicalizes to nothing. Its
    // directory is the CWD, and reading it as "no directory" would refuse the
    // daemon's own default `.perf-sentinel-acknowledgments.toml` and every
    // `--acknowledgments <name>` relative to where the command was run.
    let parent = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => return false,
    };
    let (Ok(dir), Ok(target)) = (parent.canonicalize(), path.canonicalize()) else {
        return false;
    };
    target.starts_with(&dir)
}

/// Load acknowledgments from a TOML file.
///
/// Returns `Ok(default)` when the file does not exist, so a project
/// without any acks runs unfiltered, with zero error noise.
/// Returns `Err` on TOML parse failure or on a malformed `expires_at`
/// date so a typo in the ack file fails the run loud rather than
/// silently widening the matched set.
///
/// Reads with a hard cap of [`MAX_ACKNOWLEDGMENTS_FILE_BYTES`]. The TOML
/// crate has no public depth limiter, but the size cap keeps the worst
/// case bounded and rejects `/dev/zero` and the like.
///
/// # Errors
///
/// - [`AcknowledgmentLoadError::Io`] when the file exists but cannot be read.
/// - [`AcknowledgmentLoadError::TooLarge`] when the file exceeds the cap.
/// - [`AcknowledgmentLoadError::Parse`] when the TOML cannot be parsed.
/// - [`AcknowledgmentLoadError::InvalidDate`] when an `expires_at` value is
///   not a valid `YYYY-MM-DD` ISO 8601 date.
/// - [`AcknowledgmentLoadError::SymlinkRefused`] when the path is a symlink
///   resolving outside its own directory.
pub fn load_from_file(path: &Path) -> Result<AcknowledgmentsFile, AcknowledgmentLoadError> {
    Ok(load_from_file_if_present(path)?.unwrap_or_default())
}

/// Load acknowledgments, or `None` when the file is not there.
///
/// The daemon reload has to tell absence from an empty file: a deleted
/// `ConfigMap` or an unmounted volume must keep the previous acks rather than
/// un-acknowledge everything. Asking [`Path::exists`] first would answer that
/// question one syscall early, leave the file free to vanish before the read,
/// and fold a permission error into "not there".
///
/// # Errors
///
/// The same set as [`load_from_file`], minus the absent-file case.
pub fn load_from_file_if_present(
    path: &Path,
) -> Result<Option<AcknowledgmentsFile>, AcknowledgmentLoadError> {
    // See `symlink_stays_in_its_directory` for why a link is not refused
    // outright.
    // Use symlink_metadata so a symlink at the configured path does not
    // redirect the read to a sensitive file (e.g. a hostile collaborator
    // landing a symlink to /etc/passwd in a CI runner working tree). The
    // daemon JSONL store applies the same discipline at write time. This
    // mirrors it for the read-side baseline.
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() && !symlink_stays_in_its_directory(path) {
                return Err(AcknowledgmentLoadError::SymlinkRefused);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(err) => return Err(AcknowledgmentLoadError::Io(err)),
    }
    // The file can still vanish between the stat and the open, which is a
    // `ConfigMap` swap, not an edit. Report it as absent so the caller keeps
    // what it already had.
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(AcknowledgmentLoadError::Io(err)),
    };
    // `take(cap + 1)` closes the TOCTOU window between metadata().len()
    // and read(): we read at most cap+1 bytes, and reject if we hit the
    // cap+1th byte. Same pattern as `read_file_capped` in the CLI.
    let mut buf = String::new();
    file.take(MAX_ACKNOWLEDGMENTS_FILE_BYTES + 1)
        .read_to_string(&mut buf)
        .map_err(AcknowledgmentLoadError::Io)?;
    if buf.len() as u64 > MAX_ACKNOWLEDGMENTS_FILE_BYTES {
        return Err(AcknowledgmentLoadError::TooLarge {
            cap: MAX_ACKNOWLEDGMENTS_FILE_BYTES,
        });
    }
    let parsed: AcknowledgmentsFile =
        toml::from_str(&buf).map_err(AcknowledgmentLoadError::Parse)?;

    for (idx, ack) in parsed.acknowledged.iter().enumerate() {
        if let Some(ref expires) = ack.expires_at {
            NaiveDate::parse_from_str(expires, "%Y-%m-%d").map_err(|e| {
                AcknowledgmentLoadError::InvalidDate {
                    entry_index: idx,
                    field: "expires_at",
                    value: expires.clone(),
                    message: e.to_string(),
                }
            })?;
        }
    }

    Ok(Some(parsed))
}

/// Apply acknowledgments to a `Report` in place.
///
/// 1. Clears any prior `report.acknowledged_findings` so a Report fed
///    back through this function (e.g. a baseline JSON round-trip)
///    cannot accumulate stale ack pairs across runs.
/// 2. Filters `report.findings`, moving acked entries into
///    `report.acknowledged_findings`.
/// 3. Re-evaluates the quality gate on the surviving set so an ack can
///    flip a previously failing gate to green (the purpose of
///    "won't fix / accepted" semantics). Re-evaluation runs even when no
///    ack matched, so the gate field is always self-consistent with the
///    final `findings` slice.
///
/// Acks with an `expires_at` strictly before `now` are treated as inactive
/// and the corresponding finding is preserved in `report.findings`.
///
/// `origin` gates the unmatched-ack warnings: they are only derivable
/// from a fresh analysis. A pre-computed report may already be
/// ack-filtered, so an entry matching nothing there means "consumed on
/// the previous pass", not "fixed", and its `per_endpoint_io_ops` (empty
/// on daemon snapshots) describes another run entirely.
pub fn apply_to_report(
    report: &mut Report,
    acks: &AcknowledgmentsFile,
    config: &Config,
    now: DateTime<Utc>,
    origin: ReportOrigin,
) {
    // The caller may have loaded a baseline that already carried
    // `acknowledged_findings` from a previous `--show-acknowledged` run,
    // which we do not want to double-count or treat as authoritative.
    report.acknowledged_findings.clear();
    // Same reasoning for the warnings this function owns: a baseline
    // loaded from a previous run may already carry them.
    report
        .warning_details
        .retain(|w| w.kind != warnings::UNMATCHED_ACKNOWLEDGMENT);

    let active: HashMap<&str, &Acknowledgment> = acks
        .acknowledged
        .iter()
        .filter(|a| is_ack_active(a, now))
        .map(|a| (a.signature.as_str(), a))
        .collect();

    if !active.is_empty() {
        let mut matched: HashSet<&str> = HashSet::with_capacity(active.len());
        let original = std::mem::take(&mut report.findings);
        let mut kept = Vec::with_capacity(original.len());
        for finding in original {
            let sig = signature_cow(&finding);
            if let Some((ack_sig, ack)) = active.get_key_value(sig.as_ref()) {
                matched.insert(ack_sig);
                report.acknowledged_findings.push(AcknowledgedFinding {
                    finding,
                    acknowledgment: (*ack).clone(),
                });
            } else {
                kept.push(finding);
            }
        }
        report.findings = kept;

        // An ack that suppressed nothing is the "maybe fixed" signal.
        // Sorted so two runs of the same report stay diffable.
        if origin == ReportOrigin::FreshAnalysis {
            let mut unmatched: Vec<&Acknowledgment> = active
                .values()
                .filter(|a| !matched.contains(a.signature.as_str()))
                .copied()
                .collect();
            unmatched.sort_unstable_by(|a, b| a.signature.cmp(&b.signature));
            let observed: HashSet<(&str, &str)> = report
                .per_endpoint_io_ops
                .iter()
                .map(|e| (e.service.as_str(), e.endpoint.as_str()))
                .collect();
            let kept_signatures: Vec<(Cow<'_, str>, &Finding)> = report
                .findings
                .iter()
                .map(|f| (signature_cow(f), f))
                .collect();
            let new_warnings: Vec<Warning> = unmatched
                .iter()
                .map(|ack| {
                    let successor = drifted_successor(ack, &kept_signatures);
                    Warning::from_untrusted(
                        warnings::UNMATCHED_ACKNOWLEDGMENT,
                        &unmatched_message(ack, &observed, successor),
                    )
                })
                .collect();
            report.warning_details.extend(new_warnings);
        }
    }

    report.quality_gate = quality_gate::evaluate(
        &report.findings,
        &report.green_summary,
        &config.thresholds,
        report.analysis.ingest.as_ref(),
    );
}

/// The lone kept finding whose signature shares the ack's
/// `<type>:<service>:<endpoint>` prefix with a different template hash:
/// the signature of a template drift rather than a fix. `None` with zero
/// or several candidates, since naming one among several would be a guess.
///
/// The prefix is not injective: service and endpoint may contain `:`,
/// so two distinct pairs can collide on it. When the ack names its
/// `service` / `source_endpoint`, the candidate's structured fields are
/// checked too, which removes the collision. An ack without the fields
/// keeps the small residual risk and the message stays a hint.
///
/// Never transfers the ack itself: a signature is a suppression
/// boundary, and carrying an ack across a template change could silence
/// a new problem. The operator re-acknowledges by hand.
fn drifted_successor<'a>(
    ack: &Acknowledgment,
    kept: &'a [(Cow<'a, str>, &'a Finding)],
) -> Option<(&'a str, Drift)> {
    let acked = ack.signature.rsplit_once(':')?;
    let (mut templates, mut attributions) = (Vec::new(), Vec::new());
    for (sig, finding) in kept {
        match drift_kind(ack, acked, sig, finding) {
            Some(Drift::Template) => templates.push(sig.as_ref()),
            Some(Drift::Attribution) => attributions.push(sig.as_ref()),
            None => {}
        }
    }
    // A template drift (same service and endpoint) is the stronger
    // reading and outranks any attribution candidate. Two of a kind
    // stay a guess and fall back to the generic message.
    match (templates.as_slice(), attributions.as_slice()) {
        ([successor], _) => Some((*successor, Drift::Template)),
        ([], [successor]) => Some((*successor, Drift::Attribution)),
        _ => None,
    }
}

/// What moved, when a current finding can explain an ack's signature.
#[derive(Clone, Copy)]
enum Drift {
    /// Same detector, service and endpoint, different template hash: the
    /// query itself changed.
    Template,
    /// Same detector and template hash under a different prefix: the
    /// service or endpoint the finding is attributed to moved. A
    /// `service.name` that starts resolving differently produces this.
    Attribution,
}

/// Classify one current finding against an unmatched ack. The Template
/// arm requires the ack's structured fields, where present, to agree.
/// The Attribution arm checks the endpoint only: a candidate agreeing on
/// both fields would share the prefix and be a Template candidate, so
/// with fields present it names a service move.
fn drift_kind(
    ack: &Acknowledgment,
    (ack_prefix, ack_hash): (&str, &str),
    sig: &str,
    finding: &Finding,
) -> Option<Drift> {
    let (prefix, hash) = sig.rsplit_once(':')?;
    let endpoint_matches = ack
        .source_endpoint
        .as_ref()
        .is_none_or(|e| e == &finding.source_endpoint);
    if prefix == ack_prefix && hash != ack_hash {
        let service_matches = ack.service.as_ref().is_none_or(|s| s == &finding.service);
        return (service_matches && endpoint_matches).then_some(Drift::Template);
    }
    if hash == ack_hash && prefix != ack_prefix {
        // The prefix is not injective (a service or endpoint may hold a
        // colon), so the detector is checked by prefix rather than parsed
        // out. Two candidates still collapse to the generic message.
        let same_kind = ack_prefix
            .strip_prefix(finding.finding_type.as_str())
            .is_some_and(|rest| rest.starts_with(':'));
        return (same_kind && endpoint_matches).then_some(Drift::Attribution);
    }
    None
}

/// A finding's stored signature, computed on the fly when the report
/// predates enrichment.
fn signature_cow(finding: &Finding) -> Cow<'_, str> {
    if finding.signature.is_empty() {
        Cow::Owned(compute_signature(finding))
    } else {
        Cow::Borrowed(finding.signature.as_str())
    }
}

/// Message for an active ack that suppressed nothing. When exactly one
/// current finding shares the ack's detector, service, and endpoint with
/// a different template hash, the template drifted and the message names
/// the successor signature. Otherwise, when the entry names its service
/// and endpoint, the run's per-endpoint I/O ops say whether that
/// endpoint did I/O, which splits "fixed" from "scenario did not run".
/// A successor whose template hash is unchanged means the attribution
/// moved instead, which the message says rather than reading as "fixed".
/// The counts only hold endpoints that emitted I/O spans, so absence
/// stays ambiguous (not exercised, or a fix that removed the I/O
/// outright) and the message says so. Entries without the fields keep
/// the indeterminate double reading.
fn unmatched_message(
    ack: &Acknowledgment,
    observed: &HashSet<(&str, &str)>,
    successor: Option<(&str, Drift)>,
) -> String {
    let sig = &ack.signature;
    if let Some((successor, drift)) = successor {
        // "Moved" implies the old attribution is gone: while the acked
        // (service, endpoint) still emits I/O, a same-template finding
        // elsewhere is a sibling and the observed verdict below applies.
        let old_still_emits = matches!(
            (&ack.service, &ack.source_endpoint),
            (Some(s), Some(e)) if observed.contains(&(s.as_str(), e.as_str()))
        );
        let why = match drift {
            Drift::Template => Some(
                "with the same detector, service, and endpoint: the \
                 template drifted (schema or query change)",
            ),
            Drift::Attribution if old_still_emits => None,
            Drift::Attribution => Some(
                "with the same detector and template under a different \
                 service or endpoint: the attribution moved, not the query",
            ),
        };
        if let Some(why) = why {
            return format!(
                "acknowledgment {sig} matched no finding in this run, but \
                 {successor} fired {why}, re-acknowledge the new signature \
                 if the reason still holds"
            );
        }
    }
    match (&ack.service, &ack.source_endpoint) {
        (Some(service), Some(endpoint)) => {
            if observed.contains(&(service.as_str(), endpoint.as_str())) {
                format!(
                    "acknowledgment {sig} matched no finding in this run: \
                     {service} {endpoint} was exercised and the finding did not \
                     fire, the problem looks fixed and the entry can be removed"
                )
            } else {
                format!(
                    "acknowledgment {sig} matched no finding in this run: \
                     {service} {endpoint} emitted no I/O in this run (not \
                     exercised, or its I/O was removed outright), so this \
                     proves nothing, keep the entry"
                )
            }
        }
        _ => format!(
            "acknowledgment {sig} matched no finding in this run: \
             the problem is either fixed, and the entry can be removed, \
             or the scenario that produced it did not run (add service and \
             source_endpoint to the entry to tell the two apart)"
        ),
    }
}

pub(crate) fn is_ack_active(ack: &Acknowledgment, now: DateTime<Utc>) -> bool {
    let Some(ref expires) = ack.expires_at else {
        return true;
    };
    let Ok(parsed) = NaiveDate::parse_from_str(expires, "%Y-%m-%d") else {
        // Malformed dates are rejected at load time. Defensively treat a
        // bad value as inactive rather than ack-everything.
        return false;
    };
    // Treat the entire expiry day as still valid: an ack `expires_at =
    // 2026-12-31` is honored through 2026-12-31 23:59:59 UTC.
    let Some(end_of_day) = parsed.and_hms_opt(23, 59, 59) else {
        return false;
    };
    end_of_day.and_utc() >= now
}

/// Errors that can occur when loading the acknowledgments file.
#[derive(Debug, thiserror::Error)]
pub enum AcknowledgmentLoadError {
    #[error("Failed to read acknowledgments file: {0}")]
    Io(#[from] std::io::Error),

    #[error("Acknowledgments file exceeds the {cap}-byte cap")]
    TooLarge { cap: u64 },

    #[error("Failed to parse acknowledgments TOML: {0}")]
    Parse(toml::de::Error),

    #[error("Entry {entry_index}: invalid {field} value '{value}': {message}")]
    InvalidDate {
        entry_index: usize,
        field: &'static str,
        value: String,
        message: String,
    },

    #[error(
        "Acknowledgments file is a symlink resolving outside its own directory, refusing to follow"
    )]
    SymlinkRefused,
}

#[cfg(all(test, unix))]
mod symlink_tests {
    use super::*;
    use std::path::PathBuf;

    /// Reproduce how Kubernetes projects a `ConfigMap`: the real file lives in a
    /// timestamped directory, `..data` points at it, and each key is a symlink
    /// through that indirection. Nothing escapes the mount.
    fn project_like_kubernetes(dir: &Path, name: &str, body: &str) -> PathBuf {
        let data = dir.join("..2026_08_14_09_00_00");
        std::fs::create_dir_all(&data).expect("create data dir");
        std::fs::write(data.join(name), body).expect("write payload");
        std::os::unix::fs::symlink("..2026_08_14_09_00_00", dir.join("..data"))
            .expect("link ..data");
        std::os::unix::fs::symlink(Path::new("..data").join(name), dir.join(name))
            .expect("link key");
        dir.join(name)
    }

    #[test]
    fn a_configmap_projection_is_readable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = project_like_kubernetes(
            dir.path(),
            "acks.toml",
            "[[acknowledged]]\nsignature = \"a:b:c:d\"\nacknowledged_by = \"x\"\nacknowledged_at = \"2026-08-14T00:00:00Z\"\nreason = \"y\"\n",
        );
        let file = load_from_file(&path).expect("a ConfigMap mount must load");
        assert_eq!(file.acknowledged.len(), 1);
    }

    #[test]
    fn a_symlink_escaping_the_directory_is_still_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(outside.path().join("secret.toml"), "").expect("write");
        let link = dir.path().join("acks.toml");
        std::os::unix::fs::symlink(outside.path().join("secret.toml"), &link).expect("link");
        assert!(
            matches!(
                load_from_file(&link),
                Err(AcknowledgmentLoadError::SymlinkRefused)
            ),
            "a link pointing outside its own directory stays refused"
        );
    }
}

#[cfg(test)]
mod tests;
