//! End-to-end integration tests for the perf-sentinel CLI.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn cli_demo_runs_successfully() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("demo")
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success(), "demo command failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("perf-sentinel demo"),
        "demo output should contain header"
    );
    assert!(
        stdout.contains("N+1 SQL") || stdout.contains("N+1 HTTP"),
        "demo output should contain findings"
    );
}

#[test]
fn cli_analyze_reads_from_stdin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("analyze")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perf-sentinel");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"[]")
        .expect("failed to write to stdin");

    let output = child.wait_with_output().expect("failed to wait on child");
    assert!(
        output.status.success(),
        "analyze from stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_analyze_reads_from_file() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("traces.json");
    fs::write(&file_path, b"[]").expect("failed to write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", file_path.to_str().unwrap()])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "analyze from file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("findings"));
}

#[test]
fn cli_analyze_reads_fixture_with_findings() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "analyze failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("n_plus_one_sql"),
        "expected N+1 SQL finding in output"
    );
}

#[test]
fn cli_analyze_rejects_missing_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", "nonexistent.json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
}

#[test]
fn cli_help_shows_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("--help")
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("analyze"));
    assert!(stdout.contains("watch"));
    assert!(stdout.contains("demo"));
}

#[test]
fn cli_demo_piped_no_ansi() {
    // When stdout is piped (not a TTY), demo should not emit ANSI escape codes
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("demo")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success(), "demo command failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\x1b["),
        "piped output should not contain ANSI escape codes, got: {stdout}"
    );
}
