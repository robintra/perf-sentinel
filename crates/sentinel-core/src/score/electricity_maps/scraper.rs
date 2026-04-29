//! Electricity Maps API scraper: periodic polling for real-time
//! carbon intensity per zone.
//!
//! Follows the same pattern as the Scaphandre and cloud energy
//! scrapers: background tokio task, publish-to-state, warn-once on
//! failure.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use crate::http_client;
use crate::text_safety::sanitize_for_terminal;

use super::config::{ApiVersion, ElectricityMapsConfig, EmissionFactorType, TemporalGranularity};
use super::state::{ElectricityMapsState, IntensityReading, monotonic_ms};

/// Maximum body size for API responses (1 MiB, smaller than the shared
/// 8 MiB constant since API responses are tiny JSON objects).
const MAX_API_BODY_BYTES: usize = 1024 * 1024;

/// Request timeout for API calls.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Number of consecutive failures before a diagnostic warning.
const FAILURE_THRESHOLD: u32 = 3;

/// Maximum byte length we accept for the optional `estimationMethod`
/// string from the API. Real values today (`"TIME_SLICER_AVERAGE"`,
/// `"GENERAL_PURPOSE_ZONE_DEVELOPMENT"`) sit well below 64 bytes. The
/// cap prevents a future API regression (or a compromised endpoint)
/// from inflating per-region rows that get replicated across every
/// JSON snapshot the daemon serves.
const MAX_ESTIMATION_METHOD_LEN: usize = 64;

// ---------------------------------------------------------------
// Error type
// ---------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
enum EmapsScraperError {
    #[error("invalid API URI: {0}")]
    InvalidUri(String),
    #[error("HTTP transport error")]
    Transport(#[source] hyper_util::client::legacy::Error),
    #[error("API body read failed: {0}")]
    BodyRead(String),
    #[error("API returned HTTP {0}")]
    HttpStatus(u16),
    #[error("API request timed out (5s)")]
    Timeout,
    #[error("JSON parse error: {0}")]
    JsonParse(String),
}

// ---------------------------------------------------------------
// API response type
// ---------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CarbonIntensityResponse {
    #[serde(rename = "carbonIntensity")]
    carbon_intensity: f64,
    /// Whether the value was estimated rather than measured. Documented
    /// at <https://app.electricitymaps.com/developer-hub/api/getting-started>
    /// (Estimations section). Optional with `#[serde(default)]` so the
    /// scraper survives API version changes that omit the field.
    #[serde(default, rename = "isEstimated")]
    is_estimated: Option<bool>,
    /// Estimation algorithm tag, e.g. `"TIME_SLICER_AVERAGE"`. Optional
    /// for the same forward-compatibility reason.
    #[serde(default, rename = "estimationMethod")]
    estimation_method: Option<String>,
}

/// One successful API response, normalized for downstream use.
#[derive(Debug)]
struct FetchedReading {
    gco2_per_kwh: f64,
    is_estimated: Option<bool>,
    estimation_method: Option<String>,
}

// ---------------------------------------------------------------
// Scraper
// ---------------------------------------------------------------

/// Spawn the Electricity Maps scraper as a background tokio task.
///
/// Returns a `JoinHandle` for shutdown abort.
#[must_use]
pub fn spawn_electricity_maps_scraper(
    config: ElectricityMapsConfig,
    state: Arc<ElectricityMapsState>,
) -> tokio::task::JoinHandle<()> {
    warn_if_legacy_v3_endpoint(&config.api_endpoint);
    tokio::spawn(run_scraper_loop(config, state))
}

/// Emit a deprecation warning when the configured endpoint targets the
/// legacy `Electricity Maps` v3 API. Called once per daemon startup
/// from `spawn_electricity_maps_scraper`, not per scrape. The endpoint
/// is sanitized through `sanitize_for_terminal` before logging because
/// it originates from operator-supplied TOML and could otherwise carry
/// ANSI / OSC 8 / control bytes into the log stream. Detection delegates
/// to `ApiVersion::from_endpoint`, single source of truth.
fn warn_if_legacy_v3_endpoint(endpoint: &str) {
    if ApiVersion::from_endpoint(endpoint) == ApiVersion::V3 {
        let safe_endpoint = sanitize_for_terminal(endpoint);
        tracing::warn!(
            endpoint = %safe_endpoint,
            "Electricity Maps endpoint configured with legacy /v3 path. \
             v3 is still supported but in legacy mode. Migrate to v4 by \
             setting `endpoint = \"https://api.electricitymaps.com/v4\"` \
             in your perf-sentinel TOML config. \
             See https://app.electricitymaps.com/developer-hub/api/reference"
        );
    }
}

