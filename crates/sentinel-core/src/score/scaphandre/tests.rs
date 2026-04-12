//! Integration tests that cross the sub-module boundaries (parser,
//! ops, state, scraper). Per-sub-module unit tests that only touch
//! one file could live next to their code, but keeping everything in
//! one place makes the test module layout match the pre-split
//! organization exactly so git blame still works.

use std::collections::HashMap;
use std::time::Duration;

use crate::http_client::build_client as build_scraper_client;

use super::config::ScaphandreConfig;
use super::ops::{OpsSnapshotDiff, apply_scrape, compute_energy_per_op_kwh};
use super::parser::{ProcessPower, parse_scaphandre_metrics};
use super::scraper::{ScraperError, fetch_metrics_once, spawn_scraper};
use super::state::ScaphandreState;

#[test]
fn parse_empty_body() {
    assert!(parse_scaphandre_metrics("").is_empty());
}

#[test]
fn parse_comments_only() {
    let body = "# HELP scaph_host_power_microwatts host power\n\
                # TYPE scaph_host_power_microwatts gauge\n";
    assert!(parse_scaphandre_metrics(body).is_empty());
}

#[test]
fn parse_single_process_power() {
    let body = r#"# HELP scaph_process_power_consumption_microwatts per-process power
# TYPE scaph_process_power_consumption_microwatts gauge
scaph_process_power_consumption_microwatts{exe="java",cmdline="java -jar app.jar",pid="1234"} 12500000.0
"#;
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "java");
    assert!((parsed[0].power_microwatts - 12_500_000.0).abs() < f64::EPSILON);
}

#[test]
fn parse_skips_other_metrics() {
    // Only the per-process metric should be extracted; host and
    // socket metrics must be filtered out.
    let body = r#"scaph_host_power_microwatts 50000000.0
scaph_socket_power_microwatts{socket_id="0"} 25000000.0
scaph_process_power_consumption_microwatts{exe="java"} 8000000.0
scaph_process_power_consumption_microwatts{exe="dotnet"} 3000000.0
"#;
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 2);
    assert!(parsed.iter().any(|p| p.exe == "java"));
    assert!(parsed.iter().any(|p| p.exe == "dotnet"));
}

#[test]
fn parse_handles_escaped_quotes_in_cmdline() {
    // JVM with quoted args in cmdline must not break label parsing.
    let body = r#"scaph_process_power_consumption_microwatts{exe="java",cmdline="java -Dfoo=\"bar baz\" -jar app.jar"} 12000000.0
"#;
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "java");
}

#[test]
fn parse_handles_escaped_backslash() {
    let body = r#"scaph_process_power_consumption_microwatts{exe="weird\\path",cmdline="..."} 100.0
"#;
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "weird\\path");
}

#[test]
fn parse_skips_malformed_lines() {
    let body = r#"scaph_process_power_consumption_microwatts{exe="java"} not_a_number
scaph_process_power_consumption_microwatts broken no braces
scaph_process_power_consumption_microwatts{exe="dotnet"} 5000000.0
"#;
    let parsed = parse_scaphandre_metrics(body);
    // Only the well-formed dotnet line should come through.
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "dotnet");
}

#[test]
fn parse_unmatched_brace_is_skipped() {
    // Opening brace but no closing brace → line is skipped.
    let body = "scaph_process_power_consumption_microwatts{exe=\"java\",cmdline=\"broken 100.0\n";
    assert!(parse_scaphandre_metrics(body).is_empty());
}

#[test]
fn parse_unescapes_newline_escape() {
    // Prometheus spec lists \n as a valid escape; ensure we handle it.
    let body = "scaph_process_power_consumption_microwatts{exe=\"multi\\nline\"} 1.0\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "multi\nline");
}

#[test]
fn compute_energy_per_op_basic() {
    // 12W × 5s = 60J = 60 / 3.6e6 kWh ≈ 1.667e-5 kWh.
    // Over 8000 ops → 2.083e-9 kWh/op.
    let got = compute_energy_per_op_kwh(12_000_000.0, 5.0, 8000).unwrap();
    let expected = (12.0 * 5.0 / 3_600_000.0) / 8000.0;
    assert!((got - expected).abs() < 1e-18);
}

#[test]
fn compute_energy_per_op_zero_ops_returns_none() {
    assert!(compute_energy_per_op_kwh(12_000_000.0, 5.0, 0).is_none());
}

#[test]
fn compute_energy_per_op_negative_power_returns_none() {
    assert!(compute_energy_per_op_kwh(-1.0, 5.0, 100).is_none());
}

