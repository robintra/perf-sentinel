//! Integration tests for the Alumet scraper: interval-energy math,
//! attribution, state, and the full HTTP-level scrape path.
//!
//! Prometheus exposition parsing is covered in
//! `crate::score::prom_parser`, the shared module this scraper feeds
//! from.

use std::collections::HashMap;
use std::time::Duration;

use super::apply::{apply_scrape, compute_energy_per_op_kwh};
use super::config::{AlumetConfig, DEFAULT_ENERGY_INTERVAL_SECS};
use super::scraper::{
    ScraperError, fetch_metrics_once, scraper_error_reason, spawn_scraper, track_zero_sample_streak,
};
use super::state::AlumetState;
use crate::score::prom_parser::PromSample;

fn sample_config() -> AlumetConfig {
    let mut mappings = HashMap::new();
    mappings.insert("checkout".to_string(), "checkout-pod".to_string());
    AlumetConfig {
        endpoint: "http://localhost:9091/metrics".to_string(),
        scrape_interval: Duration::from_secs(5),
        metric_name: "attributed_energy_cpu_alumet".to_string(),
        label_key: "name".to_string(),
        energy_interval_secs: DEFAULT_ENERGY_INTERVAL_SECS,
        service_mappings: mappings,
        auth_header: None,
    }
}

// --- energy-per-op math -----------------------------------------------

#[test]
fn compute_energy_per_op_basic() {
    // 10 J per 1s interval = 10 W. Over a 5s scrape window that is
    // 50 J. 50 / 3.6e6 = 1.3889e-5 kWh, over 100 ops.
    let kwh = compute_energy_per_op_kwh(10.0, 1.0, 5.0, 100).unwrap();
    let expected = (10.0 / 1.0 * 5.0) / 3_600_000.0 / 100.0;
    assert!((kwh - expected).abs() < 1e-18);
}

#[test]
fn energy_interval_scales_the_result_inversely() {
    // The whole point of energy_interval_secs: the same raw joules
    // reading means half the power when it covers twice the interval.
    // A config drift here rescales energy and carbon linearly, which is
    // exactly the silent failure mode documented in LIMITATIONS.
    let at_1s = compute_energy_per_op_kwh(10.0, 1.0, 5.0, 100).unwrap();
    let at_2s = compute_energy_per_op_kwh(10.0, 2.0, 5.0, 100).unwrap();
    assert!(
        (at_1s / at_2s - 2.0).abs() < 1e-9,
        "doubling the interval must halve the coefficient"
    );
}

#[test]
fn joules_over_matching_interval_reads_as_watts() {
    // With Alumet's default 1 Hz poll, the raw number already is the
    // mean wattage. Pins the identity a reader would assume.
    let kwh = compute_energy_per_op_kwh(42.0, 1.0, 1.0, 1).unwrap();
    assert!((kwh - 42.0 / 3_600_000.0).abs() < 1e-18);
}

#[test]
fn compute_energy_per_op_zero_ops_returns_none() {
    assert!(compute_energy_per_op_kwh(10.0, 1.0, 5.0, 0).is_none());
}

#[test]
fn compute_energy_per_op_negative_joules_returns_none() {
    assert!(compute_energy_per_op_kwh(-1.0, 1.0, 5.0, 5).is_none());
}

#[test]
fn compute_energy_per_op_nan_returns_none() {
    assert!(compute_energy_per_op_kwh(f64::NAN, 1.0, 5.0, 5).is_none());
    assert!(compute_energy_per_op_kwh(f64::INFINITY, 1.0, 5.0, 5).is_none());
}

#[test]
fn compute_energy_per_op_zero_interval_returns_none() {
    // Config validation rejects this, the math must not divide by zero
    // into an infinite coefficient if it ever gets through.
    assert!(compute_energy_per_op_kwh(10.0, 0.0, 5.0, 5).is_none());
    assert!(compute_energy_per_op_kwh(10.0, -1.0, 5.0, 5).is_none());
    assert!(compute_energy_per_op_kwh(10.0, f64::NAN, 5.0, 5).is_none());
}

#[test]
fn compute_energy_per_op_zero_scrape_interval_returns_none() {
    assert!(compute_energy_per_op_kwh(10.0, 1.0, 0.0, 5).is_none());
}

#[test]
fn compute_energy_per_op_overflowing_product_returns_none() {
    // A huge reading over a tiny interval must not publish an infinite
    // coefficient that would poison every downstream carbon figure.
    let out = compute_energy_per_op_kwh(f64::MAX, f64::MIN_POSITIVE, 3600.0, 1);
    assert!(out.is_none_or(f64::is_finite));
}

// --- apply_scrape behavior --------------------------------------------

#[test]
fn apply_scrape_updates_state_with_coefficient() {
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 10.0,
    }];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    apply_scrape(&state, &samples, &op_deltas, &cfg, 1000);
    let snap = state.snapshot(1000, 10_000);
    assert_eq!(snap.len(), 1);
    let expected = (10.0 / 1.0 * 5.0) / 3_600_000.0 / 100.0;
    assert!((snap["checkout"] - expected).abs() < 1e-18);
}

#[test]
fn apply_scrape_keeps_prior_entry_when_ops_zero() {
    let state = AlumetState::default();
    let cfg = sample_config();
    state.insert_for_test("checkout".to_string(), 5e-7, 100);
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 10.0,
    }];
    let op_deltas = HashMap::new(); // no ops for the service
    apply_scrape(&state, &samples, &op_deltas, &cfg, 200);
    let snap = state.snapshot(200, 10_000);
    assert_eq!(snap.len(), 1);
    assert!((snap["checkout"] - 5e-7).abs() < f64::EPSILON);
}