async fn run_scraper_loop(config: ElectricityMapsConfig, state: Arc<ElectricityMapsState>) {
    let client = http_client::build_client();
    let mut ticker = tokio::time::interval(config.poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut consecutive_failures: u32 = 0;

    loop {
        ticker.tick().await;
        let any_success = run_one_tick(&client, &config, &state).await;
        update_failure_counter(&mut consecutive_failures, any_success);
    }
}

/// Execute one polling tick. Returns `true` when at least one unique
/// zone returned a fresh reading and the new state table was published,
/// `false` when no zones were configured or every fetch failed.
///
/// Deduplicates zones so a `region_map` mapping multiple `cloud_region`
/// keys to the same zone (two AZs in the same country, or an AWS region
/// and a local-k3d cluster both pinned to `FR`) only spends one API
/// call per zone per tick. Critical on quota-constrained tiers.
/// `cloud_region` keys sharing the same zone are atomically updated
/// together: either all of them get the fresh reading or none do, the
/// previous reading is preserved by the `current_owned + insert-only-
/// on-success` pattern.
async fn run_one_tick(
    client: &http_client::HttpClient,
    config: &ElectricityMapsConfig,
    state: &ElectricityMapsState,
) -> bool {
    // BTreeSet for deterministic iteration order (stable test snapshots).
    let unique_zones: BTreeSet<&str> = config.region_map.values().map(String::as_str).collect();

    // Empty `region_map`: skip without recording a failure. Otherwise
    // an unconfigured map would eventually fire the diagnostic warn
    // even though no API call was attempted.
    if unique_zones.is_empty() {
        return false;
    }

    let zone_readings = fetch_zones(client, config, &unique_zones).await;
    if zone_readings.is_empty() {
        return false;
    }

    let now = monotonic_ms();
    let mut new_table = state.current_owned();
    dispatch_readings(&zone_readings, &config.region_map, now, &mut new_table);
    state.publish(new_table);
    true
}

/// Fetch every unique zone in `unique_zones`. Returns a map keyed by
/// zone code containing only the zones that returned 200 + valid JSON.
async fn fetch_zones<'a>(
    client: &http_client::HttpClient,
    config: &ElectricityMapsConfig,
    unique_zones: &BTreeSet<&'a str>,
) -> HashMap<&'a str, FetchedReading> {
    let mut zone_readings: HashMap<&str, FetchedReading> =
        HashMap::with_capacity(unique_zones.len());
    for zone in unique_zones {
        match fetch_intensity(
            client,
            &config.api_endpoint,
            &config.auth_token,
            zone,
            config.emission_factor_type,
            config.temporal_granularity,
        )
        .await
        {
            Ok(reading) => {
                tracing::debug!(
                    zone = %zone,
                    intensity = reading.gco2_per_kwh,
                    "Electricity Maps: fetched intensity"
                );
                zone_readings.insert(*zone, reading);
            }
            Err(e) => {
                tracing::debug!(
                    zone = %zone,
                    error = %e,
                    "Electricity Maps: failed to fetch intensity"
                );
            }
        }
    }
    zone_readings
}

/// Spread each successful zone reading across every `cloud_region`
/// mapped to that zone. Mutates `new_table` in place so the caller can
/// publish the resulting snapshot.
fn dispatch_readings(
    zone_readings: &HashMap<&str, FetchedReading>,
    region_map: &HashMap<String, String>,
    now: u64,
    new_table: &mut HashMap<String, IntensityReading>,
) {
    for (cloud_region, zone) in region_map {
        if let Some(reading) = zone_readings.get(zone.as_str()) {
            new_table.insert(
                cloud_region.clone(),
                IntensityReading {
                    gco2_per_kwh: reading.gco2_per_kwh,
                    last_update_ms: now,
                    is_estimated: reading.is_estimated,
                    estimation_method: reading.estimation_method.clone(),
                },
            );
        }
    }
}

