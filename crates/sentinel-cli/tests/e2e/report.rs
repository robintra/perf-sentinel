//! `report` subcommand: HTML dashboard output and input auto-detection.

use crate::helpers::extract_payload_json_from_html;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------
// `report` subcommand: HTML dashboard output.
// ---------------------------------------------------------------------

#[test]
fn cli_report_writes_html_file_from_input_flag() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn perf-sentinel");

    assert!(
        output.status.success(),
        "report subcommand failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out_path.exists(), "HTML output must exist");
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<script id=\"report-data\""));
    // Payload round-trips.
    let payload = extract_payload_json_from_html(&html);
    assert!(
        payload["report"]["findings"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "report must contain findings"
    );
}

#[test]
fn cli_report_reads_from_stdin_via_dash() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_minimal.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = fs::read(&fixture_path).expect("fixture readable");

    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            "-",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(&raw)
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "report from stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out_path.exists());
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.starts_with("<!DOCTYPE html>"));
    let payload = extract_payload_json_from_html(&html);
    assert_eq!(payload["input_label"], "-");
    // The minimal fixture exercises three patterns under one parent
    // (5 distinct order_item lookups, 3 identical orders lookups, and the
    // resulting sequential chain of 8 sibling SQL calls), hence one
    // `n_plus_one_sql`, one `redundant_sql`, and one `serialized_calls`.
    let findings = payload["report"]["findings"].as_array().unwrap();
    assert_eq!(
        findings.len(),
        3,
        "minimal fixture yields exactly 3 findings"
    );
    let types: std::collections::BTreeSet<&str> = findings
        .iter()
        .map(|f| f["type"].as_str().unwrap_or(""))
        .collect();
    let expected: std::collections::BTreeSet<&str> =
        ["n_plus_one_sql", "redundant_sql", "serialized_calls"]
            .into_iter()
            .collect();
    assert_eq!(
        types, expected,
        "minimal fixture must produce one of each type"
    );
}

#[test]
fn cli_report_help_mentions_all_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["report", "--help"])
        .output()
        .expect("spawn");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--input"), "help mentions --input");
    assert!(help.contains("--output"), "help mentions --output");
    assert!(help.contains("--config"), "help mentions --config");
    assert!(
        help.contains("--max-traces-embedded"),
        "help mentions --max-traces-embedded"
    );
    assert!(
        help.contains("--pg-stat-top"),
        "help mentions --pg-stat-top"
    );
}

#[test]
fn cli_report_exits_zero_on_quality_gate_fail() {
    // The realistic fixture fails the default quality gate (see
    // pipeline output during fixture crafting: quality_gate.passed =
    // false). `report` differs from `analyze --ci` here: it must exit
    // 0 regardless, because the gate status is rendered as a badge in
    // the HTML top bar, not as a CI signal.
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");

    assert!(
        output.status.success(),
        "report must exit 0 even when gate fails"
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    // The static shell carries both badge labels; check the payload
    // says the gate actually failed.
    let payload = extract_payload_json_from_html(&html);
    assert_eq!(
        payload["report"]["quality_gate"]["passed"], false,
        "gate status must be surfaced in the payload"
    );
}

#[test]
fn cli_report_overrides_default_cap_with_explicit_flag() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
            "--max-traces-embedded",
            "1",
        ])
        .output()
        .expect("spawn");
    assert!(output.status.success());

    let html = fs::read_to_string(&out_path).expect("read html");
    let payload = extract_payload_json_from_html(&html);
    let embedded = payload["embedded_traces"]
        .as_array()
        .expect("embedded_traces array");
    assert_eq!(embedded.len(), 1, "explicit cap must be honored exactly");
    let trimmed = &payload["trimmed_traces"];
    assert!(
        trimmed.is_object(),
        "trimmed_traces must be present when fewer traces are embedded than findings point to"
    );
    assert_eq!(trimmed["kept"], 1);
    // At least 2 distinct findings-bearing traces exist in the
    // realistic fixture; the `total` figure must reflect that.
    assert!(
        trimmed["total"].as_u64().unwrap() >= 2,
        "total must count all candidate traces"
    );
}

#[test]
fn cli_report_logs_trim_notice_when_capped() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
            "--max-traces-embedded",
            "1",
        ])
        .output()
        .expect("spawn");
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("trimmed for file size"),
        "expected trim notice in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("--max-traces-embedded"),
        "expected hint about --max-traces-embedded in stderr, got:\n{stderr}"
    );
}

