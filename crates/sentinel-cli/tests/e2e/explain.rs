//! `explain` subcommand.

use serde_json::Value;
use std::fs;
use std::process::{Command, Stdio};

#[test]
fn cli_explain_text_output() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            &fixture_path,
            "--trace-id",
            "trace-n1-sql",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "explain should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("trace-n1-sql"), "should show trace ID");
}

#[test]
fn cli_explain_json_output() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            &fixture_path,
            "--trace-id",
            "trace-n1-sql",
            "--format",
            "json",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "explain --format json should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tree: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("explain JSON should be valid: {e}\nstdout: {stdout}"));
    assert_eq!(tree["trace_id"], "trace-n1-sql");
}

#[test]
fn cli_explain_unknown_trace_id_fails() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            &fixture_path,
            "--trace-id",
            "nonexistent-trace",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        !output.status.success(),
        "should fail with unknown trace ID"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "should mention trace not found"
    );
}

#[test]
fn cli_help_shows_explain() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("--help")
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("explain"),
        "help should list explain subcommand"
    );
}

/// On an unknown --trace-id, explain lists the available ids so the
/// operator can recover from a typo. This exercises the shared
/// `trace_not_found_exit` helper that `explain --tui` reuses (the --tui
/// path is gated on a TTY, so the recovery hint is asserted here on the
/// pipe-safe non-interactive command).
#[test]
fn cli_explain_unknown_trace_lists_available_ids() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("traces.json");
    fs::write(&file_path, b"[]").expect("failed to write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            file_path.to_str().unwrap(),
            "--trace-id",
            "nope",
        ])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(
        !output.status.success(),
        "explain with an unknown trace id must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("trace ID 'nope' not found"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("Available trace IDs"),
        "expected the available-ids recovery hint, got: {stderr}"
    );
}
