//! Integration tests for the Alumet scraper: interval-energy math,
//! attribution, state, and the full HTTP-level scrape path.
//!
//! Prometheus exposition parsing is covered in
//! `crate::score::prom_parser`, the shared module this scraper feeds
//! from.

use std::collections::HashMap;
use std::time::Duration;

use super::apply::{apply_scrape, compute_energy_per_op_kwh, compute_window_kwh};
use super::config::{AlumetConfig, AlumetDatabaseConfig, DEFAULT_ENERGY_INTERVAL_SECS};
use super::scraper::{
    ScraperError, WarnOnceStreak, fetch_metrics_once, post_scrape_bookkeeping,
    scraper_error_reason, spawn_scraper, track_db_label_streak, track_zero_sample_streak,
};
use super::state::AlumetState;
use super::state::DbEnergyState;
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
        database: None,
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
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
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
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 200);
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
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 200);
    let snap = state.snapshot(200, 10_000);
    assert_eq!(snap.len(), 1);
    assert!((snap["checkout"] - 5e-7).abs() < f64::EPSILON);
}

#[test]
fn apply_scrape_sums_series_sharing_a_label_value() {
    // Alumet's label_key is operator-chosen and routinely non-unique:
    // one pod carries a row per RAPL domain, and `label_key = "domain"`
    // on a dual-socket host carries one `package` row per socket.
    // Energy is additive, so the rows must sum. Overwriting (a plain
    // `.collect()` into a HashMap) would keep the last row and halve the
    // figure silently, under a top-precedence `measured` tag.
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: 6.0,
        },
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: 4.0,
        },
    ];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
    let snap = state.snapshot(1000, 10_000);
    // 6 + 4 = 10 J over a 1s interval, extrapolated over the 5s window.
    let expected = (10.0 / 1.0 * 5.0) / 3_600_000.0 / 100.0;
    assert!(
        (snap["checkout"] - expected).abs() < 1e-18,
        "rows sharing a label_value must sum, got {} want {expected}",
        snap["checkout"]
    );
}

#[test]
fn sum_skips_nan_rows_instead_of_poisoning_the_label() {
    // The Prometheus text format legitimately carries NaN. One NaN row
    // must not poison every row sharing its label: the valid rows still
    // sum and publish, and the service still counts as matched.
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: 6.0,
        },
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: f64::NAN,
        },
    ];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    let matched = apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
    assert_eq!(matched, 1, "a NaN row must not unmatch the label");
    let snap = state.snapshot(1000, 10_000);
    let expected = (6.0 / 1.0 * 5.0) / 3_600_000.0 / 100.0;
    assert!(
        (snap["checkout"] - expected).abs() < 1e-18,
        "the valid row must publish, got {}",
        snap["checkout"]
    );
}

#[test]
fn sum_skips_negative_rows_instead_of_subtracting() {
    // A finite negative row (a mis-pointed metric_name matching a gauge
    // that dips negative) must not subtract from an otherwise valid sum
    // and understate the published figure.
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: 6.0,
        },
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: -2.0,
        },
    ];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 100);
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
    let snap = state.snapshot(1000, 10_000);
    let expected = (6.0 / 1.0 * 5.0) / 3_600_000.0 / 100.0;
    assert!(
        (snap["checkout"] - expected).abs() < 1e-18,
        "the negative row must be skipped, not subtracted, got {}",
        snap["checkout"]
    );
}

#[test]
fn apply_scrape_reports_matched_services_independently_of_ops() {
    let state = AlumetState::default();
    let cfg = sample_config();
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 10.0,
    }];
    // Idle service: the label is on the wire, so the mapping is right.
    let matched = apply_scrape(&state, None, &samples, &HashMap::new(), &cfg, 1000);
    assert_eq!(matched, 1, "an idle mapped service still counts as matched");

    // Mistyped mapping: nothing on the wire carries that label value.
    let wrong = vec![PromSample {
        label_value: "typo-pod".to_string(),
        value: 10.0,
    }];
    let matched = apply_scrape(&state, None, &wrong, &HashMap::new(), &cfg, 1000);
    assert_eq!(matched, 0, "a mapping that matches nothing must report 0");
}

