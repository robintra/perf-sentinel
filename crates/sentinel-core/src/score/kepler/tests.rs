//! Integration tests for the Kepler scraper: counter-delta,
//! attribution, state, and the full HTTP-level scrape path.
//!
//! Prometheus exposition parsing is covered in
//! `crate::score::prom_parser`, the shared module this scraper feeds
//! from.

use std::collections::HashMap;
use std::time::Duration;

use super::apply::{apply_scrape, compute_energy_per_op_kwh, joules_deltas, process_scrape};
use super::config::{KeplerConfig, KeplerMetricKind};
use super::scraper::{
    ScraperError, fetch_metrics_once, scraper_error_reason, spawn_scraper, track_zero_sample_streak,
};
use super::state::KeplerState;
use crate::score::prom_parser::PromSample;

// --- counter-delta tests ----------------------------------------------

#[test]
fn first_observation_records_raw_no_delta() {
    let mut last = HashMap::new();
    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());
    let samples = vec![PromSample {
        label_value: "order".to_string(),
        value: 1000.0,
    }];
    let deltas = joules_deltas(&samples, &mappings, &mut last);
    assert!(deltas.is_empty(), "first tick must not emit a delta");
    assert_eq!(last.get("order-svc"), Some(&1000.0));
}

#[test]
fn second_observation_emits_positive_delta() {
    let mut last = HashMap::new();
    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());

    let first = vec![PromSample {
        label_value: "order".to_string(),
        value: 1000.0,
    }];
    joules_deltas(&first, &mappings, &mut last);

    let second = vec![PromSample {
        label_value: "order".to_string(),
        value: 1234.0,
    }];
    let deltas = joules_deltas(&second, &mappings, &mut last);
    assert_eq!(deltas.len(), 1);
    assert!((deltas.get("order-svc").unwrap() - 234.0).abs() < f64::EPSILON);
    assert_eq!(last.get("order-svc"), Some(&1234.0));
}

#[test]
fn counter_reset_clamps_to_no_delta() {
    let mut last = HashMap::new();
    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());

    let first = vec![PromSample {
        label_value: "order".to_string(),
        value: 5000.0,
    }];
    joules_deltas(&first, &mappings, &mut last);

    // Counter went backwards (exporter restart).
    let second = vec![PromSample {
        label_value: "order".to_string(),
        value: 100.0,
    }];
    let deltas = joules_deltas(&second, &mappings, &mut last);
    assert!(
        deltas.is_empty(),
        "negative delta must be omitted, not surfaced"
    );
    // The raw counter is still advanced so the NEXT delta is correct.
    assert_eq!(last.get("order-svc"), Some(&100.0));
}

#[test]
fn no_change_produces_empty_deltas() {
    let mut last = HashMap::new();
    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());

    let samples = vec![PromSample {
        label_value: "order".to_string(),
        value: 1000.0,
    }];
    joules_deltas(&samples, &mappings, &mut last);
    let deltas = joules_deltas(&samples, &mappings, &mut last);
    assert!(deltas.is_empty());
}

#[test]
fn unmapped_label_is_ignored() {
    let mut last = HashMap::new();
    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());

    let samples = vec![PromSample {
        label_value: "different-label".to_string(),
        value: 1000.0,
    }];
    let deltas = joules_deltas(&samples, &mappings, &mut last);
    assert!(deltas.is_empty());
    assert!(!last.contains_key("order-svc"));
}

// --- energy-per-op math ----------------------------------------------

#[test]
fn compute_energy_per_op_basic() {
    // 3.6 MJ over 100 ops → 1 kWh / 100 = 0.01 kWh per op.
    let kwh = compute_energy_per_op_kwh(3_600_000.0, 100).unwrap();
    assert!((kwh - 0.01).abs() < 1e-12);
}

#[test]
fn compute_energy_per_op_zero_ops_returns_none() {
    assert!(compute_energy_per_op_kwh(100.0, 0).is_none());
}

#[test]
fn compute_energy_per_op_negative_joules_returns_none() {
    assert!(compute_energy_per_op_kwh(-1.0, 5).is_none());
}

#[test]
fn compute_energy_per_op_nan_returns_none() {
    assert!(compute_energy_per_op_kwh(f64::NAN, 5).is_none());
    assert!(compute_energy_per_op_kwh(f64::INFINITY, 5).is_none());
}

// --- apply_scrape behavior --------------------------------------------

fn sample_config() -> KeplerConfig {
    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());
    KeplerConfig {
        endpoint: "http://kepler:9102/metrics".to_string(),
        scrape_interval: Duration::from_secs(5),
        metric_kind: KeplerMetricKind::Container,
        service_mappings: mappings,
        auth_header: None,
    }
}

