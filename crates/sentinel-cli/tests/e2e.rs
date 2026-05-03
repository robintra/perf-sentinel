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
    assert_eq!(stdout.contains("watch"), cfg!(feature = "daemon"));
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
    let config_path = dir.path().join("perf-sentinel.toml");
    // Pin a small payload limit via config so the test stays cheap.
    // Default bumped to 16 MiB in 0.5.13, writing a 16 MiB file just
    // to trip the guard would balloon the test fixture.
    fs::write(&config_path, "[daemon]\nmax_payload_size = 1048576\n")
        .expect("failed to write config");
    let file_path = dir.path().join("huge.json");
    let data = vec![b'x'; 1_048_576 + 1];
    fs::write(&file_path, &data).expect("failed to write oversized file");

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "analyze",
            "--config",
            config_path.to_str().unwrap(),
            "--input",
            file_path.to_str().unwrap(),
        ])
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
fn cli_watch_help_documents_listen_address_override() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["watch", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success(), "watch --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--listen-address"),
        "watch --help should advertise --listen-address, got: {stdout}"
    );
    assert!(
        stdout.contains("--listen-port-http"),
        "watch --help should advertise --listen-port-http, got: {stdout}"
    );
    assert!(
        stdout.contains("--listen-port-grpc"),
        "watch --help should advertise --listen-port-grpc, got: {stdout}"
    );
}

#[test]
fn cli_watch_listen_address_override_starts_cleanly() {
    use std::time::Duration;

    // Use non-default ports to avoid collisions with a local daemon and
    // verify the override path is wired end-to-end (invalid ports or a
    // parse failure would exit before the sleep expires).
    let mut child = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "watch",
            "--listen-address",
            "127.0.0.1",
            "--listen-port-http",
            "14318",
            "--listen-port-grpc",
            "14317",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perf-sentinel watch");

    std::thread::sleep(Duration::from_millis(500));
    let still_running = child.try_wait().expect("try_wait failed").is_none();
    child.kill().expect("failed to kill watch process");
    let _ = child.wait_with_output();
    assert!(
        still_running,
        "daemon should still be running after overrides; likely a CLI parse or validation error"
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
            "service": "order-svc",
            "type": "sql",
            "operation": "SELECT",
            "target": format!("SELECT * FROM order_item WHERE order_id = {i}"),
            "duration_us": 800,
            "source": {
                "endpoint": "POST /api/orders/42/submit",
                "method": "OrderService::create_order"
            }
        }));
    }
    for i in 1..=3 {
        events.push(serde_json::json!({
            "timestamp": format!("2025-07-10T14:32:02.{:03}Z", i * 50),
            "trace_id": "trace-dup",
            "span_id": format!("span-dup-{i}"),
            "service": "order-svc",
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
fn cli_analyze_text_shows_severity_breakdown() {
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

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Found 1 issue(s):"),
        "header missing, got:\n{stdout}"
    );
    assert!(
        stdout.contains("critical") && stdout.contains("warning") && stdout.contains("info"),
        "severity breakdown line missing, got:\n{stdout}"
    );
}

#[test]
fn cli_analyze_help_documents_correlations_are_daemon_only() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Cross-trace correlations"),
        "analyze --help must mention correlations, got:\n{stdout}"
    );
    assert!(
        stdout.contains("perf-sentinel watch")
            && stdout.contains("perf-sentinel query correlations"),
        "analyze --help must point at watch + query correlations, got:\n{stdout}"
    );
}

#[cfg(feature = "tui")]
#[test]
fn cli_inspect_help_documents_report_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["inspect", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Report JSON"),
        "inspect --help must mention Report JSON input, got:\n{stdout}"
    );
    assert!(
        stdout.contains("/api/export/report"),
        "inspect --help must reference the daemon snapshot endpoint, got:\n{stdout}"
    );
}