#[test]
fn zero_joules_reading_keeps_the_previous_entry() {
    // A zero reading means the exporter's last flush caught the consumer
    // idle, not that the window's work was free. Publishing 0.0 would
    // override every lower-tier backend with a measured zero for a
    // service that demonstrably did I/O.
    let state = AlumetState::default();
    let cfg = sample_config();
    state.insert_for_test("checkout".to_string(), 5e-7, 100);
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 0.0,
    }];
    let mut op_deltas = HashMap::new();
    op_deltas.insert("checkout".to_string(), 500);
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 200);
    let snap = state.snapshot(200, 10_000);
    assert!(
        (snap["checkout"] - 5e-7).abs() < f64::EPSILON,
        "a 0 J reading must not publish a measured zero"
    );
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
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
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
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
    let first = state.snapshot(1000, 10_000)["checkout"];
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 2000);
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
    apply_scrape(&state, None, &samples, &op_deltas, &cfg, 1000);
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

// --- warn-once latches -------------------------------------------------

fn streaks() -> (WarnOnceStreak, WarnOnceStreak) {
    (WarnOnceStreak::default(), WarnOnceStreak::default())
}

fn one_tick(
    samples: usize,
    matched: usize,
    mappings: usize,
    a: &mut WarnOnceStreak,
    b: &mut WarnOnceStreak,
) {
    track_zero_sample_streak(
        samples,
        matched,
        0,
        mappings,
        "http://x/metrics",
        "m",
        "l",
        a,
        b,
    );
}

#[test]
fn no_samples_streak_warns_once_then_latches() {
    let (mut a, mut b) = streaks();
    for _ in 0..2 {
        one_tick(0, 0, 1, &mut a, &mut b);
    }
    assert!(!a.has_warned(), "must not warn before the 3-tick threshold");
    one_tick(0, 0, 1, &mut a, &mut b);
    // Pins WHICH latch armed: an empty exposition is a metric_name
    // problem, blaming the mappings would send the operator down the
    // wrong debugging path.
    assert!(
        a.has_warned(),
        "third consecutive zero-sample tick must warn"
    );
    assert!(
        !b.has_warned(),
        "an empty exposition must not blame the mappings"
    );
}

#[test]
fn no_samples_streak_resets_on_any_samples() {
    let (mut a, mut b) = streaks();
    for _ in 0..3 {
        one_tick(0, 0, 1, &mut a, &mut b);
    }
    assert!(a.has_warned());
    one_tick(2, 1, 1, &mut a, &mut b);
    assert!(
        !a.has_warned(),
        "a healthy tick must re-arm the no-samples latch"
    );
}

#[test]
fn no_match_streak_arms_when_samples_parse_but_no_service_matches() {
    // The mistyped-service_mappings case: metric_name and label_key are
    // right, so samples keep flowing and every counter reads healthy,
    // but nothing maps. Gating on samples_len alone would leave this
    // misconfiguration with no diagnostic at all.
    let (mut a, mut b) = streaks();
    for _ in 0..3 {
        one_tick(9, 0, 1, &mut a, &mut b);
    }
    assert!(
        b.has_warned(),
        "samples parsed but zero services matched must warn"
    );
    assert!(!a.has_warned(), "the no-samples latch must stay untouched");
}

#[test]
fn no_match_streak_never_arms_on_empty_mappings() {
    // An empty service_mappings table trivially matches nothing, that
    // is a staged config, not a typo. It gets a startup warning, not a
    // recurring streak warn.
    let (mut a, mut b) = streaks();
    for _ in 0..5 {
        one_tick(9, 0, 0, &mut a, &mut b);
    }
    assert!(
        !b.has_warned(),
        "empty mappings must not trip the no-match warn"
    );
}

#[test]
fn no_samples_ticks_do_not_advance_the_no_match_streak() {
    // Mixed streak: two empty ticks then a samples-present-no-match
    // tick. The no-match streak must count its OWN ticks from zero, not
    // inherit the empty ticks and fire on the first transition with a
    // message blaming the mappings for a metric_name problem.
    let (mut a, mut b) = streaks();
    one_tick(0, 0, 1, &mut a, &mut b);
    one_tick(0, 0, 1, &mut a, &mut b);
    one_tick(9, 0, 1, &mut a, &mut b);
    assert!(
        !b.has_warned(),
        "the no-match streak must not inherit no-samples ticks"
    );
    one_tick(9, 0, 1, &mut a, &mut b);
    one_tick(9, 0, 1, &mut a, &mut b);
    assert!(b.has_warned(), "three no-match ticks of its own must warn");
}

#[test]
fn a_latched_no_samples_warn_does_not_suppress_the_no_match_warn() {
    // Exporter warms up empty (latch A fires), then the metric appears
    // but the mappings are mistyped. The second cause must still get
    // its own warn, a single shared latch would silence it forever.
    let (mut a, mut b) = streaks();
    for _ in 0..3 {
        one_tick(0, 0, 1, &mut a, &mut b);
    }
    assert!(a.has_warned());
    for _ in 0..3 {
        one_tick(9, 0, 1, &mut a, &mut b);
    }
    assert!(
        b.has_warned(),
        "the no-match warn must fire after a latched no-samples warn"
    );
}