/// Reset on success, increment otherwise. Emit the diagnostic warn
/// once when the threshold is hit. The counter is zone-set-level: a
/// partial-success tick (zone FR ok, zone DE ko) resets it because at
/// least one zone returned data. Only a tick where all unique zones
/// fail will increment.
fn update_failure_counter(consecutive_failures: &mut u32, any_success: bool) {
    if any_success {
        *consecutive_failures = 0;
        return;
    }
    *consecutive_failures = consecutive_failures.saturating_add(1);
    if *consecutive_failures == FAILURE_THRESHOLD {
        tracing::warn!(
            failures = *consecutive_failures,
            "Electricity Maps: {} consecutive failures, \
             falling back to embedded profiles",
            *consecutive_failures
        );
    }
}

/// Compose the `carbon-intensity/latest` request URL. The optional
/// query params are only appended when they differ from the API
/// defaults (`lifecycle` / `hourly`), so the wire stays exactly
/// as-was for users who have not opted into the knobs.
fn build_request_url(
    api_endpoint: &str,
    zone: &str,
    emission_factor_type: EmissionFactorType,
    temporal_granularity: TemporalGranularity,
) -> String {
    let mut uri_str = format!("{api_endpoint}/carbon-intensity/latest?zone={zone}");
    if emission_factor_type != EmissionFactorType::default() {
        uri_str.push_str("&emissionFactorType=");
        uri_str.push_str(emission_factor_type.as_query_value());
    }
    if temporal_granularity != TemporalGranularity::default() {
        uri_str.push_str("&temporalGranularity=");
        uri_str.push_str(temporal_granularity.as_query_value());
    }
    uri_str
}

async fn fetch_intensity(
    client: &http_client::HttpClient,
    api_endpoint: &str,
    auth_token: &str,
    zone: &str,
    emission_factor_type: EmissionFactorType,
    temporal_granularity: TemporalGranularity,
) -> Result<FetchedReading, EmapsScraperError> {
    let uri_str = build_request_url(
        api_endpoint,
        zone,
        emission_factor_type,
        temporal_granularity,
    );
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|e: hyper::http::uri::InvalidUri| EmapsScraperError::InvalidUri(e.to_string()))?;

    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(&uri)
        .header("auth-token", auth_token)
        .header("User-Agent", "perf-sentinel")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| EmapsScraperError::BodyRead(e.to_string()))?;

    let resp = tokio::time::timeout(REQUEST_TIMEOUT, client.request(req))
        .await
        .map_err(|_| EmapsScraperError::Timeout)?
        .map_err(EmapsScraperError::Transport)?;

    let status = resp.status().as_u16();
    if status != 200 {
        tracing::debug!(
            status,
            endpoint = %http_client::redact_endpoint(&uri),
            "Electricity Maps: non-200 response"
        );
        return Err(EmapsScraperError::HttpStatus(status));
    }

    let limited = http_body_util::Limited::new(resp.into_body(), MAX_API_BODY_BYTES);
    let body = http_body_util::BodyExt::collect(limited)
        .await
        .map_err(|e| EmapsScraperError::BodyRead(e.to_string()))?
        .to_bytes();

    let text =
        std::str::from_utf8(&body).map_err(|e| EmapsScraperError::BodyRead(e.to_string()))?;

    let response: CarbonIntensityResponse =
        serde_json::from_str(text).map_err(|e| EmapsScraperError::JsonParse(e.to_string()))?;

    if !response.carbon_intensity.is_finite() || response.carbon_intensity < 0.0 {
        return Err(EmapsScraperError::JsonParse(format!(
            "invalid carbon intensity value: {}",
            response.carbon_intensity
        )));
    }

    Ok(FetchedReading {
        gco2_per_kwh: response.carbon_intensity,
        is_estimated: response.is_estimated,
        estimation_method: response
            .estimation_method
            .and_then(sanitize_estimation_method),
    })
}