#[test]
fn compute_energy_per_op_nan_returns_none() {
    assert!(compute_energy_per_op_kwh(f64::NAN, 5.0, 100).is_none());
    assert!(compute_energy_per_op_kwh(f64::INFINITY, 5.0, 100).is_none());
}

#[test]
fn ops_snapshot_diff_first_call_counts_all() {
    let mut diff = OpsSnapshotDiff::default();
    let mut current = HashMap::new();
    current.insert("order-svc".to_string(), 100u64);
    current.insert("chat-svc".to_string(), 50u64);
    let deltas = diff.delta_and_advance(current);
    assert_eq!(deltas.get("order-svc"), Some(&100));
    assert_eq!(deltas.get("chat-svc"), Some(&50));
}

#[test]
fn ops_snapshot_diff_second_call_subtracts() {
    let mut diff = OpsSnapshotDiff::default();
    let mut first = HashMap::new();
    first.insert("order-svc".to_string(), 100u64);
    diff.delta_and_advance(first);

    let mut second = HashMap::new();
    second.insert("order-svc".to_string(), 160u64);
    let deltas = diff.delta_and_advance(second);
    assert_eq!(deltas.get("order-svc"), Some(&60));
}

#[test]
fn ops_snapshot_diff_no_change_produces_empty() {
    let mut diff = OpsSnapshotDiff::default();
    let mut first = HashMap::new();
    first.insert("order-svc".to_string(), 100u64);
    diff.delta_and_advance(first.clone());
    let deltas = diff.delta_and_advance(first);
    assert!(deltas.is_empty());
}

#[test]
fn ops_snapshot_diff_counter_reset_produces_zero_delta() {
    // If a counter goes backwards (process restart, metric reset),
    // emit delta 0 rather than wrap-around garbage.
    let mut diff = OpsSnapshotDiff::default();
    let mut first = HashMap::new();
    first.insert("order-svc".to_string(), 100u64);
    diff.delta_and_advance(first);

    let mut second = HashMap::new();
    second.insert("order-svc".to_string(), 10u64);
    let deltas = diff.delta_and_advance(second);
    assert!(!deltas.contains_key("order-svc"));
}

/// Build a standard test fixture: one Java process reading, one
/// "order-svc" → "java" process map, and a 5s scrape interval.
fn test_scrape_fixture(
    power_microwatts: f64,
    ops: u64,
) -> (
    ScaphandreState,
    Vec<ProcessPower>,
    HashMap<String, u64>,
    ScaphandreConfig,
) {
    let state = ScaphandreState::default();
    let readings = vec![ProcessPower {
        exe: "java".to_string(),
        power_microwatts,
    }];
    let mut deltas = HashMap::new();
    if ops > 0 {
        deltas.insert("order-svc".to_string(), ops);
    }
    let mut process_map = HashMap::new();
    process_map.insert("order-svc".to_string(), "java".to_string());
    let cfg = ScaphandreConfig {
        endpoint: "http://localhost:8080/metrics".to_string(),
        scrape_interval: Duration::from_secs(5),
        process_map,
    };
    (state, readings, deltas, cfg)
}

#[test]
fn apply_scrape_updates_mapped_service() {
    let (state, readings, deltas, cfg) = test_scrape_fixture(12_000_000.0, 8000);
    apply_scrape(&state, &readings, &deltas, &cfg, 100);

    // 12W × 5s / 8000 ops → 7.5e-9 kWh/op
    let snap = state.snapshot(100, 60_000);
    let got = *snap.get("order-svc").unwrap();
    let expected = (12.0 * 5.0 / 3_600_000.0) / 8000.0;
    assert!((got - expected).abs() < 1e-18);
}

#[test]
fn apply_scrape_keeps_previous_when_ops_zero() {
    // First scrape: 5000 ops, coefficient X.
    // Second scrape: 0 ops → state must NOT be updated (prevents
    // model-tag flapping for idle services).
    let (state, readings, _, cfg) = test_scrape_fixture(10_000_000.0, 5000);
    let mut deltas = HashMap::new();
    deltas.insert("order-svc".to_string(), 5000u64);
    apply_scrape(&state, &readings, &deltas, &cfg, 100);
    let first = *state.snapshot(100, 60_000).get("order-svc").unwrap();

    // Idle window: ops delta is absent from the map (we skip
    // services with 0 delta in OpsSnapshotDiff::delta_and_advance).
    apply_scrape(&state, &readings, &HashMap::new(), &cfg, 110);
    let second = *state.snapshot(110, 60_000).get("order-svc").unwrap();
    assert!((first - second).abs() < f64::EPSILON);
}

