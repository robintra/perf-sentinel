//! `demo` and `bench` subcommands.

use crate::helpers::extract_payload_json_from_html;
use serde_json::Value;
use std::fs;
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

#[test]
fn cli_demo_annotates_quality_gate_as_informational() {
    // The demo gate fails (io_waste_ratio) but the process exits 0, so the
    // FAILED line must carry the informational annotation, in plain text.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("demo")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "demo should exit 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "Quality gate: FAILED (informational in demo, would exit 1 under analyze --ci)"
        ),
        "demo gate line must be annotated as informational, got:\n{stdout}"
    );
}

#[test]
fn cli_demo_html_writes_dashboard() {
    // `demo --html <path>` writes the same HTML dashboard as `report`,
    // built from the embedded demo dataset, and exits 0.
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("demo.html");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["demo", "--html", out_path.to_str().unwrap()])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "demo --html should exit 0, got: {output:?}"
    );
    assert!(out_path.exists(), "HTML output must exist");
    let html = fs::read_to_string(&out_path).expect("read html");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<script id=\"report-data\""));
    let payload = extract_payload_json_from_html(&html);
    assert!(
        payload["report"]["findings"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "demo dashboard must embed findings"
    );
    // The demo is a showcase: synthesized correlations plus pg_stat and diff
    // fixtures must populate the Correlations, pg_stat and Diff tabs.
    assert!(
        payload["report"]["correlations"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "demo dashboard must embed synthesized correlations"
    );
    assert!(
        payload["pg_stat"]["rankings"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "demo dashboard must embed pg_stat rankings"
    );
    assert!(
        !payload["diff"]["new_findings"].is_null()
            && !payload["diff"]["resolved_findings"].is_null(),
        "demo dashboard must embed a diff against the baseline"
    );
}

#[test]
fn cli_demo_output_contains_green_impact() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("demo")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success(), "demo command failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Extra I/O:"),
        "demo should show green_impact Extra I/O"
    );
    assert!(stdout.contains("IIS:"), "demo should show green_impact IIS");
    assert!(
        stdout.contains("Top offenders:"),
        "demo should show top offenders section"
    );
}

#[test]
fn cli_help_shows_bench() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("--help")
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bench"),
        "help should list bench subcommand"
    );
}

#[test]
fn cli_bench_runs_on_fixture() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["bench", "--input", &fixture_path, "--iterations", "3"])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "bench should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("bench output should be valid JSON: {e}\nstdout: {stdout}"));

    assert_eq!(report["iterations"], 3);
    assert_eq!(report["events_per_iteration"], 6);
    assert!(
        report["throughput_events_per_sec"].as_f64().unwrap() > 0.0,
        "throughput should be positive"
    );
    assert!(
        report["latency_per_event_us"]["p50"].as_f64().unwrap() > 0.0,
        "p50 latency should be positive"
    );
    assert!(
        report["latency_per_event_us"]["p99"].as_f64().unwrap() > 0.0,
        "p99 latency should be positive"
    );
    assert!(
        report["total_elapsed_ms"].is_number(),
        "total_elapsed_ms should be present"
    );
}

#[test]
fn cli_bench_default_iterations() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/clean_traces.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["bench", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(&stdout).expect("should be valid JSON");
    assert_eq!(report["iterations"], 10, "default iterations should be 10");
}

#[test]
fn cli_demo_output_contains_slow_findings() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("demo")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success(), "demo command failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Slow SQL"),
        "demo should show Slow SQL finding, got:\n{stdout}"
    );
    assert!(
        stdout.contains("CRITICAL") || stdout.contains("WARNING"),
        "demo should show severity for slow finding"
    );
    assert!(
        stdout.contains("Consider adding an index"),
        "demo should show slow query suggestion"
    );
}

#[test]
fn cli_demo_shows_carbon_disclaimer() {
    // the GreenOps summary CLI output must include a one-line
    // disclaimer when CO₂ estimates are emitted.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["demo"])
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel demo");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The disclaimer is framed as 2x multiplicative uncertainty,
    // matching the constants: low = mid/2, high = mid*2.
    assert!(
        stdout.contains("multiplicative uncertainty"),
        "demo output should include CO2 disclaimer with new framing, got: {stdout}"
    );
    assert!(
        stdout.contains("docs/LIMITATIONS.md"),
        "disclaimer should reference LIMITATIONS.md, got: {stdout}"
    );
}
