//! `perf-sentinel verify-hash` subcommand.
//!
//! Chains up to three checks against a periodic disclosure report:
//! deterministic content hash recompute (pure Rust, always run),
//! Sigstore cosign attestation (delegated to the `cosign` binary), and
//! SLSA build provenance for the producing binary (verified via the
//! `gh attestation verify` GitHub CLI command). Exit codes:
//!
//! - `0` `TRUSTED` (content hash matched AND signature verified ok)
//! - `1` `UNTRUSTED` (at least one check returned a hard failure: hash
//!   mismatch, signature invalid, attestation invalid, identity
//!   mismatch)
//! - `2` `PARTIAL` (no hard failure but at least one check could not
//!   complete: cosign absent, `gh` CLI absent, signature metadata
//!   absent, sidecars missing). A scripted gate
//!   `verify-hash && deploy` still blocks on 2, but distinguishing
//!   `PARTIAL` from `UNTRUSTED` lets the operator tell a tamper
//!   attempt from a missing tool.
//! - `3` `INPUT_ERROR` (report file unreadable, JSON invalid, missing
//!   `--report` / `--url`)
//! - `4` `NETWORK_ERROR` (only `--url` mode: fetch of report or sidecar
//!   failed)

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sentinel_core::report::periodic::compute_content_hash;
use sentinel_core::report::periodic::schema::{IntegrityLevel, PeriodicReport, SignatureMetadata};

/// Hard cap on remote payloads pulled by `--url`. 10 MB is well above any
/// realistic report file size and guards against pathological responses.
const MAX_REMOTE_BYTES: usize = 10 * 1024 * 1024;

/// Per-request timeout for `--url` fetches.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);

/// Exit code: every verification check returned Ok.
pub const EXIT_TRUSTED: i32 = 0;
/// Exit code: at least one check returned a hard failure (hash mismatch,
/// signature invalid, attestation invalid, identity mismatch).
pub const EXIT_UNTRUSTED: i32 = 1;
/// Exit code: no hard failure but at least one check could not complete
/// (cosign absent, `gh` CLI absent, signature metadata absent,
/// sidecars missing).
pub const EXIT_PARTIAL: i32 = 2;
/// Exit code: report file unreadable, JSON invalid, or required flag
/// missing.
pub const EXIT_INPUT_ERROR: i32 = 3;
/// Exit code: `--url` fetch failed (HTTP error, scheme rejected, body
/// over the size cap).
pub const EXIT_NETWORK_ERROR: i32 = 4;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum VerifyHashFormat {
    Text,
    Json,
}

enum Status {
    Ok(String),
    Fail(String),
    Skip(String),
    NotProvided,
}

impl Status {
    fn is_failure(&self) -> bool {
        matches!(self, Status::Fail(_))
    }
}

struct Outcome {
    report_path: String,
    report: PeriodicReport,
    content_hash: Status,
    core_patterns: Status,
    signature: Status,
    binary_attestation: Status,
}

/// Signer identity expectation, fed by the CLI flags
/// `--expected-identity`, `--expected-issuer`, `--no-identity-check`.
/// A consumer who does not assert one of these cannot tell a legitimate
/// signature from a forged one (a Sigstore bundle without identity
/// constraint can be issued by any GitHub or Google account holder).
#[derive(Default, Debug, Clone)]
pub struct IdentityOptions {
    pub expected_identity: Option<String>,
    pub expected_issuer: Option<String>,
    pub no_identity_check: bool,
}