#[test]
fn cli_analyze_ci_text_lists_quality_gate_rules() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "analyze",
            "--ci",
            "--format",
            "text",
            "--input",
            &fixture_path,
        ])
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Quality gate:"),
        "missing quality gate header, got:\n{stdout}"
    );
    assert!(
        stdout.contains("PASS") || stdout.contains("FAIL"),
        "quality gate must list at least one rule outcome (PASS or FAIL), got:\n{stdout}"
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
    // disable hourly profiles so this test still asserts the
    // model tag (`io_proxy_v1`). The hourly path is
    // exercised by dedicated tests in score/mod.rs and by e2e
    // `cli_analyze_hourly_profiles_upgrades_model_tag` below.
    fs::write(
        &config_path,
        "[green]\nenabled = true\ndefault_region = \"eu-west-3\"\nuse_hourly_profiles = false\n",
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

    // structured `co2` object with 2× multiplicative uncertainty
    // interval and SCI methodology tags (review fix: sci_version → methodology
    // with distinct values for total vs avoidable).
    let co2 = &report["green_summary"]["co2"];
    assert!(co2.is_object(), "co2 should be a structured object");
    assert!(co2["total"]["mid"].is_number());
    assert!(co2["total"]["low"].is_number());
    assert!(co2["total"]["high"].is_number());
    assert_eq!(co2["total"]["model"], "io_proxy_v1");
    assert_eq!(co2["total"]["methodology"], "sci_v1_numerator");
    assert_eq!(co2["avoidable"]["methodology"], "sci_v1_operational_ratio");
    assert!(co2["operational_gco2"].is_number());
    assert!(co2["embodied_gco2"].is_number());

    // Per-region breakdown.
    let regions = &report["green_summary"]["regions"];
    assert!(regions.is_array());
    assert!(!regions.as_array().unwrap().is_empty());
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
    // review fix: disclaimer reframed as 2× multiplicative
    // uncertainty (matches the constants: low = mid/2, high = mid×2).
    assert!(
        stdout.contains("multiplicative uncertainty"),
        "demo output should include CO2 disclaimer with new framing, got: {stdout}"
    );
    assert!(
        stdout.contains("docs/LIMITATIONS.md"),
        "disclaimer should reference LIMITATIONS.md, got: {stdout}"
    );
}

#[test]
fn cli_analyze_no_disclaimer_when_green_disabled() {
    // When green scoring is disabled, the CLI must NOT print the CO₂
    // disclaimer (no estimates are produced).
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "[green]\nenabled = false\n").expect("failed to write config");

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
        ])
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("multiplicative uncertainty"),
        "disclaimer should be absent when green is disabled, got: {stdout}"
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
        stdout.contains("Excessive fanout"),
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

#[test]
fn cli_analyze_emits_suggested_fix_for_jpa_n_plus_one() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql_java_jpa.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "analyze failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("analyze stdout should be valid JSON");

    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .expect("report should contain findings array");
    let n1 = findings
        .iter()
        .find(|f| f.get("type").and_then(Value::as_str) == Some("n_plus_one_sql"))
        .expect("expected an n_plus_one_sql finding");

    let fix = n1
        .get("suggested_fix")
        .expect("n_plus_one_sql finding should carry a suggested_fix");
    assert_eq!(
        fix.get("framework").and_then(Value::as_str),
        Some("java_jpa"),
        "framework should be java_jpa, got: {fix}"
    );
    assert_eq!(
        fix.get("pattern").and_then(Value::as_str),
        Some("n_plus_one_sql"),
    );
    let recommendation = fix
        .get("recommendation")
        .and_then(Value::as_str)
        .expect("recommendation should be a string");
    assert!(
        recommendation.contains("JOIN FETCH") || recommendation.contains("EntityGraph"),
        "JPA recommendation should mention JOIN FETCH or EntityGraph, got: {recommendation}"
    );
}

#[test]
fn cli_analyze_omits_suggested_fix_for_non_java_finding() {
    // The plain n_plus_one_sql.json fixture has no code attributes.
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .expect("report should contain findings array");
    for f in findings {
        assert!(
            f.get("suggested_fix").is_none(),
            "no finding should carry suggested_fix when code_location is absent, got: {f}"
        );
    }
}

#[test]
fn cli_analyze_text_output_shows_suggested_fix_line() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql_java_jpa.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Suggested fix:"),
        "text output should contain a 'Suggested fix:' line, got:\n{stdout}"
    );
}

#[test]
fn cli_analyze_sarif_output_includes_fixes_array() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql_java_jpa.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "sarif"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let sarif: Value =
        serde_json::from_slice(&output.stdout).expect("SARIF output should be valid JSON");

    let result = sarif
        .pointer("/runs/0/results/0")
        .expect("SARIF should have at least one result");
    let fixes = result
        .get("fixes")
        .and_then(Value::as_array)
        .expect("result should have a fixes array when suggested_fix is present");
    assert!(!fixes.is_empty(), "fixes array should not be empty");
    let text = fixes[0]
        .pointer("/description/text")
        .and_then(Value::as_str)
        .expect("fixes[0].description.text must be a string");
    assert!(
        text.contains("JOIN FETCH") || text.contains("EntityGraph"),
        "SARIF fix text should carry the JPA recommendation, got: {text}"
    );
}

