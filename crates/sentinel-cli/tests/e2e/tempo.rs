//! `tempo` subcommand: exit-code contract for local argument-validation
//! and fetch failures, which don't need a live Tempo backend to trigger.

#![cfg(feature = "tempo")]

use std::process::Command;

#[test]
fn cli_tempo_missing_trace_id_and_service_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["tempo", "--endpoint", "http://127.0.0.1:1"])
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
fn cli_tempo_invalid_lookback_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_fetch_failure_exits_tooling_error() {
    // Port 1 is a privileged port nothing listens on; the fetch fails
    // fast with a connection error, no live Tempo backend needed.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
        stderr.contains("Error fetching traces from Tempo"),
        "stderr should name the fetch failure, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_absolute_window_conflicts_with_lookback() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_absolute_window_conflicts_with_trace_id() {
    // A trace ID resolves to exactly one trace, so a window would be read
    // and silently dropped rather than applied.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_from_requires_to() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_inverted_absolute_window_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
    // The window is refused before any request is issued, so this is an
    // invocation error like an unparsable --lookback, not a fetch failure.
    assert_eq!(
        output.status.code(),
        Some(75),
        "an inverted window must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_absolute_window_reaches_the_fetch() {
    // Nothing listens on port 1, so getting as far as a connection error
    // proves the window parsed and the request was actually issued.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--from",
            "2026-08-20T15:00:00Z",
            "--to",
            "2026-08-20T16:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error fetching traces from Tempo"),
        "an absolute window must reach the fetch, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_rejects_max_traces_past_the_shared_ceiling() {
    // Parse-time, no server involved: the bound is the client's, shared
    // with jaeger-query, and must refuse before any request is issued.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_rejects_zero_max_traces() {
    // The bottom of the same range: a search for zero traces is a
    // typo, refused before the subcommand runs.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_rejects_max_traces_one_past_the_ceiling() {
    // 10001 is the first refused value, the boundary a far-out figure
    // like 999999 never pins down.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
fn cli_tempo_max_traces_at_the_ceiling_reaches_the_fetch() {
    // 10000 is the last accepted value. Nothing listens on port 1, so
    // failing on the connection proves the validator let it through.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
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
        stderr.contains("Error fetching traces from Tempo"),
        "an in-range --max-traces must reach the fetch, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not a clap usage error"
    );
}

#[test]
fn cli_tempo_help_mentions_sort() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["tempo", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--sort"), "help mentions --sort");
}
