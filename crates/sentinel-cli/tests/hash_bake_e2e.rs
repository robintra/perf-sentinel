//! End-to-end integration tests for the `hash-bake` subcommand.
//!
//! Exercise the CLI binary end to end (parse, dispatch, write) and
//! the roundtrip with `verify-hash` that this subcommand was added
//! to unblock.

use sentinel_core::report::periodic::compute_content_hash;
use sentinel_core::report::periodic::schema::{PeriodicReport, SignatureMetadata};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn workspace_doc(rel: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("..").join("..").join(rel)
}

fn run_bake(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("hash-bake")
        .args(args)
        .output()
        .expect("spawn perf-sentinel hash-bake")
}

fn run_verify(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("verify-hash")
        .args(args)
        .output()
        .expect("spawn perf-sentinel verify-hash")
}

fn copy_g2(dir: &std::path::Path) -> PathBuf {
    let example = workspace_doc("docs/schemas/examples/example-official-public-G2.json");
    let dest = dir.join("in.json");
    fs::write(&dest, fs::read(&example).unwrap()).unwrap();
    dest
}

fn dummy_signature() -> SignatureMetadata {
    SignatureMetadata {
        format: "sigstore-cosign-intoto-v1".to_string(),
        bundle_url: "https://example.invalid/x.sig".to_string(),
        signer_identity: "user@example.invalid".to_string(),
        signer_issuer: "https://accounts.google.com".to_string(),
        rekor_url: "https://rekor.sigstore.dev".to_string(),
        rekor_log_index: 1,
        signed_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

#[test]
fn hash_bake_writes_canonical_hash_matching_in_process_api() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let in_path = copy_g2(tmp.path());
    let out_path = tmp.path().join("out.json");

    let out = run_bake(&[
        "--report",
        in_path.to_str().unwrap(),
        "--output",
        out_path.to_str().unwrap(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let baked: PeriodicReport = serde_json::from_slice(&fs::read(&out_path).unwrap()).unwrap();
    let recomputed = compute_content_hash(&baked).unwrap();
    assert_eq!(baked.integrity.content_hash, recomputed);
    let zero_placeholder = format!("sha256:{}", "0".repeat(64));
    assert_ne!(baked.integrity.content_hash, zero_placeholder);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Content hash baked:"), "stdout: {stdout}");
}

#[test]
fn hash_bake_then_verify_hash_returns_partial_exit_2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let in_path = copy_g2(tmp.path());
    let out_path = tmp.path().join("baked.json");

    let bake = run_bake(&[
        "--report",
        in_path.to_str().unwrap(),
        "--output",
        out_path.to_str().unwrap(),
    ]);
    assert_eq!(bake.status.code(), Some(0));

    let verify = run_verify(&["--report", out_path.to_str().unwrap()]);
    assert_eq!(
        verify.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(stdout.contains("[OK] Content hash"), "{stdout}");
    assert!(stdout.contains("PARTIAL"), "{stdout}");
}

#[test]
fn hash_bake_refuses_signed_report_without_flag_e2e() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let in_path = tmp.path().join("signed.json");
    let out_path = tmp.path().join("out.json");
    let mut report: PeriodicReport = serde_json::from_slice(
        &fs::read(workspace_doc(
            "docs/schemas/examples/example-official-public-G2.json",
        ))
        .unwrap(),
    )
    .unwrap();
    report.integrity.signature = Some(dummy_signature());
    fs::write(&in_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();

    let out = run_bake(&[
        "--report",
        in_path.to_str().unwrap(),
        "--output",
        out_path.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!out_path.exists(), "refused bake must not create output");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already has integrity.signature populated"),
        "stderr: {stderr}"
    );
}

#[test]
fn hash_bake_in_place_then_verify_hash_partial() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = copy_g2(tmp.path());

    let bake = run_bake(&[
        "--report",
        path.to_str().unwrap(),
        "--output",
        path.to_str().unwrap(),
    ]);
    assert_eq!(
        bake.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&bake.stderr)
    );

    let verify = run_verify(&["--report", path.to_str().unwrap()]);
    assert_eq!(verify.status.code(), Some(2));
}

#[test]
fn hash_bake_after_tamper_fails_verify() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let in_path = copy_g2(tmp.path());
    let baked = tmp.path().join("baked.json");

    let bake = run_bake(&[
        "--report",
        in_path.to_str().unwrap(),
        "--output",
        baked.to_str().unwrap(),
    ]);
    assert_eq!(bake.status.code(), Some(0));

    let bytes = fs::read_to_string(&baked).unwrap();
    let mutated = bytes.replace("\"Example SAS\"", "\"Tampered SAS\"");
    assert_ne!(bytes, mutated, "tamper string must be present in fixture");
    fs::write(&baked, mutated).unwrap();

    let verify = run_verify(&["--report", baked.to_str().unwrap()]);
    assert_eq!(verify.status.code(), Some(1));
}