#[test]
fn cli_analyze_emits_suggested_fix_for_csharp_ef_core_n_plus_one() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql_csharp_ef_core.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let n1 = report
        .get("findings")
        .and_then(Value::as_array)
        .and_then(|fs| {
            fs.iter()
                .find(|f| f.get("type").and_then(Value::as_str) == Some("n_plus_one_sql"))
        })
        .expect("expected an n_plus_one_sql finding");

    let fix = n1
        .get("suggested_fix")
        .expect("EF Core finding should carry a suggested_fix");
    assert_eq!(
        fix.get("framework").and_then(Value::as_str),
        Some("csharp_ef_core"),
        "framework should be csharp_ef_core, got: {fix}"
    );
    let recommendation = fix.get("recommendation").and_then(Value::as_str).unwrap();
    assert!(
        recommendation.contains(".Include()") || recommendation.contains("AsSplitQuery"),
        "EF Core recommendation should mention .Include() or AsSplitQuery, got: {recommendation}"
    );
}

#[test]
fn cli_analyze_emits_suggested_fix_for_rust_diesel_n_plus_one() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql_rust_diesel.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let n1 = report
        .get("findings")
        .and_then(Value::as_array)
        .and_then(|fs| {
            fs.iter()
                .find(|f| f.get("type").and_then(Value::as_str) == Some("n_plus_one_sql"))
        })
        .expect("expected an n_plus_one_sql finding");

    let fix = n1
        .get("suggested_fix")
        .expect("Diesel finding should carry a suggested_fix");
    assert_eq!(
        fix.get("framework").and_then(Value::as_str),
        Some("rust_diesel"),
        "framework should be rust_diesel, got: {fix}"
    );
    let recommendation = fix.get("recommendation").and_then(Value::as_str).unwrap();
    assert!(
        recommendation.contains("belonging_to") || recommendation.contains("inner_join"),
        "Diesel recommendation should mention belonging_to or inner_join, got: {recommendation}"
    );
}

#[test]
fn cli_analyze_emits_suggested_fix_for_quarkus_non_reactive_n_plus_one() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/n_plus_one_sql_java_quarkus.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["analyze", "--input", &fixture_path, "--format", "json"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let n1 = report
        .get("findings")
        .and_then(Value::as_array)
        .and_then(|fs| {
            fs.iter()
                .find(|f| f.get("type").and_then(Value::as_str) == Some("n_plus_one_sql"))
        })
        .expect("expected an n_plus_one_sql finding");

    let fix = n1
        .get("suggested_fix")
        .expect("Quarkus non-reactive finding should carry a suggested_fix");
    assert_eq!(
        fix.get("framework").and_then(Value::as_str),
        Some("java_quarkus"),
        "framework should be java_quarkus (non-reactive), not java_quarkus_reactive, got: {fix}"
    );
    let recommendation = fix.get("recommendation").and_then(Value::as_str).unwrap();
    assert!(
        recommendation.contains("JOIN FETCH") || recommendation.contains("@EntityGraph"),
        "Quarkus non-reactive recommendation should mention JOIN FETCH or @EntityGraph, got: {recommendation}"
    );
}

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

// ---------------------------------------------------------------------
// `report` subcommand: HTML dashboard output.
// ---------------------------------------------------------------------