#[test]
fn apply_scrape_skips_service_with_zero_ops_explicit_entry() {
    // Symmetric to the above: an explicit zero count must not publish a
    // 0.0 coefficient over the previous one.
    let state = AlumetState::default();
    let cfg = sample_config();
    state.insert_for_test("checkout".to_string(), 5e-7, 100);
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 10.0,
    }];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 0);
    apply_scrape(&state, &samples, &op_deltas, &cfg, 200);
    let snap = state.snapshot(200, 10_000);
    assert_eq!(snap.len(), 1);
    assert!((snap["checkout"] - 5e-7).abs() < f64::EPSILON);
}

#[test]
fn apply_scrape_ignores_unmapped_label() {
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![PromSample {
        label_value: "some-other-pod".to_string(),
        value: 10.0,
    }];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    apply_scrape(&state, &samples, &op_deltas, &cfg, 1000);
    assert!(state.snapshot(1000, 10_000).is_empty());
}

#[test]
fn apply_scrape_is_stateless_across_ticks() {
    // Unlike Kepler, no counter delta: the same reading twice must
    // publish the same coefficient, not a zero delta on the second tick.
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 10.0,
    }];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    apply_scrape(&state, &samples, &op_deltas, &cfg, 1000);
    let first = state.snapshot(1000, 10_000)["checkout"];
    apply_scrape(&state, &samples, &op_deltas, &cfg, 2000);
    let second = state.snapshot(2000, 10_000)["checkout"];
    assert!((first - second).abs() < f64::EPSILON);
}

#[test]
fn apply_scrape_reflects_configured_energy_interval() {
    let state = AlumetState::default();
    let mut cfg = sample_config();
    cfg.energy_interval_secs = 5.0;
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 10.0,
    }];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    apply_scrape(&state, &samples, &op_deltas, &cfg, 1000);
    let snap = state.snapshot(1000, 10_000);
    // 10 J over 5s = 2 W, over a 5s window = 10 J.
    let expected = 10.0 / 3_600_000.0 / 100.0;
    assert!((snap["checkout"] - expected).abs() < 1e-18);
}

// --- staleness --------------------------------------------------------

#[test]
fn stale_entries_are_filtered() {
    let state = AlumetState::default();
    state.insert_for_test("stale-svc".to_string(), 1e-7, 0);
    assert!(state.snapshot(100, 1).is_empty());
    state.insert_for_test("fresh-svc".to_string(), 2e-7, 99);
    let snap = state.snapshot(100, 50);
    assert!(snap.contains_key("fresh-svc"));
    assert!(!snap.contains_key("stale-svc"));
}

// --- warn-once latch --------------------------------------------------

#[test]
fn zero_sample_streak_warns_once_then_latches() {
    let mut ticks = 0;
    let mut warned = false;
    for _ in 0..2 {
        track_zero_sample_streak(0, 0, "http://x/metrics", "m", "l", &mut ticks, &mut warned);
    }
    assert!(!warned, "must not warn before the 3-tick threshold");
    track_zero_sample_streak(0, 0, "http://x/metrics", "m", "l", &mut ticks, &mut warned);
    assert!(warned, "third consecutive zero-sample tick must warn");
    assert_eq!(ticks, 3);
}

#[test]
fn zero_sample_streak_resets_on_samples() {
    let mut ticks = 5;
    let mut warned = true;
    track_zero_sample_streak(2, 1, "http://x/metrics", "m", "l", &mut ticks, &mut warned);
    assert_eq!(ticks, 0);
    assert!(!warned, "a successful match must re-arm the latch");
}

// --- scraper error mapping --------------------------------------------

#[test]
fn scraper_error_reason_maps_fetch_errors() {
    use crate::http_client::FetchError;
    use crate::report::metrics::AlumetScrapeReason;
    let utf8_err = ScraperError::Utf8(String::from_utf8(vec![0xff, 0xfe]).unwrap_err());
    assert_eq!(
        scraper_error_reason(&utf8_err),
        AlumetScrapeReason::InvalidUtf8
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::Timeout)),
        AlumetScrapeReason::Timeout
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::HttpStatus(503))),
        AlumetScrapeReason::HttpError
    );
}

// --- HTTP-level path --------------------------------------------------

#[tokio::test]
async fn fetch_metrics_once_reads_a_live_exposition() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let body = "attributed_energy_cpu_alumet{name=\"checkout-pod\"} 12.5\n\
                attributed_energy_cpu_alumet{name=\"api-pod\"} 3.5\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/metrics", listener.local_addr().unwrap());
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
        "attributed_energy_cpu_alumet",
        "name",
    );
    assert_eq!(samples.len(), 2);
}

#[tokio::test]
async fn spawn_scraper_aborts_cleanly_on_invalid_uri() {
    let mut cfg = sample_config();
    cfg.endpoint = "not a uri".to_string();
    let state = AlumetState::new();
    let metrics = std::sync::Arc::new(crate::report::metrics::MetricsState::new());
    let handle = spawn_scraper(cfg, state, metrics);
    // The task must return on its own rather than retry-spam forever.
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("scraper must exit promptly on an invalid endpoint")
        .expect("scraper task must not panic");
}
