//! `pg-stat` subcommand: exit-code contract for malformed input.
//!
//! `--pg-stat` via `report` already gets coverage in report.rs; this
//! module covers the standalone `pg-stat` subcommand, previously
//! untested end-to-end.

use crate::helpers::fixture_path;
use std::fs;
use std::process::{Command, Stdio};

const PG_STAT_CSV: &str = "../../tests/fixtures/pg_stat_statements.csv";

#[test]
fn cli_pg_stat_text_output_lists_rankings() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["pg-stat", "--input", &fixture_path(PG_STAT_CSV)])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "pg-stat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("pg_stat_statements analysis"),
        "missing report header, got:\n{stdout}"
    );
}

#[test]
fn cli_pg_stat_malformed_input_exits_tooling_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_path = dir.path().join("bad.csv");
    fs::write(&bad_path, "query,calls\nSELECT ?,10").expect("write bad csv");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["pg-stat", "--input", bad_path.to_str().unwrap()])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success(), "missing column must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing required column"),
        "stderr should name the missing column, got: {stderr}"
    );
    // pg-stat has no quality gate, every failure is a tooling error.
    // See docs/CI.md "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(75),
        "malformed input must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_pg_stat_missing_input_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["pg-stat", "--input", "nonexistent.csv"])
        .env("RUST_LOG", "error")
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(75),
        "missing input file must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}
