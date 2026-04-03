//! End-to-end integration tests for the perf-sentinel CLI.

use serde_json::Value;
use std::fs;
use std::io::Write;
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
fn cli_analyze_reads_from_stdin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("analyze")
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
        output.status.success(),
        "analyze from stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_analyze_reads_from_file() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("traces.json");
    fs::write(&file_path, b"[]").expect("failed to write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", file_path.to_str().unwrap(), "--ci"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "analyze from file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("findings"));
}

#[test]
fn cli_analyze_reads_fixture_with_findings() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "analyze failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("N+1 SQL"),
        "expected N+1 SQL finding in colored output"
    );
}

#[test]
fn cli_analyze_rejects_missing_file() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", "nonexistent.json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
}

#[test]
fn cli_help_shows_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("--help")
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("analyze"));
    assert!(stdout.contains("watch"));
    assert!(stdout.contains("demo"));
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
fn cli_analyze_rejects_oversized_file() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("huge.json");
    // Create a file slightly larger than the 1 MB default limit
    let data = vec![b'x'; 1_048_576 + 1];
    fs::write(&file_path, &data).expect("failed to write oversized file");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", file_path.to_str().unwrap()])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success(), "should reject oversized file");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("exceeds maximum"),
        "stderr should mention size limit, got: {stderr}"
    );
}

#[test]
fn cli_analyze_rejects_invalid_json() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("bad.json");
    fs::write(&file_path, b"not valid json {{{").expect("failed to write file");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", file_path.to_str().unwrap()])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success(), "should reject invalid JSON");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error ingesting"),
        "stderr should mention ingestion error, got: {stderr}"
    );
}

#[test]
fn cli_watch_starts_and_responds_to_sigterm() {
    use std::time::Duration;

    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("watch")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perf-sentinel watch");

    // Give it a moment to start the listeners
    std::thread::sleep(Duration::from_millis(500));

    // Kill the process (SIGTERM equivalent on Windows)
    child.kill().expect("failed to kill watch process");
    let output = child.wait_with_output().expect("failed to wait");

    // The process should have exited (killed)
    assert!(!output.status.success());
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
fn cli_analyze_detects_redundant_and_critical() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let file_path = dir.path().join("mixed_severity.json");

    // Build a fixture with:
    // - 10 N+1 SQL events (different params) -> Critical severity
    // - 3 redundant SQL events (same query) -> Info severity
    let mut events = Vec::new();
    for i in 1..=10 {
        events.push(serde_json::json!({
            "timestamp": format!("2025-07-10T14:32:01.{:03}Z", i * 10),
            "trace_id": "trace-crit",
            "span_id": format!("span-{i}"),
            "service": "game",
            "type": "sql",
            "operation": "SELECT",
            "target": format!("SELECT * FROM player WHERE game_id = {i}"),
            "duration_us": 800,
            "source": {
                "endpoint": "POST /api/game/42/start",
                "method": "GameService::start_game"
            }
        }));
    }
    for i in 1..=3 {
        events.push(serde_json::json!({
            "timestamp": format!("2025-07-10T14:32:02.{:03}Z", i * 50),
            "trace_id": "trace-dup",
            "span_id": format!("span-dup-{i}"),
            "service": "game",
            "type": "sql",
            "operation": "SELECT",
            "target": "SELECT * FROM config WHERE key = 'timeout'",
            "duration_us": 500,
            "source": {
                "endpoint": "GET /api/config",
                "method": "ConfigService::get"
            }
        }));
    }

    let json = serde_json::to_string(&events).expect("failed to serialize fixture");
    fs::write(&file_path, json).expect("failed to write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", file_path.to_str().unwrap(), "--ci"])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    // With --ci, the process may exit 1 if quality gate fails, but JSON is still on stdout
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the JSON output and verify finding types and severities
    let report: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("output should be valid JSON: {e}\nstdout: {stdout}"));
    let findings = report["findings"]
        .as_array()
        .expect("findings should be array");

    let has_critical = findings
        .iter()
        .any(|f| f["severity"] == "critical" && f["type"] == "n_plus_one_sql");
    let has_redundant = findings.iter().any(|f| f["type"] == "redundant_sql");

    assert!(has_critical, "should have critical N+1 SQL finding");
    assert!(has_redundant, "should have redundant SQL finding");

    // Verify green_impact is present on findings
    for finding in findings {
        assert!(
            finding.get("green_impact").is_some(),
            "each finding should have green_impact"
        );
    }

    // Verify top_offenders is populated
    let top_offenders = report["green_summary"]["top_offenders"]
        .as_array()
        .expect("top_offenders should be array");
    assert!(
        !top_offenders.is_empty(),
        "top_offenders should not be empty"
    );
}