/// Entry point invoked from `main.rs` dispatch.
pub fn cmd_verify_hash(
    report_path: Option<&Path>,
    url: Option<&str>,
    attestation_path: Option<&Path>,
    bundle_path: Option<&Path>,
    format: VerifyHashFormat,
    identity: &IdentityOptions,
) -> i32 {
    let (report, display_path, fetched_paths) = match load_report(report_path, url) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let content_hash = verify_content_hash(&report);
    let core_patterns = verify_core_patterns(&report);
    let signature = verify_signature(
        &report,
        fetched_paths.attestation.as_deref().or(attestation_path),
        fetched_paths.bundle.as_deref().or(bundle_path),
        identity,
    );
    let binary_attestation = verify_binary_attestation(&report);

    let outcome = Outcome {
        report_path: display_path,
        report,
        content_hash,
        core_patterns,
        signature,
        binary_attestation,
    };

    match format {
        VerifyHashFormat::Text => print_text(&outcome),
        VerifyHashFormat::Json => print_json(&outcome),
    }

    exit_code(&outcome)
}

struct FetchedPaths {
    attestation: Option<PathBuf>,
    bundle: Option<PathBuf>,
}

fn load_report(
    report_path: Option<&Path>,
    url: Option<&str>,
) -> Result<(PeriodicReport, String, FetchedPaths), i32> {
    if let Some(path) = report_path {
        let bytes = std::fs::read(path).map_err(|e| {
            eprintln!("Error: read {}: {e}", path.display());
            EXIT_INPUT_ERROR
        })?;
        let report = parse_report(&bytes).map_err(|e| {
            eprintln!("Error: parse {}: {e}", path.display());
            EXIT_INPUT_ERROR
        })?;
        let display = path.display().to_string();
        let fetched = FetchedPaths {
            attestation: None,
            bundle: None,
        };
        return Ok((report, display, fetched));
    }
    if let Some(url) = url {
        return fetch_from_url(url);
    }
    eprintln!("Error: one of --report or --url is required");
    Err(EXIT_INPUT_ERROR)
}

fn parse_report(bytes: &[u8]) -> Result<PeriodicReport, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn fetch_from_url(url: &str) -> Result<(PeriodicReport, String, FetchedPaths), i32> {
    let report = http_get(url)
        .map_err(|e| {
            eprintln!("Error: fetch {url}: {e}");
            EXIT_NETWORK_ERROR
        })
        .and_then(|bytes| {
            parse_report(&bytes).map_err(|e| {
                eprintln!("Error: parse {url}: {e}");
                EXIT_INPUT_ERROR
            })
        })?;
    let attestation_url = derive_sidecar_url(url, "attestation.intoto.jsonl");
    let bundle_url = derive_sidecar_url(url, "bundle.sig");
    let mut fetched = FetchedPaths {
        attestation: None,
        bundle: None,
    };
    let pid = std::process::id();
    if let Some(a_url) = attestation_url {
        match http_get(&a_url) {
            Ok(data) => {
                let path = std::env::temp_dir().join(format!(
                    "perf-sentinel-verify-attestation-{pid}.intoto.jsonl"
                ));
                if write_temp_no_follow(&path, &data).is_ok() {
                    fetched.attestation = Some(path);
                }
            }
            Err(e) => eprintln!("Note: could not fetch attestation sidecar: {e}"),
        }
    }
    if let Some(b_url) = bundle_url {
        match http_get(&b_url) {
            Ok(data) => {
                let path =
                    std::env::temp_dir().join(format!("perf-sentinel-verify-bundle-{pid}.sig"));
                if write_temp_no_follow(&path, &data).is_ok() {
                    fetched.bundle = Some(path);
                }
            }
            Err(e) => eprintln!("Note: could not fetch signature bundle: {e}"),
        }
    }
    Ok((report, url.to_string(), fetched))
}