#[test]
fn cli_report_omits_trim_notice_when_no_trim() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
            "--max-traces-embedded",
            "100",
        ])
        .output()
        .expect("spawn");
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("trimmed for file size"),
        "trim notice must not appear when embedded == total, got:\n{stderr}"
    );
}

// ---------------------------------------------------------------------
// `report` subcommand extensions: --pg-stat, --before, mutual exclusion.
// ---------------------------------------------------------------------

#[test]
fn cli_report_accepts_pg_stat_flag() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat",
            &pg_stat_fixture,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "report --pg-stat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    let payload = extract_payload_json_from_html(&html);
    let entries = payload["pg_stat"]["rankings"][0]["entries"]
        .as_array()
        .expect("rankings[0].entries");
    assert!(!entries.is_empty(), "pg_stat rankings must carry entries");
}

#[test]
fn cli_report_accepts_before_flag_for_diff() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let baseline = format!(
        "{}/../../tests/fixtures/baseline_report.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--before",
            &baseline,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "report --before failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    let payload = extract_payload_json_from_html(&html);
    let new_findings = payload["diff"]["new_findings"]
        .as_array()
        .expect("diff.new_findings");
    assert!(
        !new_findings.is_empty(),
        "realistic has findings the minimal baseline does not, so new_findings must be non-empty"
    );
    assert!(payload["diff"]["resolved_findings"].is_array());
}

#[cfg(feature = "daemon")]
#[test]
fn cli_report_rejects_both_pg_stat_and_pg_stat_prometheus() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat",
            &pg_stat_fixture,
            "--pg-stat-prometheus",
            "http://localhost:9090",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "mutual-exclusion must fail the invocation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--pg-stat") && stderr.contains("--pg-stat-prometheus"),
        "clap conflict message must mention both flags, got:\n{stderr}"
    );
}

#[test]
fn cli_report_pg_stat_top_overrides_default_ranking_size() {
    // Fixture has 15 entries, default top_n is 10, so --pg-stat-top 15
    // proves the flag flows through to rank_pg_stat.
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat",
            &pg_stat_fixture,
            "--pg-stat-top",
            "15",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "report --pg-stat-top failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    let payload = extract_payload_json_from_html(&html);
    let entries = payload["pg_stat"]["rankings"][0]["entries"]
        .as_array()
        .expect("rankings[0].entries");
    assert_eq!(
        entries.len(),
        15,
        "--pg-stat-top 15 must widen the ranking beyond the default top 10"
    );
}

#[test]
fn cli_report_pg_stat_top_rejects_zero() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat",
            &pg_stat_fixture,
            "--pg-stat-top",
            "0",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "--pg-stat-top 0 must fail clap's range validator"
    );
}

#[test]
fn cli_report_pg_stat_top_rejects_over_cap() {
    // Upper bound is 10_000. 10_001 must be rejected by clap's range
    // validator to keep local rank + upstream scrape cost bounded.
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat",
            &pg_stat_fixture,
            "--pg-stat-top",
            "10001",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "--pg-stat-top 10001 must fail clap's upper range bound"
    );
}

#[test]
fn cli_report_pg_stat_top_rejects_negative() {
    // Either the u32 parse error or the range validator fires, both
    // satisfy the non-zero exit contract.
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat",
            &pg_stat_fixture,
            "--pg-stat-top",
            "-1",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "--pg-stat-top -1 must fail clap parsing"
    );
}

#[test]
fn cli_report_pg_stat_top_requires_pg_stat_source() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--pg-stat-top",
            "5",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "--pg-stat-top without a pg_stat source must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--pg-stat-top requires --pg-stat"),
        "stderr must point at the required companion flag, got:\n{stderr}"
    );
}

