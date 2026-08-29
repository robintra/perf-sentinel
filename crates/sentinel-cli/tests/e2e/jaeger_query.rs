//! `jaeger-query` subcommand: exit-code contract for local
//! argument-validation failures. Mirrors tempo.rs, same shared shape.

#![cfg(feature = "jaeger-query")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

use crate::helpers::fixture_path;

const JAEGER_EXPORT_FIXTURE: &str = "../../tests/fixtures/jaeger_export.json";

/// Serve one canned HTTP/1.1 200 JSON response on an ephemeral port,
/// then stop. The Jaeger query API bundles whole spans into the search
/// response, so a single reply covers the one request the subcommand
/// issues. Returns the port.
fn spawn_one_shot_json(body: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        // Drain the request line and headers, otherwise the client can
        // see a reset before it reads the response.
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
            line.clear();
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });

    port
}

#[test]
fn cli_jaeger_query_missing_trace_id_and_service_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["jaeger-query", "--endpoint", "http://127.0.0.1:1"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--trace-id or --service"),
        "stderr should name the missing flag, got: {stderr}"
    );
    // Argument validation, never a quality-gate breach. See docs/CI.md
    // "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(75),
        "bad invocation must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_jaeger_query_invalid_lookback_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--lookback",
            "not-a-duration",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(75),
        "unparsable --lookback must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_jaeger_query_fetch_failure_exits_tooling_error() {
    // Port 1 is a privileged port nothing listens on; the fetch fails
    // fast with a connection error, no live Jaeger backend needed.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error fetching traces from Jaeger query API"),
        "stderr should name the fetch failure, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_jaeger_query_absolute_window_conflicts_with_lookback() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--lookback",
            "2h",
            "--from",
            "2026-08-20T15:00:00Z",
            "--to",
            "2026-08-20T16:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--lookback") && stderr.contains("cannot be used with"),
        "stderr should name the conflict, got: {stderr}"
    );
}

#[test]
fn cli_jaeger_query_absolute_window_conflicts_with_trace_id() {
    // A trace ID resolves to exactly one trace, so a window would be read
    // and silently dropped rather than applied.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--trace-id",
            "abc123def456",
            "--from",
            "2026-08-20T15:00:00Z",
            "--to",
            "2026-08-20T16:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "stderr should name the conflict, got: {stderr}"
    );
}

#[test]
fn cli_jaeger_query_from_requires_to() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--from",
            "2026-08-20T15:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--to"),
        "stderr should name the missing half of the pair, got: {stderr}"
    );
}

#[test]
fn cli_jaeger_query_inverted_absolute_window_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--from",
            "2026-08-20T16:00:00Z",
            "--to",
            "2026-08-20T15:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--from 2026-08-20T16:00:00Z --to 2026-08-20T15:00:00Z"),
        "stderr should echo the window the operator typed, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "an inverted window must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_jaeger_query_rejects_max_traces_past_the_shared_ceiling() {
    // Parse-time twin of the tempo test: the two subcommands read the
    // same constant and must refuse the same way.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "999999",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(output.status.code(), Some(2), "clap usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1..=10000"),
        "the refusal names the range, got: {stderr}"
    );
}

#[test]
fn cli_jaeger_query_rejects_zero_max_traces() {
    // The bottom of the same range, refused the same way in both
    // subcommands.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "0",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(output.status.code(), Some(2), "clap usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--max-traces") && stderr.contains("1..=10000"),
        "the refusal names the flag and the range, got: {stderr}"
    );
}

#[test]
fn cli_jaeger_query_rejects_max_traces_one_past_the_ceiling() {
    // 10001 is the first refused value, the boundary a far-out figure
    // like 999999 never pins down.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "10001",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(output.status.code(), Some(2), "clap usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--max-traces") && stderr.contains("1..=10000"),
        "the refusal names the flag and the range, got: {stderr}"
    );
}

#[test]
fn cli_jaeger_query_max_traces_at_the_ceiling_reaches_the_fetch() {
    // 10000 is the last accepted value. Nothing listens on port 1, so
    // failing on the connection proves the validator let it through.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "10000",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error fetching traces from Jaeger query API"),
        "an in-range --max-traces must reach the fetch, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not a clap usage error"
    );
}

#[test]
fn cli_jaeger_query_json_carries_the_findings_spans() {
    // The backend-query JSON travels without its input, so it has to
    // carry the spans of the traces its findings point at. The tempo file
    // pins the same seam, over a stub that serves both of its hops.
    let body = std::fs::read_to_string(fixture_path(JAEGER_EXPORT_FIXTURE)).expect("read fixture");
    let port = spawn_one_shot_json(body);

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "jaeger-query",
            "--endpoint",
            &format!("http://127.0.0.1:{port}"),
            "--service",
            "order-svc",
            "--no-acknowledgments",
            "--format",
            "json",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "the stubbed search must analyze cleanly, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout parses as JSON");
    assert!(
        !report["findings"]
            .as_array()
            .expect("findings array")
            .is_empty(),
        "the fixture must yield a finding for a trace to be embedded"
    );
    let embedded = report["embedded_traces"]
        .as_array()
        .expect("embedded_traces array present");
    assert_eq!(
        embedded.len(),
        1,
        "the one trace the findings point at travels with the report"
    );
    assert_eq!(embedded[0]["trace_id"], "trace-jaeger-1");
    assert!(
        !embedded[0]["spans"]
            .as_array()
            .expect("spans array")
            .is_empty(),
        "an embedded trace without spans draws nothing"
    );
}

#[test]
fn cli_jaeger_query_help_mentions_sort() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["jaeger-query", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--sort"), "help mentions --sort");
}