/// Write `data` to `path` while refusing to follow a pre-existing
/// symlink (defence in depth on shared `/tmp` setups). Uses
/// `O_CREAT|O_EXCL|O_WRONLY` on Unix-likes via `create_new(true)`;
/// on Windows there is no real symlink-attack surface for the
/// system temp dir, so a plain `fs::write` is acceptable.
fn write_temp_no_follow(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    // Best-effort: tolerate a residual file from a previous run with
    // the same pid wrap-around by unlinking, then create_new to bind
    // the inode atomically.
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    f.write_all(data)
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err("only https:// URLs are accepted".to_string());
    }
    // max_redirects(0): refuse cross-host redirects entirely. The
    // scheme guard above only covers the initial request; redirects
    // could rebind to http://internal or https://localhost:4317 and
    // turn this fetch into an SSRF probe. Operators who need
    // redirect-following should re-resolve the canonical URL first.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REMOTE_TIMEOUT))
        .max_redirects(0)
        .build()
        .into();
    let response = agent.get(url).call().map_err(|e| format!("http: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("http status {}", response.status().as_u16()));
    }
    let mut response = response;
    let mut reader = response.body_mut().as_reader();
    let mut out = Vec::with_capacity(64 * 1024);
    reader
        .by_ref()
        .take((MAX_REMOTE_BYTES + 1) as u64)
        .read_to_end(&mut out)
        .map_err(|e| format!("read body: {e}"))?;
    if out.len() > MAX_REMOTE_BYTES {
        return Err(format!(
            "response exceeds {MAX_REMOTE_BYTES} byte cap, refusing to load"
        ));
    }
    Ok(out)
}

/// Conventional sidecar URL: same directory as the report, fixed
/// tail. Matches the operator workflow in `docs/REPORTING.md` which
/// instructs publishing `attestation.intoto.jsonl` and `bundle.sig`
/// alongside the report regardless of the report's exact filename.
fn derive_sidecar_url(report_url: &str, tail: &str) -> Option<String> {
    let (prefix, _last) = report_url.rsplit_once('/')?;
    Some(format!("{prefix}/{tail}"))
}

fn verify_content_hash(report: &PeriodicReport) -> Status {
    let claimed = report.integrity.content_hash.clone();
    if claimed.is_empty() {
        return Status::Fail("integrity.content_hash is empty".to_string());
    }
    match compute_content_hash(report) {
        Ok(recomputed) if recomputed == claimed => {
            Status::Ok(format!("matches integrity.content_hash ({claimed})"))
        }
        Ok(recomputed) => Status::Fail(format!(
            "mismatch: recomputed {recomputed} vs claimed {claimed}"
        )),
        Err(e) => Status::Fail(format!("recompute failed: {e}")),
    }
}

/// Cross-check `methodology.core_patterns_required` against the
/// canonical set baked into the verifying binary. Detects core-pattern
/// substitution that the validator would have rejected at signing
/// time only if the signer was honest. A divergence means either the
/// report was tampered with (substitution) or the verifying binary
/// is a different perf-sentinel version with a different canonical
/// core (rare, surfaced as a hint).
fn verify_core_patterns(report: &PeriodicReport) -> Status {
    use sentinel_core::report::periodic::hash_core_patterns;
    use sentinel_core::report::periodic::schema::core_patterns_required as canonical_set;

    let declared = &report.methodology.core_patterns_required;
    let local_canonical = canonical_set();
    let declared_hash = hash_core_patterns(declared);
    let canonical_hash = hash_core_patterns(&local_canonical);

    if declared_hash == canonical_hash {
        return Status::Ok(format!(
            "matches canonical core set for local perf-sentinel ({} patterns)",
            local_canonical.len()
        ));
    }

    fn set_difference<'a>(a: &'a [String], b: &[String]) -> Vec<&'a str> {
        a.iter()
            .filter(|x| !b.iter().any(|y| y == *x))
            .map(String::as_str)
            .collect()
    }
    let only_in_declared = set_difference(declared, &local_canonical);
    let only_in_canonical = set_difference(&local_canonical, declared);
    // Cite the report-declared producing binary so an auditor on a
    // mismatched version knows exactly which perf-sentinel to download
    // and re-run against. Falls back to perf_sentinel_version when
    // binary_version is empty on pre-0.7.0 reports.
    let producing_version = if report.report_metadata.binary_version.is_empty() {
        report.report_metadata.perf_sentinel_version.as_str()
    } else {
        report.report_metadata.binary_version.as_str()
    };
    Status::Fail(format!(
        "core_patterns_required diverges from local canonical set: \
         declared-only={only_in_declared:?}, canonical-only={only_in_canonical:?}. \
         Possible substitution at sign time, or verifying binary is a different version. \
         Report was produced by perf-sentinel {producing_version}, consider re-running \
         verify-hash with that exact version before flagging the report as untrusted."
    ))
}

