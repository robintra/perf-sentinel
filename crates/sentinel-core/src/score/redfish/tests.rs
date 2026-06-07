//! Integration tests for the Redfish scraper.

use std::collections::HashMap;

use super::apply::{apply_chassis_scrape, build_chassis_services};
use super::config::RedfishSchema;
use super::parser::{ParseOutcome, parse_redfish_power};
use super::scraper::{ScraperError, scraper_error_reason};
use super::state::ServiceEnergy;

// --- attribution tests -----------------------------------------------

fn services(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

fn ops(entries: &[(&str, u64)]) -> HashMap<String, u64> {
    entries
        .iter()
        .map(|(svc, ops)| ((*svc).to_string(), *ops))
        .collect()
}

fn mappings(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(svc, chassis)| ((*svc).to_string(), (*chassis).to_string()))
        .collect()
}

#[test]
fn single_chassis_single_service_publishes_coefficient() {
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let chassis_services = services(&["order-svc"]);
    let deltas = ops(&[("order-svc", 100)]);
    // 300 W × 60 s = 18000 J = 5e-3 kWh / 100 ops = 5e-5 kWh per op.
    let changed = apply_chassis_scrape(&mut next, &chassis_services, 300.0, 60.0, &deltas, 1000);
    assert!(changed);
    assert_eq!(next.len(), 1);
    assert!((next["order-svc"].energy_per_op_kwh - 5e-5).abs() < 1e-12);
}

#[test]
fn two_services_on_same_chassis_share_coefficient() {
    // Total ops 300 across two services → both get the same per-op value.
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let chassis_services = services(&["svc-a", "svc-b"]);
    let deltas = ops(&[("svc-a", 100), ("svc-b", 200)]);
    apply_chassis_scrape(&mut next, &chassis_services, 300.0, 60.0, &deltas, 1000);
    assert_eq!(next.len(), 2);
    // 18000 J / 3.6e6 = 5e-3 kWh, divided by 300 ops = 1.6666e-5.
    let expected = (300.0 * 60.0 / 3_600_000.0) / 300.0;
    assert!((next["svc-a"].energy_per_op_kwh - expected).abs() < 1e-15);
    assert!((next["svc-b"].energy_per_op_kwh - expected).abs() < 1e-15);
}

#[test]
fn service_on_other_chassis_unaffected() {
    // Only chassis-1's services are passed to apply_chassis_scrape, so
    // chassis-2's service stays out of `next`.
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let chassis_services = services(&["svc-a"]); // svc-b lives elsewhere
    let deltas = ops(&[("svc-a", 100), ("svc-b", 50)]);
    apply_chassis_scrape(&mut next, &chassis_services, 300.0, 60.0, &deltas, 1000);
    assert_eq!(next.len(), 1);
    assert!(next.contains_key("svc-a"));
    assert!(!next.contains_key("svc-b"));
}

#[test]
fn zero_ops_keeps_previous_entry() {
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    next.insert(
        "svc-a".to_string(),
        ServiceEnergy {
            energy_per_op_kwh: 7e-7,
            last_update_ms: 100,
        },
    );
    let chassis_services = services(&["svc-a"]);
    let deltas = ops(&[]);
    let changed = apply_chassis_scrape(&mut next, &chassis_services, 300.0, 60.0, &deltas, 200);
    assert!(!changed);
    assert!((next["svc-a"].energy_per_op_kwh - 7e-7).abs() < f64::EPSILON);
    assert_eq!(next["svc-a"].last_update_ms, 100);
}

#[test]
fn negative_watts_ignored() {
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    next.insert(
        "svc-a".to_string(),
        ServiceEnergy {
            energy_per_op_kwh: 7e-7,
            last_update_ms: 100,
        },
    );
    let chassis_services = services(&["svc-a"]);
    let deltas = ops(&[("svc-a", 100)]);
    let changed = apply_chassis_scrape(&mut next, &chassis_services, -1.0, 60.0, &deltas, 200);
    assert!(!changed);
    assert!((next["svc-a"].energy_per_op_kwh - 7e-7).abs() < f64::EPSILON);
}

#[test]
fn nan_watts_ignored() {
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let chassis_services = services(&["svc-a"]);
    let deltas = ops(&[("svc-a", 100)]);
    let changed = apply_chassis_scrape(&mut next, &chassis_services, f64::NAN, 60.0, &deltas, 200);
    assert!(!changed);
    assert!(next.is_empty());
}

#[test]
fn non_finite_scrape_interval_rejected() {
    // Defense in depth, config clamps to [15, 3600] but the function
    // is `pub` and could be called with non-finite f64.
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let chassis_services = services(&["svc-a"]);
    let deltas = ops(&[("svc-a", 100)]);
    assert!(!apply_chassis_scrape(
        &mut next,
        &chassis_services,
        300.0,
        f64::NAN,
        &deltas,
        200,
    ));
    assert!(!apply_chassis_scrape(
        &mut next,
        &chassis_services,
        300.0,
        f64::INFINITY,
        &deltas,
        200,
    ));
    assert!(next.is_empty());
}