#[test]
fn apply_scrape_updates_state_with_coefficient() {
    let state = KeplerState::default();
    let mut joule_deltas_map = HashMap::new();
    joule_deltas_map.insert("order-svc".to_string(), 7_200_000.0); // 2 kWh worth
    let mut op_deltas = HashMap::new();
    op_deltas.insert("order-svc".to_string(), 200);
    apply_scrape(&state, &joule_deltas_map, &op_deltas, 1000);
    let snap = state.snapshot(1000, 10_000);
    assert_eq!(snap.len(), 1);
    // 7.2e6 J / 3.6e6 = 2 kWh / 200 ops = 0.01 kWh per op.
    assert!((snap["order-svc"] - 0.01).abs() < 1e-12);
}

#[test]
fn apply_scrape_keeps_prior_entry_when_ops_zero() {
    let state = KeplerState::default();
    state.insert_for_test("order-svc".to_string(), 5e-7, 100);
    let mut joule_deltas_map = HashMap::new();
    joule_deltas_map.insert("order-svc".to_string(), 1.0);
    let op_deltas = HashMap::new(); // no ops for the service
    apply_scrape(&state, &joule_deltas_map, &op_deltas, 200);
    let snap = state.snapshot(200, 10_000);
    assert_eq!(snap.len(), 1);
    assert!((snap["order-svc"] - 5e-7).abs() < f64::EPSILON);
}

#[test]
fn apply_scrape_skips_service_with_zero_ops_explicit_entry() {
    // Symmetric to the above: when op_deltas explicitly contains the
    // service with a zero count (rather than being absent), we still
    // skip and keep the previous coefficient. Guards against a
    // refactor that bumps `0` ops to a published 0.0 coefficient.
    let state = KeplerState::default();
    state.insert_for_test("order-svc".to_string(), 5e-7, 100);
    let mut joule_deltas_map = HashMap::new();
    joule_deltas_map.insert("order-svc".to_string(), 1.0);
    let mut op_deltas = HashMap::new();
    op_deltas.insert("order-svc".to_string(), 0);
    apply_scrape(&state, &joule_deltas_map, &op_deltas, 200);
    let snap = state.snapshot(200, 10_000);
    assert_eq!(snap.len(), 1);
    assert!((snap["order-svc"] - 5e-7).abs() < f64::EPSILON);
}

#[test]
fn process_scrape_end_to_end_after_two_ticks() {
    let state = KeplerState::default();
    let cfg = sample_config();
    let mut last_raw = HashMap::new();

    // Tick 1: record raw, no state change.
    let samples1 = vec![PromSample {
        label_value: "order".to_string(),
        value: 1_000_000.0,
    }];
    let mut ops1 = HashMap::new();
    ops1.insert("order-svc".to_string(), 50);
    process_scrape(&state, &samples1, &ops1, &cfg, &mut last_raw, 1000);
    assert!(state.snapshot(1000, 10_000).is_empty());

    // Tick 2: delta = 3,600,000 J over 100 ops → 0.01 kWh per op.
    let samples2 = vec![PromSample {
        label_value: "order".to_string(),
        value: 4_600_000.0,
    }];
    let mut ops2 = HashMap::new();
    ops2.insert("order-svc".to_string(), 100);
    process_scrape(&state, &samples2, &ops2, &cfg, &mut last_raw, 2000);
    let snap = state.snapshot(2000, 10_000);
    assert_eq!(snap.len(), 1);
    assert!((snap["order-svc"] - 0.01).abs() < 1e-12);
}

// --- state staleness ---------------------------------------------------

#[test]
fn state_filters_stale_entries() {
    let state = KeplerState::default();
    state.insert_for_test("order-svc".to_string(), 5e-7, 100);
    // now=700, staleness=500 → age 600 >= 500 → stale.
    assert!(state.snapshot(700, 500).is_empty());
}

// --- HTTP-level scraper tests -----------------------------------------

#[tokio::test]
async fn fetch_metrics_once_reads_from_fake_server() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let body = "# HELP kepler_container_cpu_joules_total Energy consumption in joules\n\
                # TYPE kepler_container_cpu_joules_total counter\n\
                kepler_container_cpu_joules_total{container_name=\"order-svc\",pod_name=\"p1\"} 12345.6\n\
                kepler_container_cpu_joules_total{container_name=\"chat-svc\",pod_name=\"p2\"} 999.9\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; version=0.0.4\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}/metrics");

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
        let _ = socket.shutdown().await;
    });

    let client = crate::http_client::build_client();
    let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
    let fetched = fetch_metrics_once(&client, &uri, None)
        .await
        .expect("fake server fetch should succeed");
    server.await.unwrap();

    let samples = crate::score::prom_parser::parse_metric_samples(
        &fetched,
        "kepler_container_cpu_joules_total",
        "container_name",
    );
    assert_eq!(samples.len(), 2);
}

