//! Electricity Maps API scraper: periodic polling for real-time
//! carbon intensity per zone.
//!
//! Follows the same pattern as the Scaphandre and cloud energy
//! scrapers: background tokio task, publish-to-state, warn-once on
//! failure.

use std::sync::Arc;

use crate::http_client;

use super::config::ElectricityMapsConfig;
use super::state::{ElectricityMapsState, IntensityReading, monotonic_ms};

/// Maximum body size for API responses (1 MiB, smaller than the shared
/// 8 MiB constant since API responses are tiny JSON objects).
const MAX_API_BODY_BYTES: usize = 1024 * 1024;

/// Request timeout for API calls.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Number of consecutive failures before a diagnostic warning.
const FAILURE_THRESHOLD: u32 = 3;

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
    tokio::spawn(run_scraper_loop(config, state))
}

async fn run_scraper_loop(config: ElectricityMapsConfig, state: Arc<ElectricityMapsState>) {
    let client = http_client::build_client();
    let interval = config.poll_interval;
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut consecutive_failures: u32 = 0;

    loop {
        ticker.tick().await;

        let now = monotonic_ms();
        let mut new_table = state.current_owned();
        let mut any_success = false;

        for (cloud_region, zone) in &config.region_map {
            match fetch_intensity(&client, &config.api_endpoint, &config.auth_token, zone).await {
                Ok(intensity) => {
                    new_table.insert(
                        cloud_region.clone(),
                        IntensityReading {
                            gco2_per_kwh: intensity,
                            last_update_ms: now,
                        },
                    );
                    any_success = true;
                    tracing::debug!(
                        zone = %zone,
                        region = %cloud_region,
                        intensity,
                        "Electricity Maps: fetched intensity"
                    );
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

        if any_success {
            state.publish(new_table);
            consecutive_failures = 0;
        } else {
            consecutive_failures = consecutive_failures.saturating_add(1);
            if consecutive_failures == FAILURE_THRESHOLD {
                tracing::warn!(
                    failures = consecutive_failures,
                    "Electricity Maps: {} consecutive failures, \
                     falling back to embedded profiles",
                    consecutive_failures
                );
            }
        }
    }
}

async fn fetch_intensity(
    client: &http_client::HttpClient,
    api_endpoint: &str,
    auth_token: &str,
    zone: &str,
) -> Result<f64, EmapsScraperError> {
    let uri_str = format!("{api_endpoint}/carbon-intensity/latest?zone={zone}");
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

    Ok(response.carbon_intensity)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