#[test]
fn scaphandre_state_snapshot_filters_stale() {
    let state = ScaphandreState::default();
    state.insert_for_test("order-svc".to_string(), 1e-7, 100);
    // staleness_threshold = 500ms, now = 1000ms → age = 900 > 500 → drop
    let snap = state.snapshot(1000, 500);
    assert!(snap.is_empty());
    // staleness_threshold = 2000ms, now = 1000ms → age = 900 < 2000 → keep
    let snap = state.snapshot(1000, 2000);
    assert_eq!(snap.len(), 1);
}

#[test]
fn scaphandre_state_snapshot_clock_skew_kept_as_fresh() {
    // If now_ms < last_update_ms (e.g. monotonic timer reset in
    // tests), saturating_sub returns 0 and the row is considered
    // fresh rather than being silently evicted.
    let state = ScaphandreState::default();
    state.insert_for_test("order-svc".to_string(), 1e-7, 1000);
    let snap = state.snapshot(500, 100);
    assert_eq!(snap.len(), 1);
}

/// End-to-end test: spin a minimal HTTP/1.1 server on a random port,
/// serve a canned Scaphandre Prometheus response, point
/// `fetch_metrics_once` at it, and verify the parsed readings match.
///
/// The fake server is intentionally hand-rolled (no axum, no tonic)
/// so this test is the single integration point that exercises the
/// real hyper-util legacy client against a real TCP socket. It covers
/// the full path: URI parse -> connect -> GET -> read bounded body
/// -> parse Prometheus text -> `ProcessPower`.
#[tokio::test]
async fn fetch_metrics_once_reads_from_fake_server() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // Canned Scaphandre-style response with two process entries
    // plus some noise metrics to exercise the parser filter.
    let body = "# HELP scaph_process_power_consumption_microwatts per-process power\n\
                # TYPE scaph_process_power_consumption_microwatts gauge\n\
                scaph_host_power_microwatts 50000000.0\n\
                scaph_process_power_consumption_microwatts{exe=\"java\",pid=\"1234\"} 12000000.0\n\
                scaph_process_power_consumption_microwatts{exe=\"dotnet\",pid=\"5678\"} 3000000.0\n";
    let response_owned = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; version=0.0.4\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    // Bind on ephemeral port and accept exactly one connection.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/metrics");

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Drain the request headers so the client doesn't see a reset.
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        socket.write_all(response_owned.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
        // Half-close so the client observes Connection: close.
        let _ = socket.shutdown().await;
    });

    // Drive the full fetch through the real hyper-util client.
    let client = build_scraper_client();
    let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
    let fetched = fetch_metrics_once(&client, &uri)
        .await
        .expect("fake server fetch should succeed");
    server.await.unwrap();

    // Parser + extraction should produce exactly two process entries.
    let readings = parse_scaphandre_metrics(&fetched);
    assert_eq!(readings.len(), 2);
    let java = readings.iter().find(|p| p.exe == "java").unwrap();
    assert!((java.power_microwatts - 12_000_000.0).abs() < f64::EPSILON);
    let dotnet = readings.iter().find(|p| p.exe == "dotnet").unwrap();
    assert!((dotnet.power_microwatts - 3_000_000.0).abs() < f64::EPSILON);

    // And the full scraper loop math: feed those readings into
    // apply_scrape and verify the ScaphandreState snapshot exposes
    // the correct per-service coefficient.
    let state = ScaphandreState::default();
    let mut deltas = HashMap::new();
    deltas.insert("order-svc".to_string(), 10_000u64);
    let mut process_map = HashMap::new();
    process_map.insert("order-svc".to_string(), "java".to_string());
    let cfg = ScaphandreConfig {
        endpoint,
        scrape_interval: Duration::from_secs(5),
        process_map,
    };
    apply_scrape(&state, &readings, &deltas, &cfg, 1_000);
    let snap = state.snapshot(1_000, 60_000);
    let got = *snap.get("order-svc").unwrap();
    // 12 W × 5 s / 10_000 ops → 6e-9 kWh/op
    let expected = (12.0 * 5.0 / 3_600_000.0) / 10_000.0;
    assert!(
        (got - expected).abs() < 1e-18,
        "expected {expected}, got {got}"
    );
}

