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
/// Refusing every symlink made the file unusable from a Kubernetes `ConfigMap`,
/// which is the obvious way to ship it: the `kubelet` writes the payload into a
/// timestamped directory, points `..data` at it, and leaves one symlink per
/// key. The target never leaves the mount, so following it grants no reach the
/// caller did not already have by naming that directory.
///
/// A link resolving anywhere else stays refused, which is the case the check
/// was written for: a hostile link dropped in a CI working tree, aimed at a
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
/// without any acks observes the legacy behavior with zero error noise.
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
    // daemon JSONL store applies the same discipline at write time, this
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
///    flip a previously failing gate to green (the entire point of
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
    // Drop any prior ack pairs from the source Report. The caller may
    // have loaded a baseline that already carried `acknowledged_findings`
    // from a previous `--show-acknowledged` run, which we do not want to
    // double-count or treat as authoritative.
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
/// or several candidates, naming one among several would be a guess.
///
/// The prefix is not injective: service and endpoint may contain `:`,
/// so two distinct pairs can collide on it. When the ack names its
/// `service` / `source_endpoint`, the candidate's structured fields are
/// checked too, which removes the collision. An ack without the fields
/// keeps the small residual risk and the message stays a hint.
///
/// Never transfers the ack itself: a signature is a suppression
/// boundary, and carrying an ack across a template change could silence
/// a genuinely new problem. The operator re-acknowledges deliberately.
fn drifted_successor<'a>(
    ack: &Acknowledgment,
    kept: &'a [(Cow<'a, str>, &'a Finding)],
) -> Option<(&'a str, Drift)> {
    let (ack_prefix, ack_hash) = ack.signature.rsplit_once(':')?;
    let mut found: Option<(&str, Drift)> = None;
    for (sig, finding) in kept {
        let Some((prefix, hash)) = sig.rsplit_once(':') else {
            continue;
        };
        if let Some(drift) = drift_kind(ack, (ack_prefix, ack_hash), (prefix, hash), finding) {
            if found.is_some() {
                return None;
            }
            found = Some((sig.as_ref(), drift));
        }
    }
    found
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

/// Classify one current finding against an unmatched ack. Both arms stay
/// conservative: a candidate is only named when the ack's own structured
/// fields, where present, agree with it.
fn drift_kind(
    ack: &Acknowledgment,
    acked: (&str, &str),
    current: (&str, &str),
    finding: &Finding,
) -> Option<Drift> {
    let ((ack_prefix, ack_hash), (prefix, hash)) = (acked, current);
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
    if let Some((successor, Drift::Template)) = successor {
        return format!(
            "acknowledgment {sig} matched no finding in this run, but \
             {successor} fired with the same detector, service, and \
             endpoint: the template drifted (schema or query change), \
             re-acknowledge the new signature if the reason still holds"
        );
    }
    if let Some((successor, Drift::Attribution)) = successor {
        return format!(
            "acknowledgment {sig} matched no finding in this run, but \
             {successor} fired with the same detector and template under a \
             different service or endpoint: the attribution moved, not the \
             query, re-acknowledge the new signature if the reason still holds"
        );
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
        // Malformed dates are rejected at load time; defensively treat a
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
mod tests {
    use super::*;
    use crate::detect::{FindingType, Severity};
    use crate::report::{Analysis, GreenSummary, QualityGate};
    use crate::test_helpers::make_finding;
    use chrono::TimeZone;
    use core::assert_matches;

    #[test]
    fn a_bare_filename_resolves_against_the_current_directory() {
        // `Path::parent` of a bare name is the empty path, which canonicalizes
        // to nothing. Reading that as "no directory" would refuse the daemon's
        // own CWD-relative default, on every platform, so this stays out of the
        // unix-only symlink module. `Cargo.toml` is the crate root's own file,
        // so it is under the CWD by construction and needs no fixture.
        assert!(symlink_stays_in_its_directory(Path::new("Cargo.toml")));
    }

    #[test]
    fn an_absent_file_is_told_from_an_empty_one() {
        // The daemon reload keeps the previous acks on absence and replaces
        // them on an empty file, so the two must not answer alike.
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.toml");
        assert!(
            load_from_file_if_present(&missing)
                .expect("absence is not an error")
                .is_none()
        );
        let empty = dir.path().join("empty.toml");
        std::fs::write(&empty, "").expect("write");
        assert!(
            load_from_file_if_present(&empty)
                .expect("an empty file parses")
                .is_some_and(|f| f.acknowledged.is_empty())
        );
    }

    fn empty_report(findings: Vec<Finding>) -> Report {
        Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: findings.len(),
                traces_analyzed: 1,
                ingest: None,
            },
            findings,
            green_summary: GreenSummary::disabled(0),
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            embedded_traces: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: String::new(),
            detection_config: None,
            disclosure_waste: None,
        }
    }

    fn ack(signature: &str, expires_at: Option<&str>) -> Acknowledgment {
        Acknowledgment {
            signature: signature.to_string(),
            acknowledged_by: "test@example.com".to_string(),
            acknowledged_at: "2026-05-02".to_string(),
            reason: "test".to_string(),
            expires_at: expires_at.map(str::to_string),
            service: None,
            source_endpoint: None,
        }
    }

    fn now_2026_05_02() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap()
    }

    #[test]
    fn compute_signature_deterministic() {
        let f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let sig1 = compute_signature(&f);
        let sig2 = compute_signature(&f);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn compute_signature_differs_with_template() {
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.pattern.template = "SELECT * FROM users WHERE id = ?".to_string();
        f2.pattern.template = "SELECT * FROM orders WHERE id = ?".to_string();
        assert_ne!(compute_signature(&f1), compute_signature(&f2));
    }

    #[test]
    fn compute_signature_sanitizes_endpoint() {
        let mut f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        f.source_endpoint = "GET /api/foo bar".to_string();
        let sig = compute_signature(&f);
        let parts: Vec<&str> = sig.split(':').collect();
        assert_eq!(
            parts.len(),
            4,
            "signature must have 4 colon-separated parts: {sig}"
        );
        assert!(
            !parts[2].contains('/'),
            "endpoint segment must not contain '/'"
        );
        assert!(
            !parts[2].contains(' '),
            "endpoint segment must not contain ' '"
        );
    }

    #[test]
    fn compute_signature_strips_bidi_and_invisible_from_service_and_endpoint() {
        // service "alice<RLO>@evil.com" should produce the same signature as
        // "alice@evil.com" so a hostile span attribute cannot fork ack matching.
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.service = "alice\u{202E}@evil.com".to_string();
        f1.source_endpoint = "GET /api/items\u{200B}".to_string();
        f2.service = "alice@evil.com".to_string();
        f2.source_endpoint = "GET /api/items".to_string();
        assert_eq!(
            compute_signature(&f1),
            compute_signature(&f2),
            "BiDi/invisible characters must be stripped before signature construction"
        );
    }

    #[test]
    fn compute_signature_format_matches_brief() {
        let mut f = make_finding(FindingType::RedundantSql, Severity::Warning);
        f.service = "order-service".to_string();
        f.source_endpoint = "POST /api/orders".to_string();
        f.pattern.template = "SELECT 1".to_string();
        let sig = compute_signature(&f);
        // Format: redundant_sql:order-service:POST_/api/orders → after sanitization
        // POST_/api/orders becomes POST__api_orders.
        let mut parts = sig.splitn(4, ':');
        assert_eq!(parts.next(), Some("redundant_sql"));
        assert_eq!(parts.next(), Some("order-service"));
        assert_eq!(parts.next(), Some("POST__api_orders"));
        let hex = parts.next().expect("hex prefix present");
        assert_eq!(hex.len(), 32, "hex prefix is 32 characters (16 bytes)");
        assert!(
            hex.chars().all(|c| c.is_ascii_hexdigit()),
            "hex prefix is hex"
        );
    }

    #[test]
    fn signature_stable_across_trace_id_changes() {
        // Core ack contract: a service restart produces new trace_id and
        // span_id values, but the same finding type on the same service /
        // endpoint / template must yield the same signature. Without this
        // invariant, ack entries silently stop matching after a restart.
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.trace_id = "aaaaaaaaaaaaaaaa0000000000000000".to_string();
        f2.trace_id = "ffffffffffffffff1111111111111111".to_string();
        assert_ne!(f1.trace_id, f2.trace_id);
        assert_eq!(
            compute_signature(&f1),
            compute_signature(&f2),
            "signature must not depend on trace_id (acks survive service restarts)"
        );
    }

    #[test]
    fn compute_signature_differs_with_endpoint() {
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.source_endpoint = "POST /api/orders".to_string();
        f2.source_endpoint = "POST /api/users".to_string();
        assert_ne!(compute_signature(&f1), compute_signature(&f2));
    }

    #[test]
    fn compute_signature_differs_with_service() {
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.service = "order-svc".to_string();
        f2.service = "user-svc".to_string();
        assert_ne!(compute_signature(&f1), compute_signature(&f2));
    }

    #[test]
    fn compute_signature_differs_with_finding_type() {
        let f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let f2 = make_finding(FindingType::RedundantSql, Severity::Warning);
        assert_ne!(compute_signature(&f1), compute_signature(&f2));
    }

    #[test]
    fn load_from_file_rejects_oversized_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        let payload = vec![b'x'; (MAX_ACKNOWLEDGMENTS_FILE_BYTES + 1) as usize];
        std::fs::write(&path, &payload).unwrap();
        let err = load_from_file(&path).expect_err("oversized file must fail");
        assert!(
            matches!(err, AcknowledgmentLoadError::TooLarge { .. }),
            "expected TooLarge, got: {err:?}"
        );
    }

    #[test]
    fn apply_to_report_clears_prior_acked_entries() {
        // Simulate a Report fed back from a previous --show-acknowledged
        // run: it carries one stale ack pair. Applying a fresh empty
        // ack file must drop the stale pair, the gate is re-evaluated,
        // and findings are unchanged.
        let stale_finding = make_finding(FindingType::SlowSql, Severity::Warning);
        let stale_ack = Acknowledgment {
            signature: "stale".to_string(),
            acknowledged_by: "stale@example.com".to_string(),
            acknowledged_at: "2020-01-01".to_string(),
            reason: "from a previous run".to_string(),
            expires_at: None,
            service: None,
            source_endpoint: None,
        };
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let mut report = empty_report(findings);
        report.acknowledged_findings.push(AcknowledgedFinding {
            finding: stale_finding,
            acknowledgment: stale_ack,
        });
        let acks = AcknowledgmentsFile::default();
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert!(
            report.acknowledged_findings.is_empty(),
            "stale ack pair must be cleared on entry"
        );
        assert_eq!(report.findings.len(), 1, "active findings preserved");
    }

    #[test]
    fn load_from_file_nonexistent_returns_empty() {
        let path = std::path::PathBuf::from("/tmp/perf-sentinel-acks-does-not-exist.toml");
        let result = load_from_file(&path).expect("missing file should be Ok");
        assert!(result.acknowledged.is_empty());
    }

    #[test]
    fn load_from_file_valid_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        std::fs::write(
            &path,
            r#"
[[acknowledged]]
signature = "n_plus_one_sql:svc:GET_/a:abcd1234abcd1234abcd1234abcd1234"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "documented"

[[acknowledged]]
signature = "redundant_sql:svc:POST_/b:11223344112233441122334411223344"
acknowledged_by = "bob@example.com"
acknowledged_at = "2026-04-20"
reason = "won't fix"
expires_at = "2026-12-31"
"#,
        )
        .unwrap();
        let parsed = load_from_file(&path).expect("valid TOML parses");
        assert_eq!(parsed.acknowledged.len(), 2);
        assert_eq!(parsed.acknowledged[0].acknowledged_by, "alice@example.com");
        assert_eq!(
            parsed.acknowledged[1].expires_at.as_deref(),
            Some("2026-12-31")
        );
    }

    #[test]
    fn load_from_file_missing_signature_field_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        std::fs::write(
            &path,
            r#"
[[acknowledged]]
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "missing signature"
"#,
        )
        .unwrap();
        let err = load_from_file(&path).expect_err("missing field must fail");
        assert_matches!(err, AcknowledgmentLoadError::Parse(_));
    }

    #[test]
    fn load_from_file_invalid_expires_at_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        std::fs::write(
            &path,
            r#"
[[acknowledged]]
signature = "redundant_sql:svc:POST_/b:11223344112233441122334411223344"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "bad date"
expires_at = "not-a-date"
"#,
        )
        .unwrap();
        let err = load_from_file(&path).expect_err("invalid date must fail");
        assert_matches!(
            err,
            AcknowledgmentLoadError::InvalidDate {
                field: "expires_at",
                ..
            }
        );
    }

    #[test]
    fn apply_to_report_filters_matching() {
        let mut findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            make_finding(FindingType::RedundantSql, Severity::Warning),
            make_finding(FindingType::SlowSql, Severity::Warning),
        ];
        // Distinguish the templates so signatures differ.
        findings[0].pattern.template = "T1".to_string();
        findings[1].pattern.template = "T2".to_string();
        findings[2].pattern.template = "T3".to_string();
        enrich_with_signatures(&mut findings);
        let target_sig = findings[1].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, None)],
        };
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.acknowledged_findings.len(), 1);
        assert_eq!(
            report.acknowledged_findings[0].finding.signature,
            target_sig
        );
    }

    #[test]
    fn apply_to_report_no_match_keeps_all() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(
                "n_plus_one_sql:nope:nope:00000000000000000000000000000000",
                None,
            )],
        };
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert_eq!(report.findings.len(), 1);
        assert!(report.acknowledged_findings.is_empty());
    }

    #[test]
    fn apply_to_report_expired_ack_ignored() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, Some("2020-01-01"))],
        };
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert_eq!(report.findings.len(), 1);
        assert!(report.acknowledged_findings.is_empty());
    }

    /// The signal a fix produces: the entry is still active, nothing in
    /// the run carries its signature, so it is reported as removable.
    #[test]
    fn apply_to_report_reports_an_ack_that_matched_nothing() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack("deadbeef", None)],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        assert_eq!(report.findings.len(), 1, "the unrelated finding survives");
        let unmatched: Vec<&Warning> = report
            .warning_details
            .iter()
            .filter(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .collect();
        assert_eq!(unmatched.len(), 1);
        assert!(
            unmatched[0].message.contains("deadbeef"),
            "the warning must name the entry to remove, got: {}",
            unmatched[0].message
        );
    }

    /// A pre-computed report may already be ack-filtered and its I/O op
    /// counts describe another run, so no unmatched warning may be derived
    /// from it, not even the indeterminate one.
    #[test]
    fn apply_to_report_precomputed_origin_emits_no_unmatched_warning() {
        let mut report = empty_report(vec![]);
        report.per_endpoint_io_ops = vec![crate::report::PerEndpointIoOps {
            service: "order-service".to_string(),
            endpoint: "GET /api/orders".to_string(),
            io_ops: 12,
        }];
        let acks = AcknowledgmentsFile {
            acknowledged: vec![Acknowledgment {
                service: Some("order-service".to_string()),
                source_endpoint: Some("GET /api/orders".to_string()),
                ..ack("deadbeef", None)
            }],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::Precomputed,
        );
        assert!(
            !report
                .warning_details
                .iter()
                .any(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT),
            "a precomputed report must not claim anything, got: {:?}",
            report.warning_details
        );
    }

    /// With service and endpoint on the entry, an exercised endpoint that
    /// produced no finding reads as fixed, an absent one proves nothing.
    #[test]
    fn apply_to_report_unmatched_ack_splits_fixed_from_not_run() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let mut report = empty_report(findings);
        report.per_endpoint_io_ops = vec![crate::report::PerEndpointIoOps {
            service: "order-service".to_string(),
            endpoint: "GET /api/orders".to_string(),
            io_ops: 12,
        }];
        let located = |sig: &str, endpoint: &str| Acknowledgment {
            service: Some("order-service".to_string()),
            source_endpoint: Some(endpoint.to_string()),
            ..ack(sig, None)
        };
        let acks = AcknowledgmentsFile {
            acknowledged: vec![
                located("aaaa-exercised", "GET /api/orders"),
                located("bbbb-not-run", "GET /api/legacy/export"),
            ],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        let messages: Vec<&str> = report
            .warning_details
            .iter()
            .filter(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .map(|w| w.message.as_str())
            .collect();
        assert_eq!(messages.len(), 2);
        assert!(
            messages[0].contains("aaaa-exercised") && messages[0].contains("looks fixed"),
            "exercised endpoint must read as fixed, got: {}",
            messages[0]
        );
        assert!(
            messages[1].contains("bbbb-not-run") && messages[1].contains("proves nothing"),
            "absent endpoint must prove nothing, got: {}",
            messages[1]
        );
    }

    /// A template mutation shifts the signature's hash suffix while the
    /// detector, service, and endpoint stay put. With exactly one such
    /// finding, the warning names it instead of claiming "looks fixed".
    #[test]
    fn apply_to_report_unmatched_ack_names_the_drifted_successor() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let successor_sig = findings[0].signature.clone();
        let (prefix, _) = successor_sig.rsplit_once(':').expect("4-segment signature");
        let acked_old = format!("{prefix}:{}", "0".repeat(32));

        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&acked_old, None)],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        let warning = report
            .warning_details
            .iter()
            .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .expect("unmatched warning");
        assert!(
            warning.message.contains(&successor_sig) && warning.message.contains("drifted"),
            "warning must name the successor signature, got: {}",
            warning.message
        );
        assert_eq!(report.findings.len(), 1, "the successor stays unsuppressed");
    }

    /// A `service.name` that starts resolving differently moves the
    /// signature's prefix while the template hash stays put. The warning
    /// must name the successor instead of reading as "possibly fixed",
    /// which is what would send an operator to delete a live suppression.
    #[test]
    fn apply_to_report_unmatched_ack_names_the_reattributed_successor() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        findings[0].service = "unknown".to_string();
        enrich_with_signatures(&mut findings);
        let successor_sig = findings[0].signature.clone();

        // The same finding as it signed before the service resolved.
        let mut before = findings[0].clone();
        before.service = String::new();
        let acked_old = compute_signature(&before);
        assert_ne!(acked_old, successor_sig);

        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&acked_old, None)],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        let warning = report
            .warning_details
            .iter()
            .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .expect("unmatched warning");
        assert!(
            warning.message.contains(&successor_sig) && warning.message.contains("attribution"),
            "warning must name the re-attributed successor, got: {}",
            warning.message
        );
        assert_eq!(report.findings.len(), 1, "the successor stays unsuppressed");
    }

    /// Two findings sharing the ack's prefix would make naming either one
    /// a guess, so the message stays generic.
    #[test]
    fn apply_to_report_two_drift_candidates_keep_the_generic_message() {
        let mut second = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        second.pattern.template = "SELECT * FROM t WHERE id = ? AND tenant = ?".to_string();
        let mut findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            second,
        ];
        enrich_with_signatures(&mut findings);
        let (prefix, _) = findings[0]
            .signature
            .rsplit_once(':')
            .map(|(p, h)| (p.to_string(), h))
            .expect("4-segment signature");
        let acked_old = format!("{prefix}:{}", "0".repeat(32));

        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&acked_old, None)],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        let warning = report
            .warning_details
            .iter()
            .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .expect("unmatched warning");
        assert!(
            !warning.message.contains("drifted"),
            "two candidates must not be guessed between, got: {}",
            warning.message
        );
    }

    /// The signature prefix is not injective when service or endpoint
    /// contains a colon. An ack carrying its structured fields must not
    /// name a prefix-colliding but unrelated finding as its successor.
    #[test]
    fn drift_hint_rejects_a_prefix_collision_when_fields_are_present() {
        // service "svc:extra" + endpoint "foo" collides with
        // service "svc" + endpoint "extra:foo" on the raw prefix.
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.service = "svc".to_string();
        finding.source_endpoint = "extra:foo".to_string();
        let mut findings = vec![finding];
        enrich_with_signatures(&mut findings);
        let (prefix, _) = findings[0]
            .signature
            .rsplit_once(':')
            .map(|(p, h)| (p.to_string(), h))
            .expect("4-segment signature");
        let acked_old = format!("{prefix}:{}", "0".repeat(32));

        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![Acknowledgment {
                service: Some("svc:extra".to_string()),
                source_endpoint: Some("foo".to_string()),
                ..ack(&acked_old, None)
            }],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        let warning = report
            .warning_details
            .iter()
            .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .expect("unmatched warning");
        assert!(
            !warning.message.contains("drifted"),
            "a prefix collision must not be sold as a drift, got: {}",
            warning.message
        );
    }

    /// An ack doing its job is not noise, and an expired one is inactive,
    /// so neither may be reported as removable.
    #[test]
    fn apply_to_report_does_not_report_matched_or_expired_acks() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![
                ack(&target_sig, None),
                ack("expired-and-unmatched", Some("2020-01-01")),
            ],
        };
        apply_to_report(
            &mut report,
            &acks,
            &Config::default(),
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        assert_eq!(report.acknowledged_findings.len(), 1);
        assert!(
            !report
                .warning_details
                .iter()
                .any(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT),
            "got: {:?}",
            report.warning_details
        );
    }

    /// Re-applying over a baseline that already carries the warnings must
    /// not stack them, the same reason ack pairs are cleared on entry.
    #[test]
    fn apply_to_report_does_not_accumulate_unmatched_warnings() {
        let mut report = empty_report(vec![]);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack("deadbeef", None)],
        };
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );

        assert_eq!(
            report
                .warning_details
                .iter()
                .filter(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
                .count(),
            1
        );
    }

    #[test]
    fn apply_to_report_future_ack_applied() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, Some("2030-01-01"))],
        };
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert!(report.findings.is_empty());
        assert_eq!(report.acknowledged_findings.len(), 1);
    }

    #[test]
    fn apply_to_report_no_expires_at_permanent() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, None)],
        };
        let config = Config::default();
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert_eq!(report.acknowledged_findings.len(), 1);
    }

    #[test]
    fn apply_to_report_reevaluates_quality_gate() {
        // 1 critical N+1 SQL finding, default config has
        // n_plus_one_sql_critical_max = 0, so the gate fails before the
        // ack and must pass after.
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let config = Config::default();
        let pre_gate = quality_gate::evaluate(
            &findings,
            &GreenSummary::disabled(0),
            &config.thresholds,
            None,
        );
        assert!(!pre_gate.passed, "baseline gate must fail before ack");

        let mut report = empty_report(findings);
        report.quality_gate = pre_gate;
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, None)],
        };
        apply_to_report(
            &mut report,
            &acks,
            &config,
            now_2026_05_02(),
            ReportOrigin::FreshAnalysis,
        );
        assert!(
            report.quality_gate.passed,
            "gate must flip green after the offending finding is acked"
        );
    }

    #[test]
    fn enrich_with_signatures_overwrites() {
        let mut findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            make_finding(FindingType::RedundantSql, Severity::Warning),
        ];
        // Simulate stale signatures (e.g. computed under an older scheme).
        findings[0].signature = "stale".to_string();
        findings[1].signature = "also-stale".to_string();
        enrich_with_signatures(&mut findings);
        assert_ne!(findings[0].signature, "stale");
        assert_ne!(findings[1].signature, "also-stale");
        assert!(!findings[0].signature.is_empty());
        assert!(!findings[1].signature.is_empty());
    }
}
