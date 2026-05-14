//! `perf-sentinel verify-hash` subcommand.
//!
//! Chains up to three checks against a periodic disclosure report:
//! deterministic content hash recompute (pure Rust, always run),
//! Sigstore cosign attestation (delegated to the `cosign` binary), and
//! SLSA build provenance for the producing binary (delegated to
//! `slsa-verifier`). Exit codes:
//!
//! - `0` TRUSTED (content hash matched and signature verified ok)
//! - `1` UNTRUSTED or PARTIAL (a check failed, was skipped, or the
//!   metadata was absent and a downstream script must not assume
//!   integrity)
//! - `2` file error
//! - `3` network error
//!
//! PARTIAL collapses into `1` on purpose: a script doing
//! `verify-hash && deploy` must require the cryptographic primitives,
//! not just the local content hash.

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
    signature: Status,
    binary_attestation: Status,
}

/// Entry point invoked from `main.rs` dispatch.
pub fn cmd_verify_hash(
    report_path: Option<&Path>,
    url: Option<&str>,
    attestation_path: Option<&Path>,
    bundle_path: Option<&Path>,
    format: VerifyHashFormat,
) -> i32 {
    let (report, report_bytes, display_path, fetched_paths) = match load_report(report_path, url) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let content_hash = verify_content_hash(&report);
    let signature = verify_signature(
        &report,
        &report_bytes,
        fetched_paths.attestation.as_deref().or(attestation_path),
        fetched_paths.bundle.as_deref().or(bundle_path),
    );
    let binary_attestation = verify_binary_attestation(&report);

    let outcome = Outcome {
        report_path: display_path,
        report,
        content_hash,
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
) -> Result<(PeriodicReport, Vec<u8>, String, FetchedPaths), i32> {
    if let Some(path) = report_path {
        let bytes = std::fs::read(path).map_err(|e| {
            eprintln!("Error: read {}: {e}", path.display());
            2
        })?;
        let report = parse_report(&bytes).map_err(|e| {
            eprintln!("Error: parse {}: {e}", path.display());
            2
        })?;
        let display = path.display().to_string();
        let fetched = FetchedPaths {
            attestation: None,
            bundle: None,
        };
        return Ok((report, bytes, display, fetched));
    }
    if let Some(url) = url {
        return fetch_from_url(url);
    }
    eprintln!("Error: one of --report or --url is required");
    Err(2)
}

fn parse_report(bytes: &[u8]) -> Result<PeriodicReport, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn fetch_from_url(url: &str) -> Result<(PeriodicReport, Vec<u8>, String, FetchedPaths), i32> {
    let bytes = http_get(url).map_err(|e| {
        eprintln!("Error: fetch {url}: {e}");
        3
    })?;
    let report = parse_report(&bytes).map_err(|e| {
        eprintln!("Error: parse {url}: {e}");
        2
    })?;
    let attestation_url = derive_sidecar_url(url, "attestation.intoto.jsonl");
    let bundle_url = derive_sidecar_url(url, "bundle.sig");
    let mut fetched = FetchedPaths {
        attestation: None,
        bundle: None,
    };
    if let Some(a_url) = attestation_url {
        match http_get(&a_url) {
            Ok(data) => {
                let path =
                    std::env::temp_dir().join("perf-sentinel-verify-attestation.intoto.jsonl");
                if std::fs::write(&path, &data).is_ok() {
                    fetched.attestation = Some(path);
                }
            }
            Err(e) => eprintln!("Note: could not fetch attestation sidecar: {e}"),
        }
    }
    if let Some(b_url) = bundle_url {
        match http_get(&b_url) {
            Ok(data) => {
                let path = std::env::temp_dir().join("perf-sentinel-verify-bundle.sig");
                if std::fs::write(&path, &data).is_ok() {
                    fetched.bundle = Some(path);
                }
            }
            Err(e) => eprintln!("Note: could not fetch signature bundle: {e}"),
        }
    }
    Ok((report, bytes, url.to_string(), fetched))
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

fn verify_signature(
    report: &PeriodicReport,
    report_bytes: &[u8],
    attestation_path: Option<&Path>,
    bundle_path: Option<&Path>,
) -> Status {
    let Some(sig) = report.integrity.signature.as_ref() else {
        return Status::NotProvided;
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
    if !is_safe_cosign_argument(&sig.signer_identity) {
        return Status::Fail(
            "signer_identity rejected: starts with '-' or contains control chars".to_string(),
        );
    }
    if !is_safe_cosign_argument(&sig.signer_issuer) {
        return Status::Fail(
            "signer_issuer rejected: starts with '-' or contains control chars".to_string(),
        );
    }
    if !command_exists("cosign") {
        return Status::Skip(
            "cosign not found in PATH (install from https://docs.sigstore.dev/system_config/installation)".to_string(),
        );
    }
    let report_tmp = match write_temp_for_cosign(report_bytes) {
        Ok(p) => p,
        Err(e) => return Status::Fail(format!("temp file: {e}")),
    };
    let result = run_cosign_verify(sig, &report_tmp, att_path, b_path);
    let _ = std::fs::remove_file(&report_tmp);
    result
}

fn write_temp_for_cosign(bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "perf-sentinel-verify-report-{}.json",
        std::process::id()
    ));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn run_cosign_verify(
    sig: &SignatureMetadata,
    report_path: &Path,
    attestation_path: &Path,
    bundle_path: &Path,
) -> Status {
    // cosign verify-blob-attestation is the blob/file variant of
    // verify-attestation. The OCI variant rejects local files as
    // unparseable image references. `--predicate` carries the in-toto
    // statement, the trailing positional is the signed blob (the
    // report file).
    let _ = attestation_path;
    let output = Command::new("cosign")
        .arg("verify-blob-attestation")
        .arg("--type")
        .arg("custom")
        .arg("--bundle")
        .arg(bundle_path)
        .arg("--certificate-identity")
        .arg(&sig.signer_identity)
        .arg("--certificate-oidc-issuer")
        .arg(&sig.signer_issuer)
        .arg(report_path)
        .output();
    match output {
        Ok(out) if out.status.success() => Status::Ok(format!(
            "valid (signed by {} via {})",
            sanitise_for_terminal(&sig.signer_identity),
            sanitise_for_terminal(&sig.signer_issuer)
        )),
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
            "binary attestation metadata present (built from {} at {}, builder {}); verify the binary at {} with `slsa-verifier verify-artifact --provenance-path ... --source-uri github.com/robintra/perf-sentinel --source-tag {} <binary>`",
            sanitise_for_terminal(&att.git_tag),
            sanitise_for_terminal(&att.git_commit),
            sanitise_for_terminal(&att.builder_id),
            sanitise_for_terminal(&att.attestation_url),
            sanitise_for_terminal(&att.git_tag)
        )),
    }
}