#[test]
fn cli_report_renders_correlations_from_daemon_shape() {
    // A daemon `Report` carrying `correlations` must surface the
    // Correlations tab in the HTML output. Constructs the daemon-shape
    // JSON directly: spawning a live daemon and crafting OTLP ingestion
    // would be too expensive here.
    let daemon_report = serde_json::json!({
        "analysis": {
            "duration_ms": 42_000,
            "events_processed": 1200,
            "traces_analyzed": 87,
        },
        "findings": [{
            "type": "n_plus_one_sql",
            "severity": "warning",
            "trace_id": "daemon-trace-1",
            "service": "order-svc",
            "source_endpoint": "POST /api/orders/42/checkout",
            "pattern": {
                "template": "SELECT * FROM order_item WHERE order_id = ?",
                "occurrences": 12,
                "window_ms": 200,
                "distinct_params": 12,
            },
            "suggestion": "batch",
            "first_timestamp": "2026-04-21T10:00:00Z",
            "last_timestamp": "2026-04-21T10:00:01Z",
            "confidence": "daemon_production",
        }],
        "green_summary": {
            "total_io_ops": 1200,
            "avoidable_io_ops": 0,
            "io_waste_ratio": 0.0,
            "io_waste_ratio_band": "healthy",
            "top_offenders": [],
        },
        "quality_gate": { "passed": true, "rules": [] },
        "correlations": [{
            "source": {
                "finding_type": "n_plus_one_sql",
                "service": "order-svc",
                "template": "SELECT * FROM order_item WHERE order_id = ?",
            },
            "target": {
                "finding_type": "slow_http",
                "service": "payment-svc",
                "template": "POST /api/charge",
            },
            "co_occurrence_count": 8,
            "source_total_occurrences": 10,
            "confidence": 0.8,
            "median_lag_ms": 120.0,
            "first_seen": "2026-04-21T10:00:00Z",
            "last_seen": "2026-04-21T10:05:00Z",
            "sample_trace_id": "daemon-trace-1",
        }],
    });
    let raw = serde_json::to_vec(&daemon_report).unwrap();

    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            "-",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&raw)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "report --input - failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.contains(r#"id="panel-correlations""#));
    let payload = extract_payload_json_from_html(&html);
    let corrs = payload["report"]["correlations"]
        .as_array()
        .expect("correlations array");
    assert_eq!(corrs.len(), 1);
    assert_eq!(corrs[0]["source"]["service"].as_str().unwrap(), "order-svc");
    assert_eq!(
        corrs[0]["target"]["service"].as_str().unwrap(),
        "payment-svc"
    );
    assert_eq!(
        corrs[0]["sample_trace_id"].as_str().unwrap(),
        "daemon-trace-1"
    );
    // Live DOM behavior covered by the Playwright suite.
    assert!(html.contains("ps-correlation-clickable"));
}

#[test]
fn cli_report_accepts_bom_prefixed_report_json() {
    // Windows editors (Notepad, some VS Code flows) save UTF-8 with a
    // leading BOM (EF BB BF). The auto-detect's byte-peek used to trip
    // on the BOM and reject the input; this test pins down the strip.
    let mut raw = vec![0xEF, 0xBB, 0xBF];
    raw.extend_from_slice(
        serde_json::to_vec(&serde_json::json!({
            "analysis": {
                "duration_ms": 0,
                "events_processed": 1,
                "traces_analyzed": 1,
            },
            "findings": [],
            "green_summary": {
                "total_io_ops": 0,
                "avoidable_io_ops": 0,
                "io_waste_ratio": 0.0,
                "io_waste_ratio_band": "healthy",
                "top_offenders": [],
            },
            "quality_gate": { "passed": true, "rules": [] },
        }))
        .unwrap()
        .as_slice(),
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            "-",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&raw)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "report --input should accept BOM-prefixed Report JSON, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.starts_with("<!DOCTYPE html>"));
}

#[test]
fn cli_report_rejects_scalar_root_and_empty_input_with_distinct_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    // Empty input: message must mention emptiness, not "scalar".
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            "-",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"   \n")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("empty or whitespace-only"),
        "empty-input error must be specific, got: {stderr}"
    );

    // Scalar root: message must mention "scalar or unexpected token".
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            "-",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child.stdin.take().unwrap().write_all(b"42").expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("scalar or unexpected token"),
        "scalar-root error must differentiate from empty input, got: {stderr}"
    );
}

#[test]
fn cli_report_help_mentions_new_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["report", "--help"])
        .output()
        .expect("spawn");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--pg-stat"), "help mentions --pg-stat");
    assert!(help.contains("--before"), "help mentions --before");
    #[cfg(feature = "daemon")]
    assert!(
        help.contains("--pg-stat-prometheus"),
        "help mentions --pg-stat-prometheus when daemon feature is on"
    );
}

// Regression suite for the input format auto-detection contract of
// `report --input`. Pre-0.5.14 the helper dispatched on first byte only,
// so a Jaeger export (`{"data": [...]}`) was misrouted to the Report
// parser and died on `missing field 'analysis'`. The fix makes the `{`
// branch try Report first and fall back to JsonIngest (which handles
// Jaeger via detect_format).

