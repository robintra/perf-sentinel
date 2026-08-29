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
    assert!(help.contains("--sort"), "help mentions --sort");
    assert!(
        help.contains("--max-traces-embedded"),
        "help mentions --max-traces-embedded"
    );
    assert!(
        help.contains("--pg-stat-top"),
        "help mentions --pg-stat-top"
    );
    assert!(
        help.contains("--mysql-stat-top"),
        "help mentions --mysql-stat-top"
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
    // The operator set the cap, so the notice names it as theirs instead
    // of prescribing the very flag that caused the trim.
    assert!(
        stderr.contains("past the --max-traces-embedded cap"),
        "expected the explicit-cap notice in stderr, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("trimmed for file size"),
        "the size-budget wording must not show on an explicit cap, got:\n{stderr}"
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
fn cli_report_accepts_otlp_json_input() {
    // Covers the load_report_from_input `{`-object fallback: an OTLP/JSON
    // export is not a Report, so it must route through JsonIngest.
    let fixture = format!(
        "{}/../../tests/fixtures/otlp_export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "report on OTLP JSON failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    let payload = extract_payload_json_from_html(&html);
    let findings = payload["report"]["findings"]
        .as_array()
        .expect("report.findings");
    assert!(
        !findings.is_empty(),
        "OTLP input should analyze into findings"
    );
}

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

/// A trace input populates the trace-matched share on the pg_stat
/// panel, and a pre-computed Report input carries no traces at all, so
/// the share must be absent rather than a "0 of N" that reads as a
/// tracing gap.
#[test]
fn cli_report_stamps_trace_match_only_when_traces_were_analyzed() {
    let traces = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let pg_stat_fixture = format!(
        "{}/../../tests/fixtures/pg_stat_statements.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");

    let render = |input: &str, out: &std::path::Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
            .args([
                "report",
                "--input",
                input,
                "--pg-stat",
                &pg_stat_fixture,
                "--output",
                out.to_str().unwrap(),
            ])
            .output()
            .expect("spawn");
        assert!(
            output.status.success(),
            "report failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        extract_payload_json_from_html(&fs::read_to_string(out).expect("read html"))
    };

    let from_traces = render(&traces, &dir.path().join("traces.html"));
    assert!(
        from_traces["pg_stat"]["trace_match"].is_object(),
        "a trace input must carry the matched share"
    );

    // Feed the analysis back in: `analyze --format json` is a Report,
    // which `report` accepts and which carries no traces.
    let analyzed = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &traces, "--format", "json"])
        .output()
        .expect("spawn analyze");
    assert!(
        analyzed.status.success(),
        "analyze failed, the precomputed input would be empty: {}",
        String::from_utf8_lossy(&analyzed.stderr)
    );
    let report_path = dir.path().join("precomputed.json");
    fs::write(&report_path, &analyzed.stdout).expect("write report");

    let from_report = render(report_path.to_str().unwrap(), &dir.path().join("pre.html"));
    // The panel must still be there: indexing a missing pg_stat would
    // also yield null and make the assertion below vacuous.
    assert!(
        from_report["pg_stat"].is_object(),
        "the pg_stat panel must survive a pre-computed input"
    );
    assert!(
        from_report["pg_stat"]["trace_match"].is_null(),
        "a pre-computed report has no traces, so it must claim no matched share, got {}",
        from_report["pg_stat"]["trace_match"]
    );
}

#[test]
fn cli_report_accepts_mysql_stat_flag() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mysql_fixture = format!(
        "{}/../../tests/fixtures/mysql_perf_schema.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--mysql-stat",
            &mysql_fixture,
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "report --mysql-stat failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = fs::read_to_string(&out_path).expect("read html");
    let payload = extract_payload_json_from_html(&html);
    let rankings = payload["mysql_stat"]["rankings"]
        .as_array()
        .expect("mysql_stat.rankings");
    assert_eq!(rankings.len(), 4, "four rankings in stable order");
    assert_eq!(rankings[3]["label"], "top by rows_examined");
    assert!(
        !rankings[0]["entries"].as_array().unwrap().is_empty(),
        "mysql_stat rankings must carry entries"
    );
}

#[test]
fn cli_report_mysql_stat_top_requires_mysql_stat() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--mysql-stat-top",
            "5",
            "--output",
            "/dev/null",
        ])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "--mysql-stat-top without --mysql-stat must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--mysql-stat"),
        "error should point at the companion flag, got: {stderr}"
    );
}

