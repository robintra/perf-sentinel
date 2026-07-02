//! `disclose` subcommand flag contracts.

use crate::helpers::ORG_CONFIG_EXAMPLE;
use std::fs;
use std::process::Command;

#[test]
fn cli_disclose_tui_relaxes_period_flags_and_requires_terminal() {
    // `--tui` makes --intent/--confidentiality/--period-type/--from/--to/--output
    // optional. With stdout piped (no TTY) the preview exits via the terminal
    // guard (code 1), not a clap missing-argument error (code 2) — proving clap
    // accepted the omitted flags. --input and --org-config stay required.
    let dir = tempfile::tempdir().expect("temp dir");
    let archive = dir.path().join("archive.ndjson");
    fs::write(
        &archive,
        "{\"ts\":\"2026-03-15T00:00:00Z\",\"report\":{}}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("disclose")
        .arg("--tui")
        .arg("--input")
        .arg(&archive)
        .arg("--org-config")
        .arg(ORG_CONFIG_EXAMPLE)
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires a terminal"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn cli_disclose_without_tui_requires_period_flags() {
    // Canonical (non-TUI) disclose still demands the period/intent flags;
    // omitting them is a clap usage error (exit code 2).
    let dir = tempfile::tempdir().expect("temp dir");
    let archive = dir.path().join("archive.ndjson");
    fs::write(
        &archive,
        "{\"ts\":\"2026-03-15T00:00:00Z\",\"report\":{}}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("disclose")
        .arg("--input")
        .arg(&archive)
        .arg("--org-config")
        .arg(ORG_CONFIG_EXAMPLE)
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected a clap usage error, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--intent") || stderr.contains("required"),
        "unexpected stderr: {stderr}"
    );
}
