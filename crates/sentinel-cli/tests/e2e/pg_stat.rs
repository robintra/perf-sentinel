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

#[cfg(feature = "daemon")]
#[test]
fn cli_pg_stat_series_flags_require_the_prometheus_endpoint() {
    // `--unit` carries a closed value set, so it needs a value clap accepts
    // to reach the requires check at all.
    for (flag, value) in [
        ("--metric", "whatever"),
        ("--query-label", "whatever"),
        ("--calls-metric", "whatever"),
        ("--unit", "milliseconds"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
            .args(["pg-stat", "--input", "unused.csv", flag, value])
            .env("RUST_LOG", "error")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("failed to execute perf-sentinel");

        assert!(!output.status.success(), "{flag} alone must be rejected");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--prometheus"),
            "{flag} should point at its companion flag, got: {stderr}"
        );
    }
}

#[cfg(feature = "daemon")]
#[test]
fn cli_pg_stat_rejects_a_series_name_that_escapes_the_query_string() {
    // Both series land unencoded in the same query string, so both need the
    // guard. Rejected before any request leaves the process, so an unreachable
    // endpoint is fine here: reaching the network would itself be the bug.
    for flag in ["--metric", "--calls-metric"] {
        let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
            .args([
                "pg-stat",
                "--prometheus",
                "http://127.0.0.1:1",
                flag,
                "pg&admin=1",
            ])
            .env("RUST_LOG", "error")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("failed to execute perf-sentinel");

        assert!(!output.status.success(), "{flag} must be rejected");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("bare PromQL metric name"),
            "stderr should name the grammar it violates, got: {stderr}"
        );
    }
}

#[cfg(feature = "daemon")]
#[test]
fn cli_pg_stat_rejects_an_unknown_unit() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "pg-stat",
            "--prometheus",
            "http://127.0.0.1:1",
            "--unit",
            "picoseconds",
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success(), "an unknown unit must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("seconds") && stderr.contains("milliseconds"),
        "stderr should list the accepted units, got: {stderr}"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_pg_stat_help_lists_the_prometheus_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["pg-stat", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");

    let help = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--prometheus",
        "--auth-header",
        "--metric",
        "--query-label",
        "--calls-metric",
        "--unit",
    ] {
        assert!(help.contains(flag), "pg-stat --help should list {flag}");
    }
}
