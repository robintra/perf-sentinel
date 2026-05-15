//! End-to-end integration tests for the `verify-hash` subcommand.
//!
//! These tests exercise the CLI binary directly so they cover the full
//! parse + dispatch + verification path. Cosign delegation is not
//! mocked end to end (a PATH-override stub is fragile across runners);
//! the content hash branch is exercised in full, the signature and
//! binary attestation branches are exercised via metadata-present /
//! absent permutations.

use sentinel_core::report::periodic::compute_content_hash;
use sentinel_core::report::periodic::schema::PeriodicReport;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn workspace_doc(rel: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("..").join("..").join(rel)
}

fn run_verify(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("verify-hash")
        .args(args)
        .output()
        .expect("spawn perf-sentinel verify-hash")
}

#[test]
fn verify_hash_on_placeholder_example_fails_content_check() {
    // The shipped G2 example carries a zeroed content_hash placeholder
    // so the recompute will not match. Exit code must be 1
    // (UNTRUSTED), not 0.
    let path = workspace_doc("docs/schemas/examples/example-official-public-G2.json");
    let out = run_verify(&["--report", path.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[FAIL] Content hash"),
        "expected content-hash FAIL: {stdout}"
    );
    assert!(stdout.contains("UNTRUSTED"), "expected UNTRUSTED: {stdout}");
}

fn write_example_with_fixed_hash(dir: &std::path::Path) -> PathBuf {
    // Load the example, recompute the canonical hash, write a fresh
    // copy with the real hash baked in. Bypasses the disclose pipeline
    // entirely and exercises just the verify-hash side.
    let example = workspace_doc("docs/schemas/examples/example-official-public-G2.json");
    let mut report: PeriodicReport = serde_json::from_slice(&fs::read(&example).unwrap()).unwrap();
    let hash = compute_content_hash(&report).unwrap();
    report.integrity.content_hash = hash;
    let path = dir.join("report.json");
    fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    path
}

#[test]
fn verify_hash_returns_partial_with_exit_2_when_signature_absent() {
    // A hash-only report (no Sigstore signature) yields PARTIAL.
    // Exit code 2 distinguishes PARTIAL (verification could not
    // complete) from UNTRUSTED (exit 1, a check actively failed).
    // A scripted `verify-hash && deploy` still blocks because exit is
    // non-zero. Content hash matches, signature was never verified.
    let tmp = tempfile::tempdir().expect("tempdir");
    let report_path = write_example_with_fixed_hash(tmp.path());
    let v = run_verify(&["--report", report_path.to_str().unwrap()]);
    assert_eq!(
        v.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&v.stdout),
        String::from_utf8_lossy(&v.stderr)
    );
    let stdout = String::from_utf8_lossy(&v.stdout);
    assert!(stdout.contains("[OK] Content hash"), "{stdout}");
    assert!(stdout.contains("PARTIAL"), "{stdout}");
}

#[test]
fn verify_hash_after_content_tamper_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let report_path = write_example_with_fixed_hash(tmp.path());
    let bytes = fs::read_to_string(&report_path).unwrap();
    let mutated = bytes.replace("\"Example SAS\"", "\"Tampered SAS\"");
    fs::write(&report_path, mutated).unwrap();
    let v = run_verify(&["--report", report_path.to_str().unwrap()]);
    assert_eq!(v.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&v.stdout);
    assert!(stdout.contains("[FAIL] Content hash"), "{stdout}");
}

#[test]
fn verify_hash_missing_report_returns_input_error() {
    // Missing file is exit 3 (INPUT_ERROR), distinct from UNTRUSTED
    // (1) and PARTIAL (2) so a wrapper script can react differently
    // to a wrong path than to a tamper attempt.
    let v = run_verify(&["--report", "/nonexistent/path/to/missing.json"]);
    assert_eq!(v.status.code(), Some(3));
}

#[test]
fn verify_hash_json_format_emits_structured_output() {
    let path = workspace_doc("docs/schemas/examples/example-official-public-G2.json");
    let v = run_verify(&["--report", path.to_str().unwrap(), "--format", "json"]);
    let stdout = String::from_utf8_lossy(&v.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON output");
    assert_eq!(parsed["overall"], "UNTRUSTED");
    assert_eq!(parsed["verifications"]["content_hash"]["status"], "fail");
    assert_eq!(
        parsed["verifications"]["signature"]["status"],
        "not_provided"
    );
}

#[test]
fn verify_hash_url_mode_rejects_http_scheme() {
    // Hardening: only HTTPS is accepted. http:// must fail with a
    // network-class exit code 4 (NETWORK_ERROR), distinct from
    // INPUT_ERROR (3, e.g. file missing) and UNTRUSTED (1).
    let v = run_verify(&["--url", "http://example.fr/report.json"]);
    assert_eq!(v.status.code(), Some(4));
}

#[test]
fn verify_hash_local_report_over_size_cap_returns_input_error() {
    // 64 MiB cap on `--report <local>`. Sparse `set_len` extends without
    // writing the bytes so the test runs fast on every filesystem.
    // Source of truth: `crates/sentinel-cli/src/limits.rs::MAX_LOCAL_REPORT_BYTES`.
    // Kept as a literal here because `perf-sentinel` is a pure binary
    // crate without a `lib.rs` re-export. Bump in lockstep when the cap
    // changes.
    let tmp = tempfile::tempdir().expect("tempdir");
    let huge = tmp.path().join("huge.json");
    let file = fs::File::create(&huge).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let v = run_verify(&["--report", huge.to_str().unwrap()]);
    assert_eq!(v.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&v.stderr);
    assert!(stderr.contains("exceeds"), "stderr: {stderr}");
}