#[test]
fn cli_report_mysql_stat_top_rejects_out_of_range() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mysql_fixture = format!(
        "{}/../../tests/fixtures/mysql_perf_schema.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    for bad in ["0", "10001"] {
        let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
            .args([
                "report",
                "--input",
                &fixture,
                "--mysql-stat",
                &mysql_fixture,
                "--mysql-stat-top",
                bad,
                "--output",
                "/dev/null",
            ])
            .output()
            .expect("spawn");
        assert!(
            !output.status.success(),
            "--mysql-stat-top {bad} must be rejected"
        );
    }
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
    // An unsupported flag combination is a usage error and exits 2,
    // matching clap's own usage-error code, never the tolerable 75.
    // See docs/CI.md "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(2),
        "post-parse usage validation must exit 2, not 75 or 1"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_report_malformed_daemon_url_exits_tooling_error() {
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
            "--output",
            out_path.to_str().unwrap(),
            "--daemon-url",
            "not-a-url",
        ])
        .output()
        .expect("spawn");

    assert!(!output.status.success(), "malformed --daemon-url must fail");
    // report has no quality gate. See docs/CI.md "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(75),
        "malformed --daemon-url must exit EXIT_TOOLING_ERROR (75), not 1"
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
    // The rendered markup carries a click zone per side of the pair. The
    // click behaviour itself is covered by browser test 24, which drives
    // the same class.
    assert!(html.contains("ps-corr-side-link"));
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
    assert_eq!(
        output.status.code(),
        Some(75),
        "malformed --input must exit EXIT_TOOLING_ERROR (75), report has no gate to breach"
    );
}

// ---------------------------------------------------------------------
// `report --mysql-stat`: the embedded ranking source honors the same
// exit-code contract. A malformed digest export is a tooling failure,
// never a gate breach. See docs/CI.md "Exit codes".
// ---------------------------------------------------------------------

#[test]
fn cli_report_malformed_mysql_stat_exits_tooling_error() {
    let valid_input = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_mysql = dir.path().join("bad.csv");
    fs::write(&bad_mysql, "DIGEST_TEXT,COUNT_STAR\nSELECT ?,10").expect("write bad csv");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &valid_input,
            "--mysql-stat",
            bad_mysql.to_str().unwrap(),
            "--output",
            out_path.to_str().unwrap(),
        ])
        .env("RUST_LOG", "error")
        .output()
        .expect("spawn");

    assert!(
        !output.status.success(),
        "malformed --mysql-stat must fail the report"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a malformed mysql_stat source is a tooling failure (75), not a gate breach"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_report_rejects_both_mysql_stat_and_mysql_stat_prometheus() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mysql_stat_fixture = format!(
        "{}/../../tests/fixtures/mysql_perf_schema.csv",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--mysql-stat",
            &mysql_stat_fixture,
            "--mysql-stat-prometheus",
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
        stderr.contains("--mysql-stat") && stderr.contains("--mysql-stat-prometheus"),
        "clap conflict message must mention both flags, got:\n{stderr}"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_report_series_flags_require_the_prometheus_endpoint() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    // (flag, value, companion the error must name). `--pg-stat-unit` carries a
    // closed value set, so it needs a value clap accepts to reach the
    // requires check at all.
    for (flag, value, companion) in [
        ("--mysql-stat-metric", "whatever", "--mysql-stat-prometheus"),
        (
            "--mysql-stat-query-label",
            "whatever",
            "--mysql-stat-prometheus",
        ),
        (
            "--mysql-stat-auth-header",
            "whatever",
            "--mysql-stat-prometheus",
        ),
        (
            "--mysql-stat-calls-metric",
            "whatever",
            "--mysql-stat-prometheus",
        ),
        ("--pg-stat-metric", "whatever", "--pg-stat-prometheus"),
        ("--pg-stat-query-label", "whatever", "--pg-stat-prometheus"),
        ("--pg-stat-auth-header", "whatever", "--pg-stat-prometheus"),
        ("--pg-stat-calls-metric", "whatever", "--pg-stat-prometheus"),
        ("--pg-stat-unit", "milliseconds", "--pg-stat-prometheus"),
        (
            "--mysql-stat-rows-sent-metric",
            "whatever",
            "--mysql-stat-prometheus",
        ),
        (
            "--mysql-stat-rows-examined-metric",
            "whatever",
            "--mysql-stat-prometheus",
        ),
        (
            "--mysql-stat-schema-label",
            "whatever",
            "--mysql-stat-prometheus",
        ),
        (
            "--mysql-stat-unit",
            "picoseconds",
            "--mysql-stat-prometheus",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
            .args([
                "report",
                "--input",
                &fixture,
                flag,
                value,
                "--output",
                out_path.to_str().unwrap(),
            ])
            .output()
            .expect("spawn");
        assert!(!output.status.success(), "{flag} alone must be rejected");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(companion),
            "{flag} should point at {companion}, got:\n{stderr}"
        );
    }
}