#[test]
fn cli_analyze_ci_passes_clean() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/clean_traces.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--ci", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "clean traces should pass quality gate: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_analyze_ci_fails_on_violations() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--ci", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        !output.status.success(),
        "n_plus_one fixture should fail quality gate"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Quality gate FAILED"),
        "stderr should mention gate failure, got: {stderr}"
    );
}

#[test]
fn cli_analyze_without_ci_always_succeeds() {
    // Same fixture but without --ci flag: exit code should be 0 (colored output, no gate check)
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "without --ci flag, analyze should always succeed"
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
fn cli_analyze_slow_fixture_json_output() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/slow_queries.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--ci"])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(&stdout).expect("should be valid JSON");
    let findings = report["findings"].as_array().expect("findings array");

    let slow_sql = findings.iter().any(|f| f["type"] == "slow_sql");
    let slow_http = findings.iter().any(|f| f["type"] == "slow_http");
    assert!(slow_sql, "should detect slow_sql");
    assert!(slow_http, "should detect slow_http");

    // Verify severity of the critical slow SQL (2600ms > 5x500ms)
    let critical_slow = findings
        .iter()
        .find(|f| f["type"] == "slow_sql" && f["severity"] == "critical");
    assert!(
        critical_slow.is_some(),
        "slow SQL with 2600ms should be critical"
    );
}

#[test]
fn cli_analyze_with_config_region_shows_co2() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "[green]\nenabled = true\nregion = \"eu-west-3\"\n",
    )
    .expect("failed to write config");

    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "analyze",
            "--input",
            &fixture_path,
            "--config",
            config_path.to_str().unwrap(),
            "--ci",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    // With --ci, the process may exit 1 if quality gate fails, but JSON is still on stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(&stdout).expect("should be valid JSON");

    assert!(
        report["green_summary"]["estimated_co2_grams"].is_number(),
        "should have estimated_co2_grams in JSON output"
    );
    assert!(
        report["green_summary"]["avoidable_co2_grams"].is_number(),
        "should have avoidable_co2_grams in JSON output"
    );
}

#[test]
fn cli_analyze_invalid_config_explicit_path_fails() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--config", "nonexistent-config.toml"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success(), "should fail with missing config");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error reading config"),
        "stderr should mention config error, got: {stderr}"
    );
}

#[test]
fn cli_analyze_sarif_output() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "sarif"])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "sarif output should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sarif: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("sarif should be valid JSON: {e}\nstdout: {stdout}"));
    assert_eq!(sarif["version"], "2.1.0");
    assert!(!sarif["runs"].as_array().unwrap().is_empty());
}

#[test]
fn cli_analyze_jaeger_fixture() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/jaeger_export.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "Jaeger fixture analysis should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("N+1 SQL"),
        "should detect N+1 SQL from Jaeger"
    );
}

#[test]
fn cli_analyze_zipkin_fixture() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/zipkin_export.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "Zipkin fixture analysis should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("N+1 SQL"),
        "should detect N+1 SQL from Zipkin"
    );
}

#[test]
fn cli_analyze_fanout_fixture() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/fanout.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "fanout fixture analysis should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Excessive Fanout"),
        "should detect excessive fanout, got:\n{stdout}"
    );
}

#[test]
fn cli_explain_text_output() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            &fixture_path,
            "--trace-id",
            "trace-n1-sql",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "explain should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("trace-n1-sql"), "should show trace ID");
}

#[test]
fn cli_explain_json_output() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            &fixture_path,
            "--trace-id",
            "trace-n1-sql",
            "--format",
            "json",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "explain --format json should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tree: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("explain JSON should be valid: {e}\nstdout: {stdout}"));
    assert_eq!(tree["trace_id"], "trace-n1-sql");
}

#[test]
fn cli_explain_unknown_trace_id_fails() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "explain",
            "--input",
            &fixture_path,
            "--trace-id",
            "nonexistent-trace",
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        !output.status.success(),
        "should fail with unknown trace ID"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "should mention trace not found"
    );
}

#[test]
fn cli_help_shows_explain() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .arg("--help")
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("explain"),
        "help should list explain subcommand"
    );
}

#[test]
fn cli_analyze_malformed_config_explicit_path_fails() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let config_path = dir.path().join("bad-config.toml");
    fs::write(&config_path, "not valid toml {{{").expect("failed to write");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--config", config_path.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        !output.status.success(),
        "should fail with malformed config"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error parsing config"),
        "stderr should mention parse error, got: {stderr}"
    );
}