/// What identity constraint cosign should enforce. Constrain enforces
/// the operator-supplied identity (the safe default for a third-party
/// auditor). Skip runs cosign without identity flags (cryptographic
/// integrity only, signer not verified).
enum IdentityCheck {
    Constrain { identity: String, issuer: String },
    Skip,
}

fn verify_signature(
    report: &PeriodicReport,
    attestation_path: Option<&Path>,
    bundle_path: Option<&Path>,
    identity: &IdentityOptions,
) -> Status {
    let Some(sig) = report.integrity.signature.as_ref() else {
        return Status::NotProvided;
    };
    let check = match resolve_identity_check(identity) {
        Ok(c) => c,
        Err(s) => return s,
    };
    let Some(att_path) = attestation_path else {
        return Status::Skip(
            "signature metadata present, pass --attestation <path> to verify".to_string(),
        );
    };
    let Some(b_path) = bundle_path else {
        return Status::Skip(
            "signature metadata present, pass --bundle <path> to verify".to_string(),
        );
    };
    if let Some(s) = reject_unsafe_cosign_flag("signer_identity", &sig.signer_identity) {
        return s;
    }
    if let Some(s) = reject_unsafe_cosign_flag("signer_issuer", &sig.signer_issuer) {
        return s;
    }
    if !command_exists("cosign") {
        return Status::Skip(
            "cosign not found in PATH (install from https://docs.sigstore.dev/system_config/installation)".to_string(),
        );
    }
    run_cosign_verify(sig, att_path, b_path, &check)
}

/// Translate user-facing flags into an identity-check decision. The
/// "no flag at all" branch returns Fail with an explicit message so a
/// scripted gate does not silently treat an unverified signer as
/// trusted. Both flags must be present together: a partial pair is a
/// configuration error rather than a security failure, but it still
/// blocks verification because cosign cannot run with only one of the
/// pair.
fn resolve_identity_check(identity: &IdentityOptions) -> Result<IdentityCheck, Status> {
    if identity.no_identity_check {
        return Ok(IdentityCheck::Skip);
    }
    match (&identity.expected_identity, &identity.expected_issuer) {
        (Some(id), Some(issuer)) => {
            if let Some(s) = reject_unsafe_cosign_flag("--expected-identity", id) {
                return Err(s);
            }
            if let Some(s) = reject_unsafe_cosign_flag("--expected-issuer", issuer) {
                return Err(s);
            }
            Ok(IdentityCheck::Constrain {
                identity: id.clone(),
                issuer: issuer.clone(),
            })
        }
        (Some(_), None) | (None, Some(_)) => Err(Status::Fail(
            "both --expected-identity and --expected-issuer must be passed together".to_string(),
        )),
        (None, None) => Err(Status::Fail(
            "cannot verify without expected identity. Pass --expected-identity \
             and --expected-issuer to constrain the signer, or --no-identity-check \
             to verify cryptographic integrity only. A Sigstore bundle without an \
             identity constraint can be forged by any GitHub or Google account holder."
                .to_string(),
        )),
    }
}