#[cfg(feature = "daemon")]
#[test]
fn cli_report_mysql_stat_top_accepts_either_source_and_rejects_neither() {
    let fixture = format!(
        "{}/../../tests/fixtures/report_realistic.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");

    // The pairing is checked after parsing, since clap `requires` cannot
    // express "one of two flags". It must still block, and before any I/O.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "report",
            "--input",
            &fixture,
            "--mysql-stat-top",
            "5",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(2),
        "a usage error must exit 2, not the tolerable 75 bucket"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--mysql-stat-prometheus"),
        "the message should name both accepted sources, got:\n{stderr}"
    );
}

#[cfg(feature = "daemon")]
#[test]
fn cli_report_help_lists_the_mysql_stat_prometheus_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["report", "--help"])
        .output()
        .expect("spawn");

    let help = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--mysql-stat-prometheus",
        "--mysql-stat-auth-header",
        "--mysql-stat-metric",
        "--mysql-stat-query-label",
    ] {
        assert!(help.contains(flag), "report --help should list {flag}");
    }
}

// ---------------------------------------------------------------------
// Findings order: `--sort`, its default, and the span-tree embed that
// follows it. The realistic fixture yields six findings whose detector
// order is [order-01, order-02, notify-01, payment-01, payment-02,
// chat-05] and whose six signatures are all distinct, so every finding
// is its own recurrence group and the aggregate impact the sort ranks on
// equals the unitary `estimated_extra_io_ops`. Both sort permutations
// contain a 3-cycle, which is what makes these tests a guard: applying
// the inverse permutation instead of the permutation leaves swaps intact
// and misorders every longer cycle, so it would put trace-order-02 first.
// ---------------------------------------------------------------------