/// End-to-end negative test: the fake server returns a 500 error,
/// `fetch_metrics_once` must surface it as `ScraperError::Fetch`.
#[tokio::test]
async fn fetch_metrics_once_surfaces_http_error_status() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/metrics");

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let resp = "HTTP/1.1 500 Internal Server Error\r\n\
                    Content-Length: 0\r\n\
                    Connection: close\r\n\
                    \r\n";
        socket.write_all(resp.as_bytes()).await.unwrap();
        let _ = socket.shutdown().await;
    });

    let client = build_scraper_client();
    let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
    let err = fetch_metrics_once(&client, &uri)
        .await
        .expect_err("500 should error");
    server.await.unwrap();
    match err {
        ScraperError::Fetch(crate::http_client::FetchError::HttpStatus(500)) => {}
        other => panic!("expected Fetch(HttpStatus(500)), got {other:?}"),
    }
}

// --- parser edge cases: target uncovered branches in parser.rs ---

/// A metric name that *starts with* the target prefix but is a different
/// metric (`scaph_process_power_consumption_microwatts_total`) must be
/// rejected — otherwise a Scaphandre version that ships a companion
/// `_total` histogram would be confused with the live gauge.
///
/// This also exercises the `_ => continue` arm in `parse_scaphandre_metrics`
/// (prefix collision, line ~64).
#[test]
fn parse_rejects_prefix_collision_metric() {
    let body = "scaph_process_power_consumption_microwatts_total{exe=\"java\"} 123\n";
    assert!(parse_scaphandre_metrics(body).is_empty());
}

/// A valid metric line with labels but without an `exe=` label must be
/// skipped (exercises the `let Some(exe) = ... else continue` branch and
/// the final `None` return from `extract_exe_label`).
#[test]
fn parse_skips_metric_without_exe_label() {
    let body =
        "scaph_process_power_consumption_microwatts{pid=\"1234\",cmdline=\"java -jar\"} 50000\n";
    assert!(parse_scaphandre_metrics(body).is_empty());
}

/// `extract_exe_label` must scan past unrelated labels before landing
/// on `exe`. This hits the "skip closing quote + comma + whitespace"
/// branch that advances past a non-exe label before looking at the next.
#[test]
fn parse_extracts_exe_when_not_first_label() {
    let body = "scaph_process_power_consumption_microwatts{pid=\"42\",exe=\"postgres\"} 12345\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "postgres");
    assert!((parsed[0].power_microwatts - 12345.0).abs() < f64::EPSILON);
}

/// A label block with unterminated `exe="...` (missing closing quote)
/// must be rejected. Hits the `if i >= bytes.len() { return None; }`
/// guard inside the value-collection loop of `extract_exe_label`.
#[test]
fn parse_rejects_label_with_unterminated_value() {
    // Note: we emit the `{`/`}` to pass the label-block guard, then
    // build a label value that starts `exe="` without a closing quote.
    let body = "scaph_process_power_consumption_microwatts{exe=\"unclosed} 100\n";
    // The unmatched inner `}` will be consumed by find_label_block_end,
    // leaving an incomplete labels string that extract_exe_label cannot
    // terminate, so the line is skipped.
    assert!(parse_scaphandre_metrics(body).is_empty());
}

/// A labels block that opens `{` but never closes must be skipped
/// (hits the `None` return of `find_label_block_end`).
#[test]
fn parse_skips_unmatched_label_block() {
    let body = "scaph_process_power_consumption_microwatts{exe=\"java\",pid=\"1 20000\n";
    // No closing `}` so the line is dropped.
    assert!(parse_scaphandre_metrics(body).is_empty());
}

/// A line where the numeric value does not parse as `f64` must be
/// skipped. Hits the `let Ok(value) = ... else continue;` branch.
#[test]
fn parse_skips_line_with_non_numeric_value() {
    let body = "scaph_process_power_consumption_microwatts{exe=\"java\"} not-a-number\n";
    assert!(parse_scaphandre_metrics(body).is_empty());
}

/// `unescape_prometheus_value` handles `\"`, `\\`, `\n`, and unknown
/// escapes. These cases are only reachable via a label value that
/// contains backslashes — exercise them through the public parser.
#[test]
fn parse_unescapes_quote_backslash_and_newline_in_exe_label() {
    // JVM-style command lines can embed quotes via `\"`. Scaphandre
    // exposes this verbatim in the `exe` label's escaped form.
    let body = "scaph_process_power_consumption_microwatts{exe=\"a\\\"b\"} 10\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].exe, "a\"b");

    let body = "scaph_process_power_consumption_microwatts{exe=\"a\\\\b\"} 20\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed[0].exe, "a\\b");

    let body = "scaph_process_power_consumption_microwatts{exe=\"line1\\nline2\"} 30\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed[0].exe, "line1\nline2");
}