fn run_cosign_verify(
    sig: &SignatureMetadata,
    attestation_path: &Path,
    bundle_path: &Path,
    check: &IdentityCheck,
) -> Status {
    // The disclose pipeline emits a complete in-toto v1 Statement
    // (see attestation.rs). Operators sign it with `cosign sign-blob
    // <statement> --bundle <bundle.sig> --new-bundle-format`, so the
    // bundle pins the statement file itself, not the report. The
    // matching verify command is `cosign verify-blob`, with the
    // statement as the positional. Using `verify-blob-attestation`
    // here, or `attest-blob --predicate <statement>` on the sign
    // side, would wrap the statement in another statement and
    // produce a permanent double-wrapped entry in Rekor.
    let mut cmd = Command::new("cosign");
    cmd.arg("verify-blob")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--new-bundle-format");
    if let IdentityCheck::Constrain { identity, issuer } = check {
        cmd.arg("--certificate-identity")
            .arg(identity)
            .arg("--certificate-oidc-issuer")
            .arg(issuer);
    }
    cmd.arg(attestation_path);
    let output = cmd.output();
    match output {
        Ok(out) if out.status.success() => match check {
            IdentityCheck::Constrain { identity, issuer } => Status::Ok(format!(
                "valid (signed by {} via {})",
                sanitise_for_terminal(identity),
                sanitise_for_terminal(issuer)
            )),
            IdentityCheck::Skip => Status::Skip(format!(
                "identity check skipped via --no-identity-check, cryptographic integrity OK \
                 but signer ({} via {}) not verified",
                sanitise_for_terminal(&sig.signer_identity),
                sanitise_for_terminal(&sig.signer_issuer)
            )),
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Status::Fail(format!(
                "cosign rejected: {}",
                stderr.lines().last().unwrap_or("(no stderr)")
            ))
        }
        Err(e) => Status::Fail(format!("cosign spawn failed: {e}")),
    }
}

fn verify_binary_attestation(report: &PeriodicReport) -> Status {
    match report.integrity.binary_attestation.as_ref() {
        None => Status::NotProvided,
        Some(att) => Status::Skip(format!(
            "binary attestation metadata present (built from {} at {}, builder {}, attestation indexed at {}). Verify the binary with `gh attestation verify <binary> --owner robintra --repo perf-sentinel`.",
            sanitise_for_terminal(&att.git_tag),
            sanitise_for_terminal(&att.git_commit),
            sanitise_for_terminal(&att.builder_id),
            sanitise_for_terminal(&att.attestation_url),
        )),
    }
}

fn command_exists(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| {
        if d.join(name).is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat", "com"] {
                if d.join(format!("{name}.{ext}")).is_file() {
                    return true;
                }
            }
        }
        false
    })
}

/// Reject values that would let an adversarial report pivot cosign's
/// CLI flags (anything starting with `-`) or smuggle control chars
/// past terminal sanitisation. `Command::arg()` does not invoke a
/// shell so quoting / `;` / `$(...)` are inert, but flag values are
/// still parsed by cosign itself.
#[must_use]
fn is_safe_cosign_argument(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.chars().any(|c| c.is_control() || c == '\0')
}

/// Wraps `is_safe_cosign_argument` with the canonical rejection
/// message, returning `None` when the value is safe and `Some(Fail)`
/// otherwise. Centralises the message so signer fields and operator
/// flags surface the same wording.
fn reject_unsafe_cosign_flag(label: &str, value: &str) -> Option<Status> {
    if is_safe_cosign_argument(value) {
        None
    } else {
        Some(Status::Fail(format!(
            "{label} rejected: starts with '-' or contains control chars"
        )))
    }
}

fn sanitise_for_terminal(s: &str) -> String {
    sentinel_core::text_safety::sanitize_for_terminal(s).into_owned()
}

fn exit_code(outcome: &Outcome) -> i32 {
    match overall_label(outcome) {
        "TRUSTED" => EXIT_TRUSTED,
        "UNTRUSTED" => EXIT_UNTRUSTED,
        _ => EXIT_PARTIAL,
    }
}