#[test]
fn streaks_stay_quiet_when_matched_services_are_merely_idle() {
    // A matched service with no ops is normal operation (nights, low
    // traffic), not a misconfiguration. Must not warn.
    let (mut a, mut b) = streaks();
    for _ in 0..5 {
        one_tick(2, 1, 1, &mut a, &mut b);
    }
    assert!(!a.has_warned(), "idle but matched must not trip any warn");
    assert!(!b.has_warned(), "idle but matched must not trip any warn");
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
async fn spawn_scraper_unreachable_endpoint_keeps_running() {
    // Bind+drop to get a guaranteed-closed port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut cfg = sample_config();
    cfg.endpoint = format!("http://{addr}/metrics");
    cfg.scrape_interval = Duration::from_millis(50);
    let state = AlumetState::new();
    let metrics = std::sync::Arc::new(crate::report::metrics::MetricsState::new());
    let handle = spawn_scraper(cfg, state, None, metrics);

    // Let a few scrape attempts fail.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Task is still alive (not aborted, not panicked): an unreachable
    // endpoint must degrade to the precedence chain, not kill the task.
    assert!(!handle.is_finished());
    handle.abort();
}

#[tokio::test]
async fn spawn_scraper_staleness_gauge_climbs_when_never_succeeds() {
    // Same regression guard Kepler carries: if `last_success_ms` were
    // seeded to 0 instead of scraper-start time, the gauge would sit at
    // 0.0 forever on an endpoint broken from boot, and every
    // staleness alert (including the Helm PrometheusRule) would stay
    // silent on a scraper that never worked.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut cfg = sample_config();
    cfg.endpoint = format!("http://{addr}/metrics");
    cfg.scrape_interval = Duration::from_millis(50);
    let state = AlumetState::new();
    let metrics = std::sync::Arc::new(crate::report::metrics::MetricsState::new());
    let handle = spawn_scraper(cfg, state, None, metrics.clone());

    // Poll until the gauge moves rather than waiting a fixed window: on
    // Windows, connecting to a dropped port can take until the 3s fetch
    // timeout, so a short fixed wait flakes. Same shape as Kepler's.
    let mut age = 0.0;
    for _ in 0..320 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        age = metrics.alumet_last_scrape_age_seconds.get();
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

#[tokio::test]
async fn spawn_scraper_aborts_cleanly_on_invalid_uri() {
    let mut cfg = sample_config();
    cfg.endpoint = "not a uri".to_string();
    let state = AlumetState::new();
    let metrics = std::sync::Arc::new(crate::report::metrics::MetricsState::new());
    let handle = spawn_scraper(cfg, state, None, metrics);
    // The task must return on its own rather than retry-spam forever.
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("scraper must exit promptly on an invalid endpoint")
        .expect("scraper task must not panic");
}

// --- database energy accumulation -------------------------------------

#[test]
fn compute_window_kwh_basic_and_guards() {
    // 10 J per 1s interval = 10 W, over a 5s scrape window = 50 J.
    let kwh = compute_window_kwh(10.0, 1.0, 5.0).unwrap();
    assert!((kwh - 50.0 / 3_600_000.0).abs() < 1e-15);
    assert!(compute_window_kwh(0.0, 1.0, 5.0).is_none());
    assert!(compute_window_kwh(-1.0, 1.0, 5.0).is_none());
    assert!(compute_window_kwh(f64::NAN, 1.0, 5.0).is_none());
    assert!(compute_window_kwh(10.0, 0.0, 5.0).is_none());
    assert!(compute_window_kwh(10.0, 1.0, 0.0).is_none());
}

#[test]
fn db_energy_state_take_is_a_delta_with_staleness_gate() {
    let db = DbEnergyState::new();
    // Nothing recorded yet.
    assert!(db.take_window_kwh(1000, 15_000).is_none());
    db.add_window_kwh(2e-5, 1000);
    db.add_window_kwh(1e-5, 2000);
    let first = db.take_window_kwh(3000, 15_000).unwrap();
    assert!((first - 3e-5).abs() < 1e-18);
    // Nothing new accumulated since the take.
    assert!(db.take_window_kwh(4000, 15_000).is_none());
    db.add_window_kwh(5e-6, 5000);
    // Stale reading: not delivered, not consumed.
    assert!(db.take_window_kwh(100_000, 15_000).is_none());
    // Fresh again after the scraper recovers: the energy is still there.
    db.add_window_kwh(5e-6, 99_000);
    let recovered = db.take_window_kwh(100_000, 15_000).unwrap();
    assert!((recovered - 1e-5).abs() < 1e-18);
}

#[test]
fn apply_scrape_accumulates_database_energy() {
    let mut cfg = sample_config();
    cfg.database = Some(AlumetDatabaseConfig {
        label_value: "postgres-pod".to_string(),
        region: Some("eu-west-3".to_string()),
    });
    let state = AlumetState::new();
    let db = DbEnergyState::new();
    let samples = vec![
        PromSample {
            label_value: "checkout-pod".to_string(),
            value: 10.0,
        },
        PromSample {
            label_value: "postgres-pod".to_string(),
            value: 20.0,
        },
    ];
    apply_scrape(&state, Some(&db), &samples, &HashMap::new(), &cfg, 1000);
    // 20 J/s over a 5s window = 100 J.
    let kwh = db.take_window_kwh(1500, 15_000).unwrap();
    assert!((kwh - 100.0 / 3_600_000.0).abs() < 1e-15);
}

#[test]
fn apply_scrape_without_database_config_leaves_state_untouched() {
    let cfg = sample_config();
    let state = AlumetState::new();
    let db = DbEnergyState::new();
    let samples = vec![PromSample {
        label_value: "postgres-pod".to_string(),
        value: 20.0,
    }];
    apply_scrape(&state, Some(&db), &samples, &HashMap::new(), &cfg, 1000);
    assert!(db.take_window_kwh(1500, 15_000).is_none());
}

#[test]
fn mark_alive_preserves_banked_energy_through_idle_and_label_loss() {
    let mut cfg = sample_config();
    cfg.database = Some(AlumetDatabaseConfig {
        label_value: "postgres-pod".to_string(),
        region: None,
    });
    let state = AlumetState::new();
    let db = DbEnergyState::new();
    let energized = vec![PromSample {
        label_value: "postgres-pod".to_string(),
        value: 20.0,
    }];
    apply_scrape(&state, Some(&db), &energized, &HashMap::new(), &cfg, 1_000);
    // Long idle or label rename: scrapes keep succeeding without the
    // label, the scraper marks liveness on every one of them.
    db.mark_alive(100_000);
    // Banked energy is still deliverable.
    let kwh = db.take_window_kwh(101_000, 15_000).unwrap();
    assert!((kwh - 100.0 / 3_600_000.0).abs() < 1e-15);
}

// --- post-scrape diagnostics -------------------------------------------

#[test]
fn track_db_label_streak_covers_every_branch() {
    let mut cfg = sample_config();
    let redacted = "http://host/metrics";
    let mut streak = WarnOnceStreak::default();

    // No database declared: early return, no warn.
    track_db_label_streak(&[], &cfg, redacted, &mut streak);
    assert!(!streak.has_warned());

    cfg.database = Some(AlumetDatabaseConfig {
        label_value: "pg-pod".to_string(),
        region: None,
    });

    // Empty exposition: belongs to the no_samples cause, not this one.
    track_db_label_streak(&[], &cfg, redacted, &mut streak);
    assert!(!streak.has_warned());

    // Label present: reset, no warn.
    let present = vec![PromSample {
        label_value: "pg-pod".to_string(),
        value: 1.0,
    }];
    track_db_label_streak(&present, &cfg, redacted, &mut streak);
    assert!(!streak.has_warned());

    // Label absent across three consecutive non-empty ticks: warn fires.
    let absent = vec![PromSample {
        label_value: "other".to_string(),
        value: 1.0,
    }];
    for _ in 0..3 {
        track_db_label_streak(&absent, &cfg, redacted, &mut streak);
    }
    assert!(streak.has_warned());
}

#[test]
fn post_scrape_bookkeeping_marks_liveness_and_runs_diagnostics() {
    let mut cfg = sample_config();
    cfg.database = Some(AlumetDatabaseConfig {
        label_value: "pg-pod".to_string(),
        region: None,
    });
    let db = DbEnergyState::new();
    let samples = vec![PromSample {
        label_value: "checkout-pod".to_string(),
        value: 5.0,
    }];
    let mut no_samples = WarnOnceStreak::default();
    let mut no_match = WarnOnceStreak::default();
    let mut db_missing = WarnOnceStreak::default();
    post_scrape_bookkeeping(
        &samples,
        1,
        1,
        &cfg,
        "http://host/metrics",
        Some(&db),
        1_234,
        &mut no_samples,
        &mut no_match,
        &mut db_missing,
    );
    // Liveness was marked, so banked energy stays deliverable.
    db.add_window_kwh(1e-6, 1_234);
    assert!(db.take_window_kwh(1_300, 15_000).is_some());
}