/// Extract the embedded JSON payload from a rendered HTML dashboard.
/// Mirrors the test helper in `report::html::tests::extract_payload_json`
/// but lives here so the e2e tier does not reach into the core crate.
fn extract_payload_json_from_html(html: &str) -> Value {
    let tag = "<script id=\"report-data\"";
    let start = html.find(tag).expect("report-data script tag present");
    let open = html[start..].find('>').expect("script open") + 1;
    let rest = &html[start + open..];
    let end = rest.find("</script>").expect("script close");
    let blob = rest[..end].trim().replace("<\\/", "</");
    serde_json::from_str(&blob).expect("payload parses as JSON")
}

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
    assert_eq!(
        payload["report"]["findings"].as_array().unwrap().len(),
        2,
        "minimal fixture yields exactly 2 findings"
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
    // Closes the loop for the Correlations tab. A daemon produces a
    // `Report` that carries `correlations`; the `report` subcommand
    // must deserialize it and make the Correlations tab visible in
    // the HTML output. This test does not spawn a live daemon (OTLP
    // ingestion is expensive to craft in a unit-test context); it
    // constructs the daemon-shape JSON directly and pipes it through
    // `perf-sentinel report --input -`.
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
                "grid_intensity_gco2_kwh": 56.0,
                "pue": 1.2,
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

// ── Acknowledgments (0.5.17) ─────────────────────────────────────────

const ACK_FIXTURE: &str = "../../tests/fixtures/n_plus_one_sql.json";

/// Fixture path expanded against `CARGO_MANIFEST_DIR` (the CLI crate dir).
fn fixture_path(rel: &str) -> String {
    format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel)
}

/// Run `analyze --format json` and parse the output, exiting the test
/// (via panic) on failure with the captured stderr in the message.
fn analyze_json(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(args)
        .output()
        .expect("spawn perf-sentinel");
    assert!(
        output.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "analyze stdout is not valid JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Run analyze on the fixture, JSON output, no acknowledgments file in
/// play. Read the first finding's signature so each test can wire up
/// matching ack entries without hard-coding the SHA-256 prefix.
fn first_finding_signature() -> String {
    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    v["findings"][0]["signature"]
        .as_str()
        .expect("signature field present in JSON output")
        .to_string()
}

#[test]
fn cli_analyze_signature_emitted_in_json() {
    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    let findings = v["findings"].as_array().expect("findings array");
    assert!(!findings.is_empty(), "fixture must produce findings");
    for f in findings {
        let sig = f["signature"].as_str().expect("signature field present");
        assert!(!sig.is_empty(), "signature must be non-empty");
        assert_eq!(
            sig.matches(':').count(),
            3,
            "signature must have 4 colon-separated segments: {sig}"
        );
    }
}

fn write_ack_file(dir: &std::path::Path, signature: &str) -> std::path::PathBuf {
    let path = dir.join(".perf-sentinel-acknowledgments.toml");
    fs::write(
        &path,
        format!(
            "[[acknowledged]]\n\
             signature = \"{signature}\"\n\
             acknowledged_by = \"test@example.com\"\n\
             acknowledged_at = \"2026-05-02\"\n\
             reason = \"smoke test\"\n",
        ),
    )
    .expect("write ack file");
    path
}

#[test]
fn cli_analyze_with_acks_filters_output() {
    let sig = first_finding_signature();
    let dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(dir.path(), &sig);

    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--acknowledgments",
        ack_path.to_str().unwrap(),
        "--format",
        "json",
    ]);
    let signatures: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["signature"].as_str())
        .collect();
    assert!(
        !signatures.contains(&sig.as_str()),
        "acked finding must be absent: signatures={signatures:?}"
    );
    // Without --show-acknowledged, the wire payload must omit the
    // acknowledged_findings array entirely (skip_serializing_if).
    assert!(
        v.get("acknowledged_findings").is_none(),
        "acknowledged_findings must be hidden by default: payload={v}"
    );
}

#[test]
fn cli_analyze_no_acknowledgments_flag_disables() {
    let sig = first_finding_signature();
    let dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(dir.path(), &sig);

    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--acknowledgments",
        ack_path.to_str().unwrap(),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    let signatures: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["signature"].as_str())
        .collect();
    assert!(
        signatures.contains(&sig.as_str()),
        "--no-acknowledgments must surface the finding: signatures={signatures:?}"
    );
}

#[test]
fn cli_analyze_show_acknowledged_includes_in_output() {
    let sig = first_finding_signature();
    let dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(dir.path(), &sig);

    let v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--acknowledgments",
        ack_path.to_str().unwrap(),
        "--show-acknowledged",
        "--format",
        "json",
    ]);
    let acked = v["acknowledged_findings"]
        .as_array()
        .expect("acknowledged_findings present with --show-acknowledged");
    assert_eq!(acked.len(), 1, "exactly one ack matched");
    assert_eq!(
        acked[0]["finding"]["signature"].as_str(),
        Some(sig.as_str()),
        "ack finding signature roundtrips"
    );
    assert_eq!(
        acked[0]["acknowledgment"]["acknowledged_by"].as_str(),
        Some("test@example.com"),
        "ack metadata is preserved"
    );
}