fn print_text(outcome: &Outcome) {
    println!("perf-sentinel verify-hash {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Report: {}", outcome.report_path);
    println!(
        "  Period: {} to {}",
        outcome.report.period.from_date, outcome.report.period.to_date
    );
    println!(
        "  Organisation: {} ({})",
        sanitise_for_terminal(&outcome.report.organisation.name),
        sanitise_for_terminal(&outcome.report.organisation.country)
    );
    println!(
        "  Intent: {}, integrity_level: {}",
        intent_label(outcome.report.report_metadata.intent),
        integrity_level_label(outcome.report.report_metadata.integrity_level)
    );
    println!();
    println!("Verifications:");
    println!("  {}", format_status("Content hash", &outcome.content_hash));
    println!(
        "  {}",
        format_status("Core patterns", &outcome.core_patterns)
    );
    println!("  {}", format_status("Signature", &outcome.signature));
    println!(
        "  {}",
        format_status("Binary attestation", &outcome.binary_attestation)
    );
    println!();
    println!("Overall: {}", overall_label(outcome));
}

fn format_status(label: &str, s: &Status) -> String {
    match s {
        Status::Ok(detail) => format!("[OK] {label}: {detail}"),
        Status::Fail(detail) => format!("[FAIL] {label}: {detail}"),
        Status::Skip(detail) => format!("[SKIP] {label}: {detail}"),
        Status::NotProvided => format!("[--] {label}: not provided"),
    }
}

fn print_json(outcome: &Outcome) {
    let body = serde_json::json!({
        "report_path": outcome.report_path,
        "report_metadata": {
            "intent": intent_label(outcome.report.report_metadata.intent),
            "integrity_level": integrity_level_label(outcome.report.report_metadata.integrity_level),
            "perf_sentinel_version": outcome.report.report_metadata.perf_sentinel_version,
            "report_uuid": outcome.report.report_metadata.report_uuid,
        },
        "period": {
            "from_date": outcome.report.period.from_date.to_string(),
            "to_date": outcome.report.period.to_date.to_string(),
        },
        "verifications": {
            "content_hash": status_to_json(&outcome.content_hash),
            "core_patterns": status_to_json(&outcome.core_patterns),
            "signature": status_to_json(&outcome.signature),
            "binary_attestation": status_to_json(&outcome.binary_attestation),
        },
        "overall": overall_label(outcome),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    );
}

fn status_to_json(s: &Status) -> serde_json::Value {
    match s {
        Status::Ok(d) => serde_json::json!({"status": "ok", "detail": d}),
        Status::Fail(d) => serde_json::json!({"status": "fail", "detail": d}),
        Status::Skip(d) => serde_json::json!({"status": "skip", "detail": d}),
        Status::NotProvided => serde_json::json!({"status": "not_provided"}),
    }
}

fn overall_label(outcome: &Outcome) -> &'static str {
    if outcome.content_hash.is_failure()
        || outcome.core_patterns.is_failure()
        || outcome.signature.is_failure()
        || outcome.binary_attestation.is_failure()
    {
        "UNTRUSTED"
    } else if matches!(outcome.content_hash, Status::Ok(_))
        && matches!(outcome.signature, Status::Ok(_))
    {
        "TRUSTED"
    } else {
        "PARTIAL"
    }
}

fn intent_label(intent: sentinel_core::report::periodic::schema::ReportIntent) -> &'static str {
    use sentinel_core::report::periodic::schema::ReportIntent;
    match intent {
        ReportIntent::Internal => "internal",
        ReportIntent::Official => "official",
        ReportIntent::Audited => "audited",
    }
}

