//! Integration tests that cross the sub-module boundaries (parser,
//! ops, state, scraper). Per-sub-module unit tests that only touch
//! one file could live next to their code, but keeping everything in
//! one place makes the test module layout match the pre-split
//! organization exactly so git blame still works.

use std::collections::HashMap;
use std::time::Duration;

use super::config::ScaphandreConfig;
use super::ops::{OpsSnapshotDiff, apply_scrape, compute_energy_per_op_kwh};
use super::parser::{ProcessPower, parse_scaphandre_metrics};
use super::scraper::{ScraperError, build_scraper_client, fetch_metrics_once};
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

#[test]
fn apply_scrape_updates_mapped_service() {
    let state = ScaphandreState::default();
    let readings = vec![ProcessPower {
        exe: "java".to_string(),
        power_microwatts: 12_000_000.0, // 12 W
    }];
    let mut deltas = HashMap::new();
    deltas.insert("order-svc".to_string(), 8000u64);
    let mut process_map = HashMap::new();
    process_map.insert("order-svc".to_string(), "java".to_string());
    let cfg = ScaphandreConfig {
        endpoint: "http://localhost:8080/metrics".to_string(),
        scrape_interval: Duration::from_secs(5),
        process_map,
    };
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
    let state = ScaphandreState::default();
    let readings = vec![ProcessPower {
        exe: "java".to_string(),
        power_microwatts: 10_000_000.0,
    }];
    let mut deltas = HashMap::new();
    deltas.insert("order-svc".to_string(), 5000u64);
    let mut process_map = HashMap::new();
    process_map.insert("order-svc".to_string(), "java".to_string());
    let cfg = ScaphandreConfig {
        endpoint: "http://localhost:8080/metrics".to_string(),
        scrape_interval: Duration::from_secs(5),
        process_map,
    };
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
/// `fetch_metrics_once` must surface it as `ScraperError::HttpStatus`.
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
        ScraperError::HttpStatus(500) => {}
        other => panic!("expected HttpStatus(500), got {other:?}"),
    }
}