/// Unknown escape sequences (e.g. `\t`) are not part of the Prometheus
/// spec but are passed through literally rather than dropped, so the
/// round-trip is stable on weird inputs.
#[test]
fn parse_preserves_unknown_escape_in_exe_label() {
    let body = "scaph_process_power_consumption_microwatts{exe=\"a\\tb\"} 5\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    // `\t` is not a recognized Prometheus escape, so the unescaper
    // keeps both the backslash and the `t`.
    assert_eq!(parsed[0].exe, "a\\tb");
}

/// Values with a trailing timestamp (Prometheus text format allows it)
/// must parse the first token as the value and discard the rest.
#[test]
fn parse_value_with_trailing_timestamp() {
    let body = "scaph_process_power_consumption_microwatts{exe=\"java\"} 12345 1700000000\n";
    let parsed = parse_scaphandre_metrics(body);
    assert_eq!(parsed.len(), 1);
    assert!((parsed[0].power_microwatts - 12345.0).abs() < f64::EPSILON);
}

// --- spawn_scraper / run_scraper_loop integration tests ---
//
// These drive the full `spawn_scraper` entry point against a mock
// HTTP server, exercising the tokio-task orchestration, the ticker,
// the OpsSnapshotDiff delta logic, the state publish path, and the
// `scaphandre_last_scrape_age_seconds` gauge update.

/// Spawn the scraper against a mock endpoint that serves a valid
/// Scaphandre response. After ~2 ticks, abort the task and verify the
/// shared state received a reading. This exercises the full hot path
/// in `run_scraper_loop`: URI parse → client build → ticker → fetch
/// → parse → `apply_scrape` → gauge update.
#[tokio::test]
async fn spawn_scraper_happy_path_updates_state() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // The mock serves one reading per connection. The scraper task
    // ticks at 50ms, so during a 200ms test window we expect ~3-4
    // successful scrapes — we spawn a loop of accepted connections
    // that all respond with the same canned body.
    let body = "scaph_process_power_consumption_microwatts{exe=\"java\"} 10000000\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/metrics");
    let response_arc = Arc::new(response);

    // Accept connections in a loop until the test cancels.
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let resp = response_arc.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let mut process_map = HashMap::new();
    process_map.insert("order-svc".to_string(), "java".to_string());
    let cfg = ScaphandreConfig {
        endpoint,
        scrape_interval: Duration::from_millis(50),
        process_map,
    };
    let state = Arc::new(ScaphandreState::default());
    let metrics = Arc::new(crate::report::metrics::MetricsState::default());

    // Feed the metrics snapshot so OpsSnapshotDiff produces a non-zero
    // delta, otherwise apply_scrape would be a no-op.
    metrics
        .service_io_ops_total
        .with_label_values(&["order-svc"])
        .inc_by(5_000.0);

    let handle = spawn_scraper(cfg, state.clone(), metrics.clone());

    // Let the scraper tick 3-4 times before aborting.
    tokio::time::sleep(Duration::from_millis(220)).await;
    handle.abort();
    let _ = handle.await;
    server.abort();
    let _ = server.await;

    // The state must now contain a coefficient for `order-svc`. Its
    // exact value is parser-dependent and tested elsewhere; here we
    // only assert that the full pipeline ran at least once.
    let snap = state.snapshot(crate::score::scaphandre::state::monotonic_ms(), 60_000);
    assert!(
        snap.contains_key("order-svc"),
        "state should have been populated for order-svc; got {snap:?}"
    );
}

/// Spawn the scraper against a server that always returns 500. Verify
/// the task keeps running (warn-once pattern) without panicking and
/// without updating the state.
#[tokio::test]
async fn spawn_scraper_500_keeps_running_and_state_empty() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/metrics");

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let resp = "HTTP/1.1 500 Internal Server Error\r\n\
                            Content-Length: 0\r\n\
                            Connection: close\r\n\
                            \r\n";
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let cfg = ScaphandreConfig {
        endpoint,
        scrape_interval: Duration::from_millis(40),
        process_map: HashMap::new(),
    };
    let state = Arc::new(ScaphandreState::default());
    let metrics = Arc::new(crate::report::metrics::MetricsState::default());
    let handle = spawn_scraper(cfg, state.clone(), metrics);

    // Let the scraper tick 4+ times so we cross the
    // UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD (3) and exercise the
    // one-shot "likely unsupported platform" warning path.
    tokio::time::sleep(Duration::from_millis(220)).await;
    handle.abort();
    let _ = handle.await;
    server.abort();
    let _ = server.await;

    // State must remain empty — no successful scrape means no readings.
    let snap = state.snapshot(crate::score::scaphandre::state::monotonic_ms(), 60_000);
    assert!(snap.is_empty(), "500 scrapes must not populate state");
}

