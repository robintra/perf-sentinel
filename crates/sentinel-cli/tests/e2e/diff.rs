//! `diff` subcommand.

use serde_json::Value;
use std::fs;
use std::process::{Command, Stdio};

#[test]
fn cli_diff_identical_inputs_produce_empty_diff_in_json() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "diff",
            "--before",
            &fixture_path,
            "--after",
            &fixture_path,
            "--format",
            "json",
        ])
        .output()
        .expect("failed to execute perf-sentinel diff");

    assert!(
        output.status.success(),
        "diff exit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diff: Value = serde_json::from_slice(&output.stdout).expect("diff JSON should parse");
    assert!(
        diff.get("new_findings")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(false),
        "new_findings must be empty for identical inputs, got: {diff}"
    );
    assert!(
        diff.get("resolved_findings")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(false)
    );
    assert!(
        diff.get("severity_changes")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(false)
    );
    assert!(
        diff.get("endpoint_metric_deltas")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(false)
    );
}

#[test]
fn cli_diff_clean_to_n_plus_one_reports_new_finding_in_json() {
    let before = format!(
        "{}/../../tests/fixtures/clean_traces.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let after = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "diff", "--before", &before, "--after", &after, "--format", "json",
        ])
        .output()
        .expect("failed to execute perf-sentinel diff");

    assert!(output.status.success());
    let diff: Value = serde_json::from_slice(&output.stdout).unwrap();

    let new_findings = diff
        .get("new_findings")
        .and_then(Value::as_array)
        .expect("new_findings array");
    assert!(
        new_findings
            .iter()
            .any(|f| f.get("type").and_then(Value::as_str) == Some("n_plus_one_sql")),
        "expected at least one n_plus_one_sql finding in new_findings, got: {new_findings:?}"
    );

    let deltas = diff
        .get("endpoint_metric_deltas")
        .and_then(Value::as_array)
        .expect("endpoint_metric_deltas array");
    assert!(
        deltas.iter().any(|d| {
            d.get("endpoint").and_then(Value::as_str) == Some("POST /api/orders/42/submit")
                && d.get("delta").and_then(Value::as_i64) == Some(6)
        }),
        "expected a +6 delta on POST /api/orders/42/submit, got: {deltas:?}"
    );
}

#[test]
fn cli_diff_text_output_shows_summary_header() {
    let before = format!(
        "{}/../../tests/fixtures/clean_traces.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let after = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["diff", "--before", &before, "--after", &after])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel diff");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("=== perf-sentinel diff ==="),
        "text output should contain diff header, got:\n{stdout}"
    );
    assert!(
        stdout.contains("New findings"),
        "text output should contain 'New findings' section"
    );
}

#[test]
fn cli_diff_sarif_output_emits_only_new_findings() {
    let before = format!(
        "{}/../../tests/fixtures/clean_traces.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let after = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "diff", "--before", &before, "--after", &after, "--format", "sarif",
        ])
        .output()
        .expect("failed to execute perf-sentinel diff");

    assert!(output.status.success());
    let sarif: Value =
        serde_json::from_slice(&output.stdout).expect("SARIF output must be valid JSON");
    let results = sarif
        .pointer("/runs/0/results")
        .and_then(Value::as_array)
        .expect("SARIF runs[0].results must exist");
    assert!(
        !results.is_empty(),
        "SARIF results must contain at least one entry (the new finding)"
    );
    assert_eq!(
        results[0].pointer("/ruleId").and_then(Value::as_str),
        Some("n_plus_one_sql"),
        "SARIF result.ruleId should be the new finding's type"
    );
}

#[test]
fn cli_diff_writes_to_output_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out_path = dir.path().join("diff.json");
    let before = format!(
        "{}/../../tests/fixtures/clean_traces.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let after = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "diff",
            "--before",
            &before,
            "--after",
            &after,
            "--format",
            "json",
            "--output",
            out_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to execute perf-sentinel diff");

    assert!(output.status.success());
    assert!(out_path.exists(), "output file must have been created");
    let written = fs::read_to_string(&out_path).expect("read output file");
    let diff: Value = serde_json::from_str(&written).expect("output file must be valid JSON");
    assert!(diff.get("new_findings").is_some());
}