#[test]
fn cli_analyze_acknowledgments_path_override() {
    // Two distinct dirs: cwd-side has no ack file, override path holds the ack.
    let sig = first_finding_signature();
    let cwd_dir = tempfile::tempdir().unwrap();
    let ack_dir = tempfile::tempdir().unwrap();
    let ack_path = write_ack_file(ack_dir.path(), &sig);

    // Run analyze from the cwd-side directory so the default lookup
    // would find no ack file. The --acknowledgments override must still
    // pick up the ack from `ack_dir`.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .current_dir(cwd_dir.path())
        .args([
            "analyze",
            "--input",
            &fixture_path(ACK_FIXTURE),
            "--acknowledgments",
            ack_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("spawn perf-sentinel");
    assert!(
        output.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    let signatures: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["signature"].as_str())
        .collect();
    assert!(
        !signatures.contains(&sig.as_str()),
        "override path must apply the ack from outside cwd"
    );
}

#[test]
fn cli_analyze_no_ack_file_is_no_op() {
    // The default behavior must be a no-op when no ack file exists in
    // the cwd: zero error, all findings preserved.
    let cwd_dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .current_dir(cwd_dir.path())
        .args([
            "analyze",
            "--input",
            &fixture_path(ACK_FIXTURE),
            "--format",
            "json",
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "missing ack file must be a clean no-op, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = v["findings"].as_array().expect("findings array");
    assert!(!findings.is_empty(), "fixture must produce findings");
    assert!(
        v.get("acknowledged_findings").is_none(),
        "no-op path must omit acknowledged_findings entirely"
    );
}

#[test]
fn cli_signature_consistent_across_json_and_sarif() {
    let json_v = analyze_json(&[
        "analyze",
        "--input",
        &fixture_path(ACK_FIXTURE),
        "--no-acknowledgments",
        "--format",
        "json",
    ]);
    let json_findings = json_v["findings"].as_array().expect("findings array");
    assert!(
        !json_findings.is_empty(),
        "fixture must produce at least one finding"
    );

    let mut json_signatures: std::collections::HashMap<(String, String, String), String> =
        std::collections::HashMap::new();
    for f in json_findings {
        let key = (
            f["type"].as_str().unwrap().to_string(),
            f["service"].as_str().unwrap().to_string(),
            f["source_endpoint"].as_str().unwrap().to_string(),
        );
        let sig = f["signature"]
            .as_str()
            .expect("signature in JSON output")
            .to_string();
        assert!(!sig.is_empty(), "signature must be non-empty");
        json_signatures.insert(key, sig);
    }

    let sarif_output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "analyze",
            "--input",
            &fixture_path(ACK_FIXTURE),
            "--no-acknowledgments",
            "--format",
            "sarif",
        ])
        .output()
        .expect("spawn perf-sentinel");
    assert!(
        sarif_output.status.success(),
        "sarif analyze failed: {}",
        String::from_utf8_lossy(&sarif_output.stderr)
    );
    let sarif: Value =
        serde_json::from_slice(&sarif_output.stdout).expect("sarif stdout must be valid JSON");

    let results = sarif["runs"][0]["results"]
        .as_array()
        .expect("sarif results array");
    assert_eq!(
        results.len(),
        json_findings.len(),
        "JSON and SARIF must emit the same number of results"
    );

    for r in results {
        let key = (
            r["ruleId"].as_str().unwrap().to_string(),
            r["logicalLocations"][0]["name"]
                .as_str()
                .unwrap()
                .to_string(),
            r["logicalLocations"][1]["name"]
                .as_str()
                .unwrap()
                .to_string(),
        );
        let expected_sig = json_signatures
            .get(&key)
            .unwrap_or_else(|| panic!("no JSON match for SARIF key {key:?}"));
        let props_sig = r["properties"]["signature"]
            .as_str()
            .unwrap_or_else(|| panic!("SARIF result missing properties.signature for {key:?}"));
        let fp_sig = r["fingerprints"]["perfsentinel/v1"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("SARIF result missing fingerprints[perfsentinel/v1] for {key:?}")
            });
        assert_eq!(
            props_sig, expected_sig,
            "properties.signature must match JSON signature for {key:?}"
        );
        assert_eq!(
            fp_sig, expected_sig,
            "fingerprints[perfsentinel/v1] must match JSON signature for {key:?}"
        );
    }
}