#[test]
fn cli_report_accepts_jaeger_input() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/jaeger_export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("dashboard.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("spawn");

    assert!(
        output.status.success(),
        "report should accept Jaeger input: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.contains("perf-sentinel"));
    assert!(html.contains("\"findings\""));
}

#[test]
fn cli_report_accepts_zipkin_input() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/zipkin_export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("dashboard.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("spawn");

    assert!(
        output.status.success(),
        "report should accept Zipkin input: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.contains("perf-sentinel"));
    assert!(html.contains("\"findings\""));
}

#[test]
fn cli_report_accepts_native_input() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("dashboard.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture_path,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("spawn");

    assert!(
        output.status.success(),
        "report should accept native event input: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.contains("perf-sentinel"));
    assert!(html.contains("\"findings\""));
}

#[test]
fn cli_report_accepts_report_snapshot_input() {
    // The "try Report first" fast path: feed a daemon-shape Report JSON
    // to `report --input -` and assert the helper short-circuits to the
    // Report parser without any re-analysis. The fixture carries a
    // populated `green_summary` (top_offenders, regions, scoring_config)
    // so the test also regression-guards verbatim flow-through of the
    // GreenOps audit-trail fields on the snapshot path.
    let snapshot = serde_json::json!({
        "analysis": {
            "duration_ms": 0,
            "events_processed": 42,
            "traces_analyzed": 7,
        },
        "findings": [],
        "green_summary": {
            "total_io_ops": 42,
            "avoidable_io_ops": 9,
            "io_waste_ratio": 0.214,
            "io_waste_ratio_band": "moderate",
            "top_offenders": [{
                "endpoint": "POST /api/orders/checkout",
                "service": "order-svc",
                "io_intensity_score": 0.87,
                "io_intensity_band": "high",
            }],
            "regions": [{
                "status": "known",
                "region": "eu-west-3",
                "grid_intensity_gco2_kwh": 41.0,
                "pue": 1.15,
                "io_ops": 42,
                "co2_gco2": 0.123,
            }],
            "scoring_config": {
                "api_version": "v4",
                "emission_factor_type": "lifecycle",
                "temporal_granularity": "hourly",
            },
        },
        "quality_gate": { "passed": true, "rules": [] },
    });
    let raw = serde_json::to_vec(&snapshot).unwrap();

    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("dashboard.html");

    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            "-",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&raw)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");

    assert!(
        output.status.success(),
        "report should accept Report JSON snapshot: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.contains("perf-sentinel"));

    // Verbatim flow-through of the populated GreenSummary fields.
    let payload = extract_payload_json_from_html(&html);
    let green = &payload["report"]["green_summary"];
    assert_eq!(green["total_io_ops"], 42);
    assert_eq!(green["avoidable_io_ops"], 9);
    let offenders = green["top_offenders"].as_array().expect("top_offenders");
    assert_eq!(offenders.len(), 1);
    assert_eq!(offenders[0]["service"].as_str().unwrap(), "order-svc");
    let regions = green["regions"].as_array().expect("regions");
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0]["region"].as_str().unwrap(), "eu-west-3");
    assert_eq!(
        green["scoring_config"]["api_version"].as_str().unwrap(),
        "v4"
    );
}

#[test]
fn cli_report_rejects_invalid_input_with_clear_error() {
    // Pre-0.5.14, a Jaeger payload produced "missing field 'analysis'",
    // a low-level serde message that hid the real disambiguation. The
    // fix surfaces a stderr that names both accepted top-level-object
    // shapes (Report JSON and Jaeger export) when neither parses.
    let dir = tempfile::tempdir().expect("tempdir");
    let bogus_path = dir.path().join("bogus.json");
    fs::write(&bogus_path, r#"{"foo": "bar"}"#).expect("write bogus");
    let out_path = dir.path().join("dashboard.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            bogus_path.to_str().unwrap(),
            "--output",
            out_path.to_str().unwrap(),
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("spawn");

    assert!(!output.status.success(), "bogus input must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("missing field 'analysis'"),
        "0.5.14 must not surface the raw serde missing-field error: {stderr}"
    );
    assert!(
        stderr.contains("Report JSON") && stderr.contains("Jaeger"),
        "stderr must disambiguate accepted top-level object shapes: {stderr}"
    );
}