/// Render a workspace-root fixture with extra flags and return the
/// embedded payload.
fn render_fixture(fixture: &str, extra: &[&str]) -> serde_json::Value {
    let fixture_path = format!(
        "{}/../../tests/fixtures/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("report.html");
    let mut args = vec![
        "report",
        "--input",
        &fixture_path,
        "--output",
        out_path.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(&args)
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "report {extra:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    extract_payload_json_from_html(&fs::read_to_string(&out_path).expect("read html"))
}

/// Render the realistic fixture with extra flags and return the embedded
/// payload.
fn render_realistic(extra: &[&str]) -> serde_json::Value {
    render_fixture("report_realistic.json", extra)
}

/// Trace ids of the findings, in payload order.
fn finding_trace_ids(payload: &serde_json::Value) -> Vec<String> {
    payload["report"]["findings"]
        .as_array()
        .expect("report.findings")
        .iter()
        .map(|f| f["trace_id"].as_str().expect("trace_id").to_string())
        .collect()
}

/// Avoidable I/O ops of each finding, in payload order. Absent green
/// impact reads as zero, the same weight `sort_findings` gives it.
fn finding_impacts(payload: &serde_json::Value) -> Vec<u64> {
    payload["report"]["findings"]
        .as_array()
        .expect("report.findings")
        .iter()
        .map(|f| {
            f["green_impact"]["estimated_extra_io_ops"]
                .as_u64()
                .unwrap_or(0)
        })
        .collect()
}

/// Worst-first rank of each severity, in payload order.
fn severity_ranks(payload: &serde_json::Value) -> Vec<u8> {
    payload["report"]["findings"]
        .as_array()
        .expect("report.findings")
        .iter()
        .map(|f| match f["severity"].as_str().expect("severity") {
            "critical" => 0,
            "warning" => 1,
            "info" => 2,
            other => panic!("unknown severity {other}"),
        })
        .collect()
}

const IMPACT_ORDER: [&str; 6] = [
    "trace-notify-01",
    "trace-order-01",
    "trace-order-02",
    "trace-payment-01",
    "trace-payment-02",
    "trace-chat-05",
];

const SEVERITY_ORDER: [&str; 6] = [
    "trace-notify-01",
    "trace-order-01",
    "trace-order-02",
    "trace-chat-05",
    "trace-payment-01",
    "trace-payment-02",
];

// The realistic fixture holds no critical finding, so it can only prove
// warning-before-info. demo.json is the one workspace-root fixture that
// carries all three severities (1 critical, 7 warnings, 3 infos), which
// is what pins the critical rank to the top of the list.
const DEMO_SEVERITY_ORDER: [&str; 11] = [
    "trace-demo-nplus-sql",
    "trace-demo-messaging",
    "trace-demo-nplus-http",
    "trace-demo-slow-sql",
    "trace-demo-slow-http",
    "trace-demo-fanout",
    "trace-demo-chatty",
    "trace-demo-pool",
    "trace-demo-redundant-sql",
    "trace-demo-redundant-http",
    "trace-demo-serialized",
];

#[test]
fn cli_report_sort_impact_ranks_findings_by_descending_impact() {
    let payload = render_realistic(&["--sort", "impact"]);
    let traces = finding_trace_ids(&payload);
    assert_eq!(
        traces, IMPACT_ORDER,
        "the whole sequence must be the impact ranking, not a permutation of it"
    );
    // The head and the tail on their own, the two rows a reader lands on.
    assert_eq!(traces[0], "trace-notify-01", "5 avoidable ops leads");
    assert_eq!(traces[5], "trace-chat-05", "0 avoidable ops closes");
    let impacts = finding_impacts(&payload);
    assert_eq!(impacts, [5, 4, 4, 3, 2, 0]);
    assert!(
        impacts.windows(2).all(|w| w[0] >= w[1]),
        "impact must not climb back up anywhere in the list, got {impacts:?}"
    );
}

#[test]
fn cli_report_sort_severity_ranks_findings_worst_first() {
    let payload = render_realistic(&["--sort", "severity"]);
    let traces = finding_trace_ids(&payload);
    assert_eq!(
        traces, SEVERITY_ORDER,
        "the whole sequence must be the severity ranking, not a permutation of it"
    );
    assert_eq!(traces[0], "trace-notify-01", "worst severity leads");
    assert_eq!(
        traces[5], "trace-payment-02",
        "the lightest info closes the list"
    );
    let ranks = severity_ranks(&payload);
    assert_eq!(ranks, [1, 1, 1, 1, 2, 2], "four warnings then two infos");
    assert!(
        ranks.windows(2).all(|w| w[0] <= w[1]),
        "severity must never improve then worsen again, got {ranks:?}"
    );
}

#[test]
fn cli_report_sort_severity_puts_the_critical_above_every_warning_and_info() {
    let payload = render_fixture("demo.json", &["--sort", "severity"]);
    let traces = finding_trace_ids(&payload);
    assert_eq!(
        traces, DEMO_SEVERITY_ORDER,
        "the whole sequence must be the severity ranking, not a permutation of it"
    );
    let ranks = severity_ranks(&payload);
    assert_eq!(
        ranks,
        [0, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2],
        "one critical, then seven warnings, then three infos"
    );
    // The three ranks must all be reached, otherwise the ordering below
    // is asserted across a subset of the scale, which is the very gap
    // the realistic fixture leaves.
    for rank in [0, 1, 2] {
        assert!(
            ranks.contains(&rank),
            "rank {rank} must be exercised, got {ranks:?}"
        );
    }
    assert_eq!(traces[0], "trace-demo-nplus-sql", "the critical leads");
    assert_eq!(
        traces[10], "trace-demo-serialized",
        "the lightest info closes the list"
    );
    assert!(
        ranks.windows(2).all(|w| w[0] <= w[1]),
        "severity must never improve then worsen again, got {ranks:?}"
    );
    // The critical also carries the top impact, so leading the list is
    // not on its own proof of a severity sort. Its 9 avoidable ops would
    // put it first under either key; the tail is what separates them,
    // the two impact-bearing infos rank below every zero-impact warning
    // here and above them under `--sort impact`.
    let impacts = finding_impacts(&payload);
    assert_eq!(impacts[0], 9, "the critical is also the heaviest finding");
    assert_eq!(
        &impacts[7..],
        &[0, 2, 2, 0],
        "a zero-impact warning must still outrank the infos that cost I/O"
    );
}

#[test]
fn cli_report_defaults_to_the_impact_order() {
    let payload = render_realistic(&[]);
    assert_eq!(
        finding_trace_ids(&payload),
        IMPACT_ORDER,
        "no --sort must rank on impact, the key the dashboard opens on"
    );
    assert_ne!(
        finding_trace_ids(&payload),
        SEVERITY_ORDER,
        "the default must be distinguishable from the severity ranking"
    );
    assert_eq!(
        payload["initial_sort"], "impact",
        "the dashboard must open on the key the payload was ranked by"
    );
}

#[test]
fn cli_report_max_traces_embedded_caps_the_span_trees_at_n() {
    for cap in 1..=3usize {
        let payload = render_realistic(&["--max-traces-embedded", &cap.to_string()]);
        let embedded: Vec<String> = payload["embedded_traces"]
            .as_array()
            .expect("embedded_traces")
            .iter()
            .map(|t| t["trace_id"].as_str().expect("trace_id").to_string())
            .collect();
        assert_eq!(embedded.len(), cap, "cap {cap} must be honored exactly");
        // The embed runs after the sort, so the trees kept are the ones
        // the top rows of the ranked list point at.
        assert_eq!(
            embedded,
            IMPACT_ORDER[..cap],
            "cap {cap} must keep the trees of the first {cap} ranked findings"
        );
        assert_eq!(payload["trimmed_traces"]["kept"], cap);
        assert_eq!(
            payload["trimmed_traces"]["total"], 6,
            "the fixture has six candidate traces to trim from"
        );
    }
}

#[test]
fn cli_report_keeps_the_trace_id_of_a_finding_whose_tree_was_dropped() {
    // The dashboard tells a reader whose span tree was trimmed to rerun
    // on the trace id shown right above the message, so every finding
    // must carry its own id even when its tree did not make the cap.
    let payload = render_realistic(&["--max-traces-embedded", "1"]);
    let embedded: Vec<&str> = payload["embedded_traces"]
        .as_array()
        .expect("embedded_traces")
        .iter()
        .map(|t| t["trace_id"].as_str().expect("trace_id"))
        .collect();
    assert_eq!(embedded, ["trace-notify-01"]);

    let traces = finding_trace_ids(&payload);
    assert_eq!(traces, IMPACT_ORDER, "trimming must not reorder the list");
    for trace_id in &traces[1..] {
        assert!(
            !trace_id.is_empty(),
            "a trimmed finding must still name its own trace"
        );
        assert!(
            !embedded.contains(&trace_id.as_str()),
            "{trace_id} was trimmed, so it must not be in embedded_traces"
        );
    }
}
