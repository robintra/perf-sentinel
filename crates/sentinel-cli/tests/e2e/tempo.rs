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
        stderr.contains("--from/--to"),
        "stderr should name the window flags, got: {stderr}"
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
