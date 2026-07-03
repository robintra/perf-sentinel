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
fn cli_mysql_stat_malformed_input_exits_one() {
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

    assert!(!output.status.success(), "missing column must exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing required column"),
        "stderr should name the missing column, got: {stderr}"
    );
}