/// Drop the `estimationMethod` value if it exceeds the size cap or
/// contains control characters. Returning `None` is safe because the
/// field is purely informative and the rest of the pipeline already
/// treats `None` as "no estimation tag available".
fn sanitize_estimation_method(s: String) -> Option<String> {
    if s.len() > MAX_ESTIMATION_METHOD_LEN {
        return None;
    }
    if s.chars().any(char::is_control) {
        return None;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only wrapper that calls the real `fetch_intensity` with the
    /// API knobs at their defaults. Keeps the per-test call sites
    /// short, the knob behaviour itself is exercised separately via the
    /// pure `build_request_url` helper.
    async fn fetch_intensity_test(
        client: &http_client::HttpClient,
        api_endpoint: &str,
        auth_token: &str,
        zone: &str,
    ) -> Result<FetchedReading, EmapsScraperError> {
        fetch_intensity(
            client,
            api_endpoint,
            auth_token,
            zone,
            EmissionFactorType::default(),
            TemporalGranularity::default(),
        )
        .await
    }

    #[test]
    fn build_request_url_omits_query_params_when_defaults_used() {
        // Backward-compat: any user not setting the knobs sees the
        // exact same URL shape as pre-0.5.11.
        let url = build_request_url(
            "https://api.electricitymaps.com/v4",
            "FR",
            EmissionFactorType::Lifecycle,
            TemporalGranularity::Hourly,
        );
        assert_eq!(
            url,
            "https://api.electricitymaps.com/v4/carbon-intensity/latest?zone=FR"
        );
    }

    #[test]
    fn build_request_url_appends_emission_factor_type_when_direct() {
        let url = build_request_url(
            "https://api.electricitymaps.com/v4",
            "FR",
            EmissionFactorType::Direct,
            TemporalGranularity::Hourly,
        );
        assert_eq!(
            url,
            "https://api.electricitymaps.com/v4/carbon-intensity/latest?zone=FR&emissionFactorType=direct"
        );
    }

    #[test]
    fn build_request_url_appends_temporal_granularity_when_sub_hour() {
        let url = build_request_url(
            "https://api.electricitymaps.com/v4",
            "FR",
            EmissionFactorType::Lifecycle,
            TemporalGranularity::FiveMinutes,
        );
        assert_eq!(
            url,
            "https://api.electricitymaps.com/v4/carbon-intensity/latest?zone=FR&temporalGranularity=5_minutes"
        );
    }

    #[test]
    fn build_request_url_appends_both_knobs_when_both_non_default() {
        let url = build_request_url(
            "https://api.electricitymaps.com/v4",
            "DE",
            EmissionFactorType::Direct,
            TemporalGranularity::FifteenMinutes,
        );
        assert_eq!(
            url,
            "https://api.electricitymaps.com/v4/carbon-intensity/latest?zone=DE&emissionFactorType=direct&temporalGranularity=15_minutes"
        );
    }

    #[test]
    fn parse_valid_response() {
        let json = r#"{"zone":"FR","carbonIntensity":56.0,"datetime":"2025-01-01T12:00:00Z"}"#;
        let resp: CarbonIntensityResponse = serde_json::from_str(json).unwrap();
        assert!((resp.carbon_intensity - 56.0).abs() < 1e-10);
    }

    #[test]
    fn parse_response_missing_field() {
        let json = r#"{"zone":"FR"}"#;
        let result: Result<CarbonIntensityResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // Integration tests with a mock HTTP server on an ephemeral port
    // ---------------------------------------------------------------
    //
    // The mock server helpers live in `crate::test_helpers` and are
    // shared with the scaphandre, cloud_energy, and tempo tests, one
    // implementation of "bind ephemeral port + one-shot reply".

    use crate::test_helpers::{http_200_text, http_status, spawn_one_shot_server};

    /// Wrap the shared `http_200_text` with the JSON content type.
    fn http_200(body: &str) -> Vec<u8> {
        http_200_text("application/json", body)
    }

    #[tokio::test]
    async fn fetch_intensity_success_happy_path() {
        let body = r#"{"zone":"FR","carbonIntensity":56.0,"datetime":"2025-01-01T12:00:00Z"}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200(body)).await;

        let client = http_client::build_client();
        let reading = fetch_intensity_test(&client, &endpoint, "test-token", "FR")
            .await
            .expect("200 + valid JSON should succeed");
        assert!((reading.gco2_per_kwh - 56.0).abs() < 1e-10);
        assert_eq!(reading.is_estimated, None);
        assert_eq!(reading.estimation_method, None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_parses_estimation_metadata_when_present() {
        let body = r#"{"zone":"DE","carbonIntensity":380.0,"isEstimated":true,"estimationMethod":"TIME_SLICER_AVERAGE"}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200(body)).await;

        let client = http_client::build_client();
        let reading = fetch_intensity_test(&client, &endpoint, "tok", "DE")
            .await
            .expect("200 + valid JSON should succeed");
        assert_eq!(reading.is_estimated, Some(true));
        assert_eq!(
            reading.estimation_method.as_deref(),
            Some("TIME_SLICER_AVERAGE")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_handles_explicit_measured_flag() {
        let body = r#"{"zone":"FR","carbonIntensity":56.0,"isEstimated":false}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200(body)).await;

        let client = http_client::build_client();
        let reading = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect("200 + valid JSON should succeed");
        assert_eq!(reading.is_estimated, Some(false));
        assert_eq!(reading.estimation_method, None);
        server.await.unwrap();
    }

    #[test]
    fn sanitize_estimation_method_drops_oversized_strings() {
        let too_long = "X".repeat(MAX_ESTIMATION_METHOD_LEN + 1);
        assert_eq!(sanitize_estimation_method(too_long), None);
    }

    #[test]
    fn sanitize_estimation_method_drops_control_characters() {
        assert_eq!(
            sanitize_estimation_method("FOO\nBAR".to_string()),
            None,
            "newline must be rejected to prevent log forging"
        );
        assert_eq!(
            sanitize_estimation_method("FOO\x1b[31mBAR".to_string()),
            None,
            "ANSI escape must be rejected"
        );
    }

    #[test]
    fn sanitize_estimation_method_preserves_realistic_values() {
        for v in [
            "TIME_SLICER_AVERAGE",
            "GENERAL_PURPOSE_ZONE_DEVELOPMENT",
            "FUTURE_ALGO_42",
        ] {
            assert_eq!(
                sanitize_estimation_method(v.to_string()).as_deref(),
                Some(v)
            );
        }
    }

    #[tokio::test]
    async fn fetch_intensity_drops_oversized_estimation_method() {
        let big = "X".repeat(MAX_ESTIMATION_METHOD_LEN + 10);
        let body = format!(
            r#"{{"zone":"FR","carbonIntensity":56.0,"isEstimated":true,"estimationMethod":"{big}"}}"#
        );
        let (endpoint, server) = spawn_one_shot_server(http_200(&body)).await;

        let client = http_client::build_client();
        let reading = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect("oversized method must be sanitized, not rejected");
        assert_eq!(reading.is_estimated, Some(true));
        assert_eq!(reading.estimation_method, None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_v3_and_v4_responses_parse_identically() {
        // Wire-format parity guard: the CHANGELOG and design doc both
        // claim the v3 and v4 `carbon-intensity/latest` responses are
        // schema-identical. Lock the contract: the same response body
        // produces the same `FetchedReading` regardless of which API
        // path the daemon was configured against. A future v5 default
        // flip silently breaking the parser would fail this test.
        let body = r#"{"zone":"FR","carbonIntensity":56.0,"isEstimated":true,"estimationMethod":"TIME_SLICER_AVERAGE","datetime":"2026-04-27T12:00:00Z"}"#;
        let (v3_endpoint, v3_server) = spawn_one_shot_server(http_200(body)).await;
        let (v4_endpoint, v4_server) = spawn_one_shot_server(http_200(body)).await;

        let client = http_client::build_client();
        let v3_reading = fetch_intensity_test(&client, &v3_endpoint, "tok", "FR")
            .await
            .expect("v3 mock must succeed");
        let v4_reading = fetch_intensity_test(&client, &v4_endpoint, "tok", "FR")
            .await
            .expect("v4 mock must succeed");

        assert!((v3_reading.gco2_per_kwh - v4_reading.gco2_per_kwh).abs() < 1e-10);
        assert_eq!(v3_reading.is_estimated, v4_reading.is_estimated);
        assert_eq!(v3_reading.estimation_method, v4_reading.estimation_method);

        v3_server.await.unwrap();
        v4_server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_accepts_unknown_estimation_method_string() {
        // Defensive: don't hardcode a whitelist of estimation methods. The
        // API may evolve. We pass the method through verbatim and let
        // consumers decide what to render.
        let body = r#"{"zone":"FR","carbonIntensity":56.0,"isEstimated":true,"estimationMethod":"FUTURE_ALGO_42"}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200(body)).await;

        let client = http_client::build_client();
        let reading = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect("200 + valid JSON should succeed");
        assert_eq!(reading.is_estimated, Some(true));
        assert_eq!(reading.estimation_method.as_deref(), Some("FUTURE_ALGO_42"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_rejects_non_200_with_http_status_error() {
        let (endpoint, server) = spawn_one_shot_server(http_status(401, "Unauthorized")).await;

        let client = http_client::build_client();
        let err = fetch_intensity_test(&client, &endpoint, "bad-token", "FR")
            .await
            .expect_err("401 must surface as HttpStatus");
        match err {
            EmapsScraperError::HttpStatus(401) => {}
            other => panic!("expected HttpStatus(401), got {other:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_rejects_server_error() {
        let (endpoint, server) =
            spawn_one_shot_server(http_status(503, "Service Unavailable")).await;

        let client = http_client::build_client();
        let err = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect_err("503 must surface as HttpStatus");
        match err {
            EmapsScraperError::HttpStatus(503) => {}
            other => panic!("expected HttpStatus(503), got {other:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_rejects_malformed_json() {
        let (endpoint, server) = spawn_one_shot_server(http_200("not json at all")).await;

        let client = http_client::build_client();
        let err = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect_err("malformed JSON must surface as JsonParse");
        assert!(matches!(err, EmapsScraperError::JsonParse(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_rejects_json_without_carbon_intensity_field() {
        let (endpoint, server) = spawn_one_shot_server(http_200(r#"{"zone":"FR"}"#)).await;

        let client = http_client::build_client();
        let err = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect_err("missing field must surface as JsonParse");
        assert!(matches!(err, EmapsScraperError::JsonParse(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_intensity_rejects_negative_carbon_intensity() {
        // Valid schema, but the value is negative, the API should never
        // return this, but we validate defensively to avoid silently
        // flipping the sign of CO₂ estimates.
        let body = r#"{"carbonIntensity":-5.0}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200(body)).await;

        let client = http_client::build_client();
        let err = fetch_intensity_test(&client, &endpoint, "tok", "FR")
            .await
            .expect_err("negative intensity must be rejected");
        match err {
            EmapsScraperError::JsonParse(msg) => {
                assert!(msg.contains("invalid carbon intensity"));
            }
            other => panic!("expected JsonParse, got {other:?}"),
        }
        server.await.unwrap();
    }

    // NaN coverage: serde_json rejects bare `NaN` per JSON spec, so the
    // `is_finite()` guard in fetch_intensity is belt-and-braces. The
    // `fetch_intensity_rejects_negative_carbon_intensity` test above
    // exercises the same JsonParse arm, which is the actual coverage
    // target.

    #[tokio::test]
    async fn fetch_intensity_rejects_invalid_uri() {
        // Garbage endpoint, hits the `InvalidUri` error variant.
        let client = http_client::build_client();
        let err = fetch_intensity_test(&client, "not a uri :: bad", "tok", "FR")
            .await
            .expect_err("invalid URI must surface as InvalidUri");
        assert!(matches!(err, EmapsScraperError::InvalidUri(_)));
    }

    #[test]
    fn emaps_scraper_error_display_messages_are_informative() {
        // Smoke test for the `thiserror` derive: each variant has a
        // unique, user-facing message so operators can tell error
        // categories apart in logs.
        let e1 = EmapsScraperError::InvalidUri("bad".to_string());
        let e2 = EmapsScraperError::BodyRead("oops".to_string());
        let e3 = EmapsScraperError::HttpStatus(429);
        let e4 = EmapsScraperError::Timeout;
        let e5 = EmapsScraperError::JsonParse("nope".to_string());
        assert!(format!("{e1}").contains("invalid API URI"));
        assert!(format!("{e2}").contains("body read"));
        assert!(format!("{e3}").contains("429"));
        assert!(format!("{e4}").contains("timed out"));
        assert!(format!("{e5}").contains("JSON parse"));
    }

    /// Smoke test for `spawn_electricity_maps_scraper`: it must return a
    /// `JoinHandle` and not panic during task construction. The loop
    /// then polls an unreachable endpoint on the first tick; we abort
    /// immediately so the test doesn't hang.
    #[tokio::test]
    async fn spawn_scraper_returns_joinhandle_and_aborts_cleanly() {
        let mut region_map = HashMap::new();
        region_map.insert("eu-west-3".to_string(), "FR".to_string());
        let config = ElectricityMapsConfig {
            api_endpoint: "http://127.0.0.1:1".to_string(), // unreachable
            auth_token: "tok".to_string(),
            poll_interval: std::time::Duration::from_hours(1), // never ticks during the test
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let state = Arc::new(ElectricityMapsState::default());
        let handle = spawn_electricity_maps_scraper(config, state);
        // Give the task a moment to start its initial tick setup, then abort.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        handle.abort();
        // Aborted tasks return JoinError::Cancelled; awaiting them must
        // not panic from our side.
        let _ = handle.await;
    }

    // ---------------------------------------------------------------
    // Zone deduplication regression test
    // ---------------------------------------------------------------

    /// Bind an ephemeral TCP port and serve a per-zone JSON response on
    /// every accepted connection. Counts the number of accepted requests
    /// so the test can assert "one API call per unique zone, not one
    /// per `cloud_region`". `responses` maps `?zone=XX` query value to
    /// the JSON body to return.
    ///
    /// Zones absent from `responses` get an HTTP 503. Callers that map
    /// every zone in their `region_map` into `responses` see no 503,
    /// callers that intentionally omit a zone (partial-failure tests)
    /// rely on this fallback to simulate a per-zone API failure.
    async fn spawn_counting_server(
        responses: HashMap<String, String>,
    ) -> (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://{addr}");
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let counter = counter_clone.clone();
                let responses = responses.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let req = std::str::from_utf8(&buf[..n]).unwrap_or("");
                    // Naive request line parse: extract the zone query
                    // parameter from "GET /carbon-intensity/latest?zone=XX HTTP/1.1".
                    let zone = req
                        .lines()
                        .next()
                        .and_then(|line| line.split("zone=").nth(1))
                        .and_then(|tail| tail.split_whitespace().next())
                        .unwrap_or("");
                    // Zones not in `responses` get a 503 so partial-failure
                    // tests can simulate "FR ok, DE ko" by listing only FR.
                    let resp = match responses.get(zone) {
                        Some(body) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        ),
                        None => "HTTP/1.1 503 Service Unavailable\r\n\
                                 Content-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string(),
                    };
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (endpoint, counter, handle)
    }

    #[tokio::test]
    async fn run_scraper_loop_dedups_zones_when_cloud_regions_share_zone() {
        // 3 cloud_regions, 2 unique zones (FR, DE). Verify only 2 API
        // calls per tick, and that both FR cloud_regions end up with
        // the same intensity in the published state.
        let mut responses = HashMap::new();
        responses.insert(
            "FR".to_string(),
            r#"{"zone":"FR","carbonIntensity":56.0}"#.to_string(),
        );
        responses.insert(
            "DE".to_string(),
            r#"{"zone":"DE","carbonIntensity":380.0}"#.to_string(),
        );
        let (endpoint, counter, server_handle) = spawn_counting_server(responses).await;

        let mut region_map = HashMap::new();
        region_map.insert("aws:eu-west-3".to_string(), "FR".to_string());
        region_map.insert("local-k3d".to_string(), "FR".to_string());
        region_map.insert("aws:eu-central-1".to_string(), "DE".to_string());

        let config = ElectricityMapsConfig {
            api_endpoint: endpoint,
            auth_token: "tok".to_string(),
            // Poll interval larger than the wait below so exactly one
            // tick fires (tokio::time::interval emits the first tick
            // immediately, then every poll_interval).
            poll_interval: std::time::Duration::from_mins(1),
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let state = ElectricityMapsState::new();
        let scraper_handle = spawn_electricity_maps_scraper(config, state.clone());

        // One tick fires immediately, then again after the interval.
        // Wait long enough for the first tick to complete.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        scraper_handle.abort();
        server_handle.abort();

        // Exactly 2 API calls, one per unique zone, despite 3 region_map entries.
        let count = counter.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            count, 2,
            "expected 2 API calls (one per unique zone), got {count}"
        );

        // Both FR cloud_regions resolve to the same intensity; DE differs.
        let snap = state.snapshot(monotonic_ms() + 1_000_000, u64::MAX);
        assert!((snap["aws:eu-west-3"] - 56.0).abs() < 1e-10);
        assert!((snap["local-k3d"] - 56.0).abs() < 1e-10);
        assert!((snap["aws:eu-central-1"] - 380.0).abs() < 1e-10);
    }

    #[tokio::test]
    async fn run_scraper_loop_propagates_estimation_metadata_into_state() {
        // Verify the dedup-then-dispatch path forwards is_estimated /
        // estimation_method from the API response into every cloud_region
        // mapped to that zone.
        let mut responses = HashMap::new();
        responses.insert(
            "FR".to_string(),
            r#"{"zone":"FR","carbonIntensity":56.0,"isEstimated":true,"estimationMethod":"TIME_SLICER_AVERAGE"}"#
                .to_string(),
        );
        let (endpoint, _counter, server_handle) = spawn_counting_server(responses).await;

        let mut region_map = HashMap::new();
        region_map.insert("aws:eu-west-3".to_string(), "FR".to_string());
        region_map.insert("local-k3d".to_string(), "FR".to_string());

        let config = ElectricityMapsConfig {
            api_endpoint: endpoint,
            auth_token: "tok".to_string(),
            // Poll interval larger than the wait below so exactly one
            // tick fires (tokio::time::interval emits the first tick
            // immediately, then every poll_interval).
            poll_interval: std::time::Duration::from_mins(1),
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let state = ElectricityMapsState::new();
        let scraper_handle = spawn_electricity_maps_scraper(config, state.clone());

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        scraper_handle.abort();
        server_handle.abort();

        let snap = state.snapshot_with_metadata(monotonic_ms() + 1_000_000, u64::MAX);
        for region in ["aws:eu-west-3", "local-k3d"] {
            let entry = snap.get(region).expect("region present");
            assert_eq!(entry.is_estimated, Some(true));
            assert_eq!(
                entry.estimation_method.as_deref(),
                Some("TIME_SLICER_AVERAGE")
            );
        }
    }

    #[tokio::test]
    async fn run_scraper_loop_publishes_state_when_some_zones_succeed_and_others_fail() {
        // 0.5.9 partial-success regression guard. Two unique zones, FR
        // returns 200 (success), DE returns 503 (failure). The 0.5.9
        // contract: even if one zone fails, the loop still calls
        // `state.publish(new_table)` so successful zones land in the
        // snapshot, and (zone-set-level semantic) the
        // `consecutive_failures` counter is reset because at least
        // one zone returned data.
        //
        // What this test locks in: the publish-on-partial-success
        // observable. We pre-seed the state with a stale entry for
        // `aws:eu-central-1` so we can distinguish "publish ran and
        // preserved the stale entry via current_owned" from "publish
        // never ran and we observe leftover state from a different
        // path".
        //
        // What this test does NOT lock in directly: the
        // `consecutive_failures` counter value. The variable is
        // closure-local in `run_scraper_loop` and not exposed
        // anywhere observable. A future change that re-incremented
        // the counter on any per-zone failure (request-level
        // regression) would not fail this test. Capturing the
        // tracing::warn! emission would be the only direct way to
        // assert it, which would require a tracing-subscriber
        // dev-dependency we do not currently carry.
        let mut responses = HashMap::new();
        responses.insert(
            "FR".to_string(),
            r#"{"zone":"FR","carbonIntensity":56.0}"#.to_string(),
        );
        // DE intentionally absent, the helper returns 503 for it.
        let (endpoint, _counter, server_handle) = spawn_counting_server(responses).await;

        let mut region_map = HashMap::new();
        region_map.insert("aws:eu-west-3".to_string(), "FR".to_string());
        region_map.insert("aws:eu-central-1".to_string(), "DE".to_string());

        let state = ElectricityMapsState::new();

        // Pre-seed DE with a stale reading so we can prove the publish
        // path preserved it via `current_owned`. Without this seed,
        // a regression where `state.publish` is skipped would leave
        // the state empty and the "DE absent" assertion below would
        // still pass (degenerate-equal to "publish skipped").
        state.insert_for_test("aws:eu-central-1".into(), 999.0, 1);

        let config = ElectricityMapsConfig {
            api_endpoint: endpoint,
            auth_token: "tok".to_string(),
            poll_interval: std::time::Duration::from_mins(1),
            region_map,
            emission_factor_type: EmissionFactorType::default(),
            temporal_granularity: TemporalGranularity::default(),
        };
        let scraper_handle = spawn_electricity_maps_scraper(config, state.clone());

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        scraper_handle.abort();
        server_handle.abort();

        let snap = state.snapshot(monotonic_ms() + 1_000_000, u64::MAX);

        // FR succeeded, its cloud_region must carry the fresh value.
        let fr = snap
            .get("aws:eu-west-3")
            .copied()
            .expect("FR cloud_region must be present after a partial-success publish");
        assert!(
            (fr - 56.0).abs() < 1e-10,
            "FR cloud_region must carry the fresh 56.0 reading, got {fr}"
        );

        // DE failed, its pre-seeded stale value must be preserved by
        // the current_owned + insert-only-on-success pattern. This is
        // the "all-or-nothing per shared zone" invariant in action:
        // a failed zone keeps its previous reading, it is neither
        // wiped nor replaced with placeholder data.
        let de = snap
            .get("aws:eu-central-1")
            .copied()
            .expect("DE cloud_region must keep its pre-seeded stale value after a 503");
        assert!(
            (de - 999.0).abs() < 1e-10,
            "DE cloud_region must preserve the stale 999.0 reading after the 503, got {de}"
        );
    }
}