fn integrity_level_label(level: IntegrityLevel) -> &'static str {
    match level {
        IntegrityLevel::None => "none",
        IntegrityLevel::HashOnly => "hash-only",
        IntegrityLevel::Signed => "signed",
        IntegrityLevel::SignedWithAttestation => "signed-with-attestation",
        IntegrityLevel::Audited => "audited",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::report::periodic::schema::BinaryAttestationMetadata;
    use std::path::PathBuf;

    fn example_g2() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("docs/schemas/examples/example-official-public-G2.json")
    }

    #[test]
    fn content_hash_check_passes_on_valid_recompute() {
        let bytes = std::fs::read(example_g2()).unwrap();
        let mut report: PeriodicReport = serde_json::from_slice(&bytes).unwrap();
        let hash = compute_content_hash(&report).unwrap();
        report.integrity.content_hash = hash;
        let s = verify_content_hash(&report);
        assert!(matches!(s, Status::Ok(_)), "expected OK, got display");
    }

    #[test]
    fn content_hash_check_fails_on_mismatch() {
        let report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        // Example file ships with a zeroed placeholder hash that does not
        // match the recomputed canonical value.
        let s = verify_content_hash(&report);
        assert!(matches!(s, Status::Fail(_)));
    }

    #[test]
    fn core_patterns_check_passes_on_canonical_set() {
        let report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        let s = verify_core_patterns(&report);
        assert!(
            matches!(s, Status::Ok(_)),
            "G2 ships the canonical core set"
        );
    }

    #[test]
    fn core_patterns_check_fails_on_substitution() {
        let mut report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        // Substitute one of the four canonical core patterns with a
        // non-canonical one. Counts stay at 4 but the canonical hash
        // differs, which is exactly the audit case the cross-check
        // exists to catch.
        let to_replace = report.methodology.core_patterns_required[0].clone();
        for slot in &mut report.methodology.core_patterns_required {
            if *slot == to_replace {
                *slot = "slow_sql".to_string();
                break;
            }
        }
        let s = verify_core_patterns(&report);
        match s {
            Status::Fail(detail) => {
                assert!(detail.contains("slow_sql"), "{detail}");
                assert!(detail.contains(&to_replace), "{detail}");
            }
            _ => panic!("expected Fail"),
        }
    }

    #[test]
    fn core_patterns_check_fails_on_shrinkage() {
        let mut report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        let dropped = report.methodology.core_patterns_required.pop().unwrap();
        let s = verify_core_patterns(&report);
        match s {
            Status::Fail(detail) => assert!(detail.contains(&dropped), "{detail}"),
            _ => panic!("expected Fail on shrinkage"),
        }
    }

    #[test]
    fn core_patterns_check_fails_on_growth() {
        let mut report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        report
            .methodology
            .core_patterns_required
            .push("slow_sql".to_string());
        let s = verify_core_patterns(&report);
        match s {
            Status::Fail(detail) => assert!(detail.contains("slow_sql"), "{detail}"),
            _ => panic!("expected Fail on growth"),
        }
    }

    #[test]
    fn signature_check_returns_not_provided_when_absent() {
        let report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        let s = verify_signature(&report, None, None, &IdentityOptions::default());
        assert!(matches!(s, Status::NotProvided));
    }

    fn report_with_signature() -> PeriodicReport {
        let mut report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        report.integrity.signature = Some(SignatureMetadata {
            format: "sigstore-cosign-intoto-v1".to_string(),
            bundle_url: "https://example.fr/x.sig".to_string(),
            signer_identity: "user@example.fr".to_string(),
            signer_issuer: "https://accounts.google.com".to_string(),
            rekor_url: "https://rekor.sigstore.dev".to_string(),
            rekor_log_index: 1,
            signed_at: "2026-01-01T00:00:00Z".to_string(),
        });
        report
    }

    #[test]
    fn signature_check_fails_without_identity_flags() {
        // Closes the autosigning hole: a third-party auditor running
        // verify-hash without --expected-identity (the default) must
        // not see TRUSTED on a bundle whose identity comes only from
        // the report itself.
        let report = report_with_signature();
        let s = verify_signature(&report, None, None, &IdentityOptions::default());
        match s {
            Status::Fail(detail) => assert!(detail.contains("expected identity"), "{detail}"),
            _ => panic!("expected Fail without identity flags, got display"),
        }
    }

    #[test]
    fn signature_check_fails_when_only_one_identity_flag_set() {
        let report = report_with_signature();
        let s = verify_signature(
            &report,
            None,
            None,
            &IdentityOptions {
                expected_identity: Some("user@example.fr".to_string()),
                expected_issuer: None,
                no_identity_check: false,
            },
        );
        match s {
            Status::Fail(detail) => {
                assert!(detail.contains("must be passed together"), "{detail}");
            }
            _ => panic!("expected Fail on half-pair, got display"),
        }
    }

    #[test]
    fn signature_check_skips_with_no_identity_check_and_paths_absent() {
        // Operator opts out explicitly. The check still reaches the
        // paths-absent branch and returns Skip on that, which is the
        // legacy behaviour (cryptographic integrity only, signer not
        // verified).
        let report = report_with_signature();
        let s = verify_signature(
            &report,
            None,
            None,
            &IdentityOptions {
                expected_identity: None,
                expected_issuer: None,
                no_identity_check: true,
            },
        );
        match s {
            Status::Skip(detail) => assert!(detail.contains("pass --attestation"), "{detail}"),
            _ => panic!("expected Skip on missing paths under --no-identity-check"),
        }
    }

    #[test]
    fn signature_check_rejects_dash_prefix_in_expected_identity() {
        let report = report_with_signature();
        let s = verify_signature(
            &report,
            None,
            None,
            &IdentityOptions {
                expected_identity: Some("--certificate-github-workflow-ref=evil".to_string()),
                expected_issuer: Some("https://attacker".to_string()),
                no_identity_check: false,
            },
        );
        match s {
            Status::Fail(detail) => {
                assert!(detail.contains("expected-identity rejected"), "{detail}");
            }
            _ => panic!("expected Fail on dash-prefixed identity, got display"),
        }
    }

    #[test]
    fn binary_attestation_skipped_with_metadata_hint() {
        let mut report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        report.integrity.binary_attestation = Some(BinaryAttestationMetadata {
            format: "slsa-provenance-v1".to_string(),
            attestation_url: "https://gh/p.intoto.jsonl".to_string(),
            builder_id: "https://github.com/actions/runner".to_string(),
            git_tag: "v0.7.0".to_string(),
            git_commit: "a47be9d".to_string(),
            slsa_level: "L3".to_string(),
        });
        let s = verify_binary_attestation(&report);
        match s {
            Status::Skip(d) => assert!(d.contains("gh attestation")),
            other => panic!(
                "expected Skip, got {}",
                match other {
                    Status::Ok(_) => "Ok",
                    Status::Fail(_) => "Fail",
                    Status::NotProvided => "NotProvided",
                    Status::Skip(_) => unreachable!(),
                }
            ),
        }
    }

    #[test]
    fn derive_sidecar_url_uses_same_directory() {
        assert_eq!(
            derive_sidecar_url(
                "https://example.fr/perf-sentinel-report.json",
                "attestation.intoto.jsonl"
            ),
            Some("https://example.fr/attestation.intoto.jsonl".to_string())
        );
        assert_eq!(
            derive_sidecar_url("https://example.fr/report.json", "bundle.sig"),
            Some("https://example.fr/bundle.sig".to_string())
        );
    }

    #[test]
    fn derive_sidecar_url_returns_none_when_url_has_no_slash() {
        assert!(derive_sidecar_url("report.json", "bundle.sig").is_none());
    }

    #[test]
    fn cosign_argument_rejects_flag_injection() {
        assert!(!is_safe_cosign_argument(
            "--certificate-github-workflow-ref=evil"
        ));
        assert!(!is_safe_cosign_argument("-x"));
        assert!(!is_safe_cosign_argument(""));
        assert!(!is_safe_cosign_argument("user@example.fr\nmalicious"));
        assert!(is_safe_cosign_argument("user@example.fr"));
        assert!(is_safe_cosign_argument(
            "https://token.actions.githubusercontent.com"
        ));
    }
}
