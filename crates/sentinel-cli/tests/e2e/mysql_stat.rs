//! `mysql-stat` subcommand: digest parsing, rankings, cross-reference.

use crate::helpers::fixture_path;
use serde_json::Value;
use std::fs;
use std::process::{Command, Stdio};

const MYSQL_CSV: &str = "../../tests/fixtures/mysql_perf_schema.csv";

#[test]
fn cli_mysql_stat_text_output_lists_all_rankings() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["mysql-stat", "--input", &fixture_path(MYSQL_CSV)])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "mysql-stat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for label in [
        "top by total_exec_time",
        "top by calls",
        "top by mean_exec_time",
        "top by rows_examined",
    ] {
        assert!(stdout.contains(label), "missing ranking '{label}'");
    }
}

#[test]
fn cli_mysql_stat_json_output_has_stable_ranking_order() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "mysql-stat",
            "--input",
            &fixture_path(MYSQL_CSV),
            "--format",
            "json",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("mysql-stat JSON should parse: {e}\nstdout: {stdout}"));
    assert_eq!(report["total_entries"], 15);
    assert_eq!(report["rankings"][3]["label"], "top by rows_examined");
    // Picosecond timers must arrive converted: 45_005_000_000_000 ps = 45005 ms.
    assert!(
        (report["rankings"][0]["entries"][0]["total_exec_time_ms"]
            .as_f64()
            .unwrap()
            - 45005.0)
            .abs()
            < 0.001
    );
}

#[test]
fn cli_mysql_stat_traces_cross_reference_sets_marker() {
    // Build a trace file whose N+1 finding template matches the fixture's
    // first digest (`SELECT * FROM `order_item` WHERE `order_id` = ?`).
    let dir = tempfile::tempdir().expect("tempdir");
    let traces_path = dir.path().join("traces.json");
    let mut events = Vec::new();
    for i in 1..=6 {
        events.push(serde_json::json!({
            "timestamp": format!("2025-07-10T14:32:01.{:03}Z", i * 40),
            "trace_id": "trace-1",
            "span_id": format!("span-{i}"),
            "service": "shop-svc",
            "type": "sql",
            "operation": "SELECT",
            "target": format!("SELECT * FROM `order_item` WHERE `order_id` = {i}"),
            "duration_us": 800,
            "source": {"endpoint": "GET /api/orders", "method": "OrderService::list"}
        }));
    }
    fs::write(&traces_path, serde_json::to_vec(&events).unwrap()).expect("write traces");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "mysql-stat",
            "--input",
            &fixture_path(MYSQL_CSV),
            "--traces",
            traces_path.to_str().unwrap(),
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "mysql-stat --traces failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[seen in traces]"),
        "matching digest should carry the trace marker, got:\n{stdout}"
    );
}

#[test]
fn cli_mysql_stat_malformed_input_exits_tooling_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_path = dir.path().join("bad.csv");
    fs::write(&bad_path, "DIGEST_TEXT,COUNT_STAR\nSELECT ?,10").expect("write bad csv");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["mysql-stat", "--input", bad_path.to_str().unwrap()])
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
    // mysql-stat has no quality gate, every failure is a tooling error.
    // See docs/CI.md "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(75),
        "malformed input must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

// ── Prometheus scrape surface ──────────────────────────────────────
//
// The scrape itself needs a live Prometheus, so these cover what can be
// asserted without one: the flag wiring, the pairing rules clap enforces,
// and the exit-code contract. A `requires` id typo or a flipped argument
// order would otherwise ship green.

#[test]
fn cli_mysql_stat_without_any_source_exits_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["mysql-stat"])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--input"),
        "stderr should name the missing flag, got: {stderr}"
    );
    // A permanent invocation mistake is a usage error, never the tolerable
    // 75 bucket a pipeline may ignore. See docs/CI.md "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(2),
        "no source at all must exit 2, not EXIT_TOOLING_ERROR"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_mysql_stat_series_flags_require_the_prometheus_endpoint() {
    for flag in ["--metric", "--query-label"] {
        let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
            .args(["mysql-stat", "--input", "unused.csv", flag, "whatever"])
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
fn cli_mysql_stat_rejects_a_series_name_that_escapes_the_query_string() {
    // Rejected before any request leaves the process, so an unreachable
    // endpoint is fine here: reaching the network would itself be the bug.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "mysql-stat",
            "--prometheus",
            "http://127.0.0.1:1",
            "--metric",
            "mysql&admin=1",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bare PromQL metric name"),
        "stderr should name the grammar it violates, got: {stderr}"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_mysql_stat_help_lists_the_prometheus_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["mysql-stat", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");

    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--prometheus", "--auth-header", "--metric", "--query-label"] {
        assert!(help.contains(flag), "mysql-stat --help should list {flag}");
    }
}