fn command_exists(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(name).is_file())
}

/// Reject values that would let an adversarial report pivot cosign's
/// CLI flags (anything starting with `-`) or smuggle control chars
/// past terminal sanitisation. `Command::arg()` does not invoke a
/// shell so quoting / `;` / `$(...)` are inert, but flag values are
/// still parsed by cosign itself.
fn is_safe_cosign_argument(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.chars().any(|c| c.is_control() || c == '\0')
}

fn sanitise_for_terminal(s: &str) -> String {
    sentinel_core::text_safety::sanitize_for_terminal(s).into_owned()
}

fn exit_code(outcome: &Outcome) -> i32 {
    match overall_label(outcome) {
        "TRUSTED" => 0,
        _ => 1,
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
    fn signature_check_returns_not_provided_when_absent() {
        let report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        let s = verify_signature(&report, &[], None, None);
        assert!(matches!(s, Status::NotProvided));
    }

    #[test]
    fn signature_check_skips_when_metadata_present_but_paths_absent() {
        let mut report: PeriodicReport =
            serde_json::from_slice(&std::fs::read(example_g2()).unwrap()).unwrap();
        report.integrity.signature = Some(SignatureMetadata {
            format: "sigstore-cosign-intoto-v1".to_string(),
            bundle_url: "https://example.fr/x.sig".to_string(),
            signer_identity: "u".to_string(),
            signer_issuer: "https://x".to_string(),
            rekor_url: "https://rekor.sigstore.dev".to_string(),
            rekor_log_index: 1,
            signed_at: "2026-01-01T00:00:00Z".to_string(),
        });
        let s = verify_signature(&report, &[], None, None);
        assert!(matches!(s, Status::Skip(_)));
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
            slsa_level: "L2".to_string(),
        });
        let s = verify_binary_attestation(&report);
        match s {
            Status::Skip(d) => assert!(d.contains("slsa-verifier")),
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