#[test]
fn scraper_error_reason_maps_fetch_errors() {
    use crate::http_client::FetchError;
    use crate::report::metrics::KeplerScrapeReason;
    let utf8_err = ScraperError::Utf8(String::from_utf8(vec![0xff, 0xfe]).unwrap_err());
    assert_eq!(
        scraper_error_reason(&utf8_err),
        KeplerScrapeReason::InvalidUtf8
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::Timeout)),
        KeplerScrapeReason::Timeout
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::HttpStatus(503))),
        KeplerScrapeReason::HttpError
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::BodyRead("eof".into()))),
        KeplerScrapeReason::BodyReadError
    );
}

// --- track_zero_sample_streak --------------------------------------

#[test]
fn track_zero_sample_streak_does_not_warn_under_threshold() {
    let mut count: u32 = 0;
    let mut warned = false;
    for _ in 0..2 {
        track_zero_sample_streak(
            0,
            0,
            "http://redacted/metrics",
            "kepler_container_cpu_joules_total",
            "container_name",
            &mut count,
            &mut warned,
        );
    }
    assert_eq!(count, 2);
    assert!(
        !warned,
        "warn flag must stay false until the 3rd zero-sample tick"
    );
}

#[test]
fn track_zero_sample_streak_warns_after_three_consecutive_empty_ticks() {
    let mut count: u32 = 0;
    let mut warned = false;
    for _ in 0..3 {
        track_zero_sample_streak(
            0,
            0,
            "http://redacted/metrics",
            "kepler_container_cpu_joules_total",
            "container_name",
            &mut count,
            &mut warned,
        );
    }
    assert_eq!(count, 3);
    assert!(
        warned,
        "3rd consecutive zero-sample tick must trip the warn flag"
    );
}

#[test]
fn track_zero_sample_streak_warns_only_once_per_streak() {
    let mut count: u32 = 0;
    let mut warned = false;
    for _ in 0..10 {
        track_zero_sample_streak(
            0,
            0,
            "http://redacted/metrics",
            "kepler_container_cpu_joules_total",
            "container_name",
            &mut count,
            &mut warned,
        );
    }
    // Flag latches at the first trigger and stays true; the helper is
    // expected to call `tracing::warn!` exactly once over the streak.
    assert_eq!(count, 10);
    assert!(warned);
}

#[test]
fn track_zero_sample_streak_resets_on_non_empty_scrape() {
    let mut count: u32 = 5;
    let mut warned = true;
    track_zero_sample_streak(
        7, // samples_len > 0
        2,
        "http://redacted/metrics",
        "kepler_container_cpu_joules_total",
        "container_name",
        &mut count,
        &mut warned,
    );
    assert_eq!(count, 0);
    assert!(!warned, "non-empty scrape must reset the warn latch");
}

#[tokio::test]
async fn spawn_scraper_unreachable_endpoint_keeps_running() {
    // Bind+drop to get a guaranteed-closed port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let endpoint = format!("http://{addr}/metrics");

    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());
    let cfg = KeplerConfig {
        endpoint,
        scrape_interval: Duration::from_millis(50),
        metric_kind: KeplerMetricKind::Container,
        service_mappings: mappings,
        auth_header: None,
    };
    let state = KeplerState::new();
    let metrics = std::sync::Arc::new(crate::report::metrics::MetricsState::new());
    let handle = spawn_scraper(cfg, state.clone(), metrics);

    // Let a few scrape attempts fail.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Task is still alive (not aborted, not panicked).
    assert!(!handle.is_finished());
    handle.abort();
}

#[tokio::test]
async fn spawn_scraper_staleness_gauge_climbs_when_never_succeeds() {
    // Regression guard against the round-1 bug where the gauge
    // stayed at 0.0 forever if the scraper failed from the very first
    // tick. `last_success_ms` is now seeded to scraper-start time so
    // the gauge climbs from boot on a hung endpoint.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut mappings = HashMap::new();
    mappings.insert("order-svc".to_string(), "order".to_string());
    let cfg = KeplerConfig {
        endpoint: format!("http://{addr}/metrics"),
        scrape_interval: Duration::from_millis(50),
        metric_kind: KeplerMetricKind::Container,
        service_mappings: mappings,
        auth_header: None,
    };
    let state = KeplerState::new();
    let metrics = std::sync::Arc::new(crate::report::metrics::MetricsState::new());
    let handle = spawn_scraper(cfg, state, metrics.clone());

    // Poll until the staleness gauge starts climbing rather than waiting a
    // fixed window. On Linux the scrape fails fast (connection refused) within
    // a tick, but on Windows connecting to the dropped port can take until the
    // fetch timeout (3s) to fail, so a fixed 300ms wait flakes. Break as soon
    // as the gauge moves; the budget only has to exceed the fetch timeout.
    let mut age = 0.0;
    for _ in 0..320 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        age = metrics.kepler_last_scrape_age_seconds.get();
        if age > 0.0 {
            break;
        }
    }
    handle.abort();
    assert!(
        age > 0.0,
        "staleness gauge should climb on never-succeeded scraper, got {age}"
    );
}