#[test]
fn empty_chassis_services_is_noop() {
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let deltas = ops(&[("svc-a", 100)]);
    let changed = apply_chassis_scrape(&mut next, &[], 300.0, 60.0, &deltas, 200);
    assert!(!changed);
    assert!(next.is_empty());
}

#[test]
fn build_chassis_services_groups_by_chassis() {
    let m = mappings(&[
        ("svc-a", "chassis-1"),
        ("svc-b", "chassis-1"),
        ("svc-c", "chassis-2"),
    ]);
    let by_chassis = build_chassis_services(&m);
    assert_eq!(by_chassis.len(), 2);
    let mut chassis1 = by_chassis.get("chassis-1").cloned().unwrap();
    chassis1.sort();
    assert_eq!(chassis1, vec!["svc-a".to_string(), "svc-b".to_string()]);
    assert_eq!(
        by_chassis.get("chassis-2").unwrap(),
        &vec!["svc-c".to_string()]
    );
}

// --- scraper error mapping ------------------------------------------

#[test]
fn scraper_error_reason_maps_fetch_errors() {
    use crate::http_client::FetchError;
    use crate::report::metrics::RedfishScrapeReason;
    let utf8_err = ScraperError::Utf8(String::from_utf8(vec![0xff, 0xfe]).unwrap_err());
    assert_eq!(
        scraper_error_reason(&utf8_err),
        RedfishScrapeReason::InvalidUtf8
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::InvalidJson),
        RedfishScrapeReason::InvalidJson
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::PathMissing),
        RedfishScrapeReason::PathMissing
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::InvalidValue),
        RedfishScrapeReason::InvalidValue
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::Timeout)),
        RedfishScrapeReason::Timeout
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::HttpStatus(500))),
        RedfishScrapeReason::HttpError
    );
    assert_eq!(
        scraper_error_reason(&ScraperError::Fetch(FetchError::BodyRead("eof".into()))),
        RedfishScrapeReason::BodyReadError
    );
}

// --- parser tests (cross-vendor fixtures) ----------------------------

#[test]
fn parses_dell_idrac_response() {
    let body = r#"{
        "@odata.id": "/redfish/v1/Chassis/System.Embedded.1/Power",
        "Id": "Power",
        "Name": "Power",
        "PowerControl": [
            {
                "@odata.id": "/redfish/v1/Chassis/System.Embedded.1/Power#/PowerControl/0",
                "MemberId": "0",
                "Name": "System Power Control",
                "PowerConsumedWatts": 287.0,
                "PowerCapacityWatts": 750.0
            }
        ]
    }"#;
    assert_eq!(
        parse_redfish_power(body, RedfishSchema::LegacyPower),
        ParseOutcome::Ok(287.0)
    );
}

#[test]
fn parses_hpe_ilo_response() {
    // HPE iLO uses the same Redfish standard path for PowerConsumedWatts.
    let body = r#"{
        "@odata.id": "/redfish/v1/Chassis/1/Power/",
        "Id": "Power",
        "Name": "PowerMetrics",
        "PowerControl": [
            {
                "@odata.id": "/redfish/v1/Chassis/1/Power/#PowerControl/0",
                "MemberId": "0",
                "PowerConsumedWatts": 412.5,
                "PowerCapacityWatts": 1000
            }
        ],
        "Oem": {
            "Hpe": {
                "PowerRegulationEnabled": false
            }
        }
    }"#;
    assert_eq!(
        parse_redfish_power(body, RedfishSchema::LegacyPower),
        ParseOutcome::Ok(412.5)
    );
}

#[test]
fn parses_openbmc_reference_response() {
    let body = r#"{
        "@odata.id": "/redfish/v1/Chassis/chassis/Power",
        "Id": "Power",
        "Name": "Power",
        "PowerControl": [
            {
                "@odata.id": "/redfish/v1/Chassis/chassis/Power#/PowerControl/0",
                "MemberId": "0",
                "Name": "Chassis Power Control",
                "PowerConsumedWatts": 198.4
            }
        ]
    }"#;
    assert_eq!(
        parse_redfish_power(body, RedfishSchema::LegacyPower),
        ParseOutcome::Ok(198.4)
    );
}

#[test]
fn rejects_dell_response_in_transition_state() {
    // Some Dell iDRACs return null while the BMC reinitializes.
    let body = r#"{
        "PowerControl": [
            {
                "MemberId": "0",
                "PowerConsumedWatts": null
            }
        ]
    }"#;
    assert_eq!(
        parse_redfish_power(body, RedfishSchema::LegacyPower),
        ParseOutcome::InvalidValue
    );
}

#[test]
fn rejects_empty_power_control_array() {
    let body = r#"{"PowerControl": []}"#;
    assert_eq!(
        parse_redfish_power(body, RedfishSchema::LegacyPower),
        ParseOutcome::PathMissing
    );
}