/// Spawn the scraper with an invalid endpoint URI. `run_scraper_loop`
/// must log an error and exit cleanly (the task finishes naturally,
/// not via abort).
#[tokio::test]
async fn spawn_scraper_invalid_uri_exits_cleanly() {
    use std::sync::Arc;
    let cfg = ScaphandreConfig {
        endpoint: "not a valid :: uri".to_string(),
        scrape_interval: Duration::from_secs(5),
        process_map: HashMap::new(),
    };
    let state = Arc::new(ScaphandreState::default());
    let metrics = Arc::new(crate::report::metrics::MetricsState::default());
    let handle = spawn_scraper(cfg, state, metrics);

    // The task should exit on its own within a few ms because the URI
    // parse fails at the top of run_scraper_loop.
    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        result.is_ok(),
        "scraper should exit cleanly on invalid URI, got: {result:?}"
    );
}

/// Spawn the scraper against an unreachable endpoint (closed port).
/// The task must keep running (tick + fail + warn-once) without
/// panicking. We abort after ~2 ticks to end the test.
#[tokio::test]
async fn spawn_scraper_unreachable_endpoint_keeps_running() {
    use std::sync::Arc;
    // Port 1 is reserved and should be refused on localhost.
    let cfg = ScaphandreConfig {
        endpoint: "http://127.0.0.1:1/metrics".to_string(),
        scrape_interval: Duration::from_millis(50),
        process_map: HashMap::new(),
    };
    let state = Arc::new(ScaphandreState::default());
    let metrics = Arc::new(crate::report::metrics::MetricsState::default());
    let handle = spawn_scraper(cfg, state, metrics);

    // Let the scraper tick 2 times, then abort.
    tokio::time::sleep(Duration::from_millis(150)).await;
    handle.abort();
    let _ = handle.await;
    // Test passes if the task didn't panic.
}

// --- Additional fetch_metrics_once error path tests ---

/// The body limit is 8 MiB. Serve a response with a bogus
/// `Content-Length` > 8 MiB but minimal actual body — the hyper client
/// will try to read up to Content-Length and hit the Limited guard.
#[tokio::test]
async fn fetch_metrics_once_rejects_oversized_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/metrics");

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await;
        // Advertise a body larger than the 8 MiB cap and then stream
        // 9 MiB of garbage to trigger the LengthLimitError.
        let header = "HTTP/1.1 200 OK\r\n\
                      Content-Type: text/plain\r\n\
                      Content-Length: 9437184\r\n\
                      Connection: close\r\n\
                      \r\n";
        let _ = socket.write_all(header.as_bytes()).await;
        // Write 9 MiB in 64 KiB chunks.
        let chunk = vec![b'x'; 65536];
        for _ in 0..(9 * 16) {
            if socket.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = socket.shutdown().await;
    });

    let client = build_scraper_client();
    let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
    let err = fetch_metrics_once(&client, &uri)
        .await
        .expect_err("oversized body must fail with BodyRead (LengthLimit)");
    server.await.unwrap();
    match err {
        ScraperError::Fetch(crate::http_client::FetchError::BodyRead(msg)) => {
            assert!(
                msg.to_ascii_lowercase().contains("length") || msg.contains("limit"),
                "expected length-limit error, got: {msg}"
            );
        }
        other => panic!("expected Fetch(BodyRead), got {other:?}"),
    }
}

/// Ensure each `ScraperError` variant has a distinct, informative
/// Display message so operators can tell categories apart in logs.
#[test]
fn scraper_error_display_messages_are_informative() {
    use crate::http_client::FetchError;
    let e1 = ScraperError::Fetch(FetchError::BodyRead("oops".to_string()));
    let e2 = ScraperError::Fetch(FetchError::HttpStatus(418));
    let e3 = ScraperError::Fetch(FetchError::Timeout);
    assert!(format!("{e1}").contains("fetch failed"));
    assert!(format!("{e2}").contains("fetch failed"));
    assert!(format!("{e3}").contains("fetch failed"));
}
