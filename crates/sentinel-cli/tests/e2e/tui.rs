//! Unified TUI launch flags (`analyze --tui`, `explain --tui`, `inspect`).

#[cfg(feature = "tui")]
use std::fs;
#[cfg(feature = "tui")]
use std::io::Write;
#[cfg(feature = "tui")]
use std::process::{Command, Stdio};

#[cfg(feature = "tui")]
#[test]
fn cli_inspect_help_documents_report_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["inspect", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Report JSON"),
        "inspect --help must mention Report JSON input, got:\n{stdout}"
    );
    assert!(
        stdout.contains("/api/export/report"),
        "inspect --help must reference the daemon snapshot endpoint, got:\n{stdout}"
    );
}

// --- Unified TUI launch flags (`analyze --tui`, `explain --tui`) ---

/// `--tui` is mutually exclusive with the machine-output flags so it can
/// never fire in CI by accident: `analyze --tui --format json` is rejected
/// by clap before any work happens.
#[cfg(feature = "tui")]
#[test]
fn cli_analyze_tui_conflicts_with_format() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--tui", "--format", "json"])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(
        !output.status.success(),
        "analyze --tui --format must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected a clap conflict error, got: {stderr}"
    );
}

#[cfg(feature = "tui")]
#[test]
fn cli_analyze_tui_conflicts_with_ci() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--tui", "--ci"])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(
        !output.status.success(),
        "analyze --tui --ci must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected a clap conflict error, got: {stderr}"
    );
}

#[cfg(feature = "tui")]
#[test]
fn cli_explain_tui_conflicts_with_format() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("traces.json");
    fs::write(&file_path, b"[]").expect("failed to write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--tui",
            "--format",
            "json",
            "--input",
            file_path.to_str().unwrap(),
            "--trace-id",
            "x",
        ])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(
        !output.status.success(),
        "explain --tui --format must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected a clap conflict error, got: {stderr}"
    );
}

/// With stdout piped (not a TTY), the TUI launcher exits non-zero with a
/// clear message rather than trying to seize a terminal. This also proves
/// `--tui` parses and routes into the unified launcher.
#[cfg(feature = "tui")]
#[test]
fn cli_analyze_tui_requires_terminal_when_piped() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--tui"])
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
        !output.status.success(),
        "analyze --tui into a pipe should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("terminal"),
        "expected a 'requires a terminal' error, got: {stderr}"
    );
}