// `custom_power_path_resolves_for_oem_vendors` removed in v0.7.6:
// arbitrary JSON pointers are no longer configurable. An OEM that
// exposes wattage under a non-standard path is expected to either
// surface a Redfish-compliant `/Power` or `/EnvironmentMetrics` on
// its own URL, or be fronted by a reverse proxy that reshapes the
// payload. See docs/LIMITATIONS.md for the rationale.

// --- multi-chassis attribution --------------------------------------

#[test]
fn multi_chassis_each_gets_independent_coefficient() {
    // Two chassis, each with its own service set and its own wattage.
    // The scraper hoists state.current_owned()/publish() out of the
    // loop, so the test mimics that by sharing a single `next` buffer
    // across two apply_chassis_scrape calls.
    let mut next: HashMap<String, ServiceEnergy> = HashMap::new();
    let chassis_1 = services(&["svc-a"]);
    let chassis_2 = services(&["svc-b"]);
    let deltas = ops(&[("svc-a", 100), ("svc-b", 200)]);
    assert!(apply_chassis_scrape(
        &mut next, &chassis_1, 360.0, 10.0, &deltas, 1000,
    ));
    assert!(apply_chassis_scrape(
        &mut next, &chassis_2, 720.0, 10.0, &deltas, 1000,
    ));
    assert_eq!(next.len(), 2);
    let a = (360.0 * 10.0 / 3_600_000.0) / 100.0;
    let b = (720.0 * 10.0 / 3_600_000.0) / 200.0;
    assert!((next["svc-a"].energy_per_op_kwh - a).abs() < 1e-15);
    assert!((next["svc-b"].energy_per_op_kwh - b).abs() < 1e-15);
}

// --- ca_bundle_path fail-loud regression ----------------------------

#[tokio::test]
async fn spawn_scraper_with_ca_bundle_path_aborts_immediately() {
    use super::config::{RedfishConfig, RedfishEndpoint};
    use super::scraper::spawn_scraper;
    use crate::report::metrics::MetricsState;
    use crate::score::redfish::RedfishState;
    use std::sync::Arc;
    use std::time::Duration;

    let mut endpoints = HashMap::new();
    endpoints.insert(
        "chassis-1".to_string(),
        RedfishEndpoint {
            url: "https://127.0.0.1:12345/redfish/v1/Chassis/1/Power".to_string(),
            schema: RedfishSchema::LegacyPower,
        },
    );
    let mut mappings = HashMap::new();
    mappings.insert("svc-a".to_string(), "chassis-1".to_string());
    let cfg = RedfishConfig {
        endpoints,
        scrape_interval: Duration::from_secs(15),
        service_mappings: mappings,
        ca_bundle_path: Some("/tmp/perf-sentinel-fake-ca-bundle.pem".to_string()),
        auth_header: None,
    };
    let state = RedfishState::new();
    let metrics = Arc::new(MetricsState::new());
    let handle = spawn_scraper(cfg, state, metrics);
    // The scraper must exit cleanly before the first scrape tick.
    tokio::time::timeout(Duration::from_millis(500), handle)
        .await
        .expect("scraper should exit fast on ca_bundle_path")
        .expect("scraper task should complete without panic");
}

#[tokio::test]
async fn spawn_scraper_staleness_gauge_climbs_when_every_chassis_fails() {
    // Regression guard: the gauge must climb from boot when every
    // chassis is unreachable. Before the fix, last_success_ms was
    // None at boot and the gauge stayed at 0.0 indefinitely.
    use super::config::{RedfishConfig, RedfishEndpoint};
    use super::scraper::spawn_scraper;
    use crate::report::metrics::MetricsState;
    use crate::score::redfish::RedfishState;
    use std::sync::Arc;
    use std::time::Duration;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let mut endpoints = HashMap::new();
    endpoints.insert(
        "chassis-1".to_string(),
        RedfishEndpoint {
            url: format!("http://{addr}/Power"),
            schema: RedfishSchema::LegacyPower,
        },
    );
    let mut mappings = HashMap::new();
    mappings.insert("svc-a".to_string(), "chassis-1".to_string());
    // Sub-second interval lets the test fail several ticks within a
    // reasonable window. Config-load validation would reject this
    // value (clamp is [15, 3600] s) but the typed struct is built
    // directly here, bypassing that gate.
    let cfg = RedfishConfig {
        endpoints,
        scrape_interval: Duration::from_millis(50),
        service_mappings: mappings,
        ca_bundle_path: None,
        auth_header: None,
    };
    let state = RedfishState::new();
    let metrics = Arc::new(MetricsState::new());
    let handle = spawn_scraper(cfg, state, metrics.clone());

    // Poll until the staleness gauge starts climbing rather than waiting a
    // fixed window. On Linux the scrape fails fast (connection refused) within
    // a tick, but on Windows connecting to the dropped port can take until the
    // fetch timeout (5s) to fail, so a fixed 300ms wait flakes. Break as soon
    // as the gauge moves; the budget only has to exceed the fetch timeout.
    let mut age = 0.0;
    for _ in 0..320 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        age = metrics.redfish_last_scrape_age_seconds.get();
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
