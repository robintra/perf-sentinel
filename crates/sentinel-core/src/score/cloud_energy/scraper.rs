//! Cloud energy scraper: Prometheus JSON API client and energy
//! computation.
//!
//! Queries the Prometheus `/api/v1/query` endpoint for CPU utilization
//! per service, interpolates watts via the `SPECpower` table, and
//! publishes per-service energy-per-op coefficients to the shared
//! [`CloudEnergyState`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::http_client::{self, HttpClient};
use crate::ingest::auth_header::{AuthHeader, parse_scraper_auth_header};
use crate::report::metrics::MetricsState;
use crate::score::scaphandre::ops::OpsSnapshotDiff;

use super::config::{CloudEnergyConfig, ServiceCloudConfig};
use super::state::{CloudEnergyState, ServiceEnergy, monotonic_ms};
use super::table;

/// Number of consecutive scrape failures before a diagnostic warning.
const FAILURE_THRESHOLD: u32 = 3;

/// Default `PromQL` queries per cloud provider when no custom
/// `cpu_metric` or `cpu_query` is configured.
const DEFAULT_CPU_QUERY_AWS: &str = "aws_ec2_cpuutilization_average";
const DEFAULT_CPU_QUERY_GCP: &str =
    "stackdriver_gce_instance_compute_googleapis_com_instance_cpu_utilization";
const DEFAULT_CPU_QUERY_AZURE: &str = "azure_compute_virtualmachines_percentage_cpu_average";
const DEFAULT_CPU_QUERY_GENERIC: &str =
    "100 - (avg by(instance)(rate(node_cpu_seconds_total{mode=\"idle\"}[5m])) * 100)";

// ---------------------------------------------------------------
// Error type
// ---------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub(super) enum CloudScraperError {
    #[error("invalid Prometheus endpoint URI '{endpoint}'")]
    InvalidUri {
        endpoint: String,
        #[source]
        source: hyper::http::uri::InvalidUri,
    },
    /// HTTP fetch failed. Delegates to the shared
    /// [`http_client::FetchError`].
    #[error("Prometheus fetch failed")]
    Fetch(#[source] http_client::FetchError),
    #[error("Prometheus response parse error: {0}")]
    JsonParse(String),
    #[error("Prometheus query returned no result for '{query}'")]
    EmptyResult { query: String },
    #[error("Prometheus query returned error status: {0}")]
    QueryError(String),
}

// ---------------------------------------------------------------
// Prometheus JSON API response types
// ---------------------------------------------------------------

#[derive(serde::Deserialize)]
struct PromResponse {
    status: String,
    data: Option<PromData>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct PromData {
    result: Vec<PromResult>,
}

/// A single Prometheus instant query result.
/// `value` is `[timestamp, "scalar_string"]`.
#[derive(serde::Deserialize)]
struct PromResult {
    value: (f64, String),
}

// ---------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------

/// Fetch a single `PromQL` instant query and return the scalar value.
async fn fetch_cpu_percent(
    client: &HttpClient,
    base_uri: &hyper::Uri,
    query: &str,
    auth: Option<&AuthHeader>,
) -> Result<f64, CloudScraperError> {
    let encoded_query = percent_encode_query(query);
    let path_and_query = format!("/api/v1/query?query={encoded_query}");

    let uri = hyper::Uri::builder()
        .scheme(base_uri.scheme_str().unwrap_or("http"))
        .authority(
            base_uri
                .authority()
                .map_or("localhost:9090", |a| a.as_str()),
        )
        .path_and_query(path_and_query)
        .build()
        .map_err(|e| CloudScraperError::Fetch(http_client::FetchError::RequestBuild(e)))?;

    let bytes = http_client::fetch_get(
        client,
        &uri,
        "perf-sentinel/cloud-energy-scraper",
        Duration::from_secs(5),
        auth,
    )
    .await
    .map_err(CloudScraperError::Fetch)?;

    let body_str = std::str::from_utf8(&bytes).map_err(|_| {
        CloudScraperError::Fetch(http_client::FetchError::BodyRead(
            "Prometheus response was not valid UTF-8".to_string(),
        ))
    })?;

    parse_prom_scalar(body_str, query)
}

/// Parse a Prometheus instant query JSON response and extract the
/// scalar value from the first result.
fn parse_prom_scalar(body: &str, query: &str) -> Result<f64, CloudScraperError> {
    let resp: PromResponse =
        serde_json::from_str(body).map_err(|e| CloudScraperError::JsonParse(format!("{e}")))?;

    if resp.status != "success" {
        return Err(CloudScraperError::QueryError(
            resp.error.unwrap_or(resp.status),
        ));
    }

    let data = resp.data.ok_or_else(|| CloudScraperError::EmptyResult {
        query: query.to_string(),
    })?;

    let first = data
        .result
        .first()
        .ok_or_else(|| CloudScraperError::EmptyResult {
            query: query.to_string(),
        })?;

    first
        .value
        .1
        .parse::<f64>()
        .map_err(|e| CloudScraperError::JsonParse(format!("value parse: {e}")))
}

/// Minimal percent-encoding for `PromQL` query parameters.
/// Encodes characters that are not URL-safe in a query string.
fn percent_encode_query(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for b in input.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'*'
            | b':'
            | b','
            | b'/'
            | b'@' => out.push(b as char),
            _ => {
                out.push('%');
                out.push(HEX_UPPER[(b >> 4) as usize] as char);
                out.push(HEX_UPPER[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

// ---------------------------------------------------------------
// Scraper loop
// ---------------------------------------------------------------

/// Spawn the periodic cloud energy scraper task.
///
/// Returns a `JoinHandle` that the daemon captures and aborts on
/// Ctrl-C shutdown.
#[must_use]
pub fn spawn_cloud_scraper(
    cfg: CloudEnergyConfig,
    state: Arc<CloudEnergyState>,
    metrics: Arc<MetricsState>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_cloud_scraper_loop(cfg, state, metrics).await;
    })
}

#[allow(clippy::too_many_lines)] // Scraper orchestration: URI parse, auth parse, ticker, JoinSet fanout. Splitting would fragment the lifecycle without clarity gain.
async fn run_cloud_scraper_loop(
    cfg: CloudEnergyConfig,
    state: Arc<CloudEnergyState>,
    metrics: Arc<MetricsState>,
) {
    use std::str::FromStr;

    let uri = match hyper::Uri::from_str(&cfg.prometheus_endpoint) {
        Ok(u) => u,
        Err(e) => {
            let err = CloudScraperError::InvalidUri {
                endpoint: cfg.prometheus_endpoint.clone(),
                source: e,
            };
            tracing::error!(error = %err, "Cloud energy scraper aborting on invalid endpoint");
            return;
        }
    };
    let redacted = http_client::redact_endpoint(&uri);

    // Parse the optional auth header once at startup. A parse failure
    // logs and aborts the task, silent retries would just spam warn logs.
    // Option<Arc<_>> so the no-auth path pays zero refcount cost in the
    // per-tick JoinSet fanout below.
    let Ok(raw_auth) = parse_scraper_auth_header(
        cfg.auth_header.as_deref(),
        &cfg.prometheus_endpoint,
        &redacted,
        "cloud_energy",
    ) else {
        return;
    };
    let parsed_auth: Option<Arc<AuthHeader>> = raw_auth.map(Arc::new);

    let client = http_client::build_client();

    let mut ticker = tokio::time::interval(cfg.scrape_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // skip first immediate tick

    let mut snapshot_diff = OpsSnapshotDiff::default();
    let mut first_failure_warned = false;
    let mut consecutive_failures: u32 = 0;
    let mut unreachable_warned = false;

    tracing::info!(
        endpoint = %redacted,
        scrape_interval_secs = cfg.scrape_interval.as_secs(),
        services = cfg.services.len(),
        "Cloud energy scraper started"
    );

    loop {
        ticker.tick().await;

        let current_ops = metrics.snapshot_service_io_ops();
        let deltas = snapshot_diff.delta_and_advance(current_ops);

        let tick_start = std::time::Instant::now();
        let mut any_success = false;
        let mut cpu_readings: HashMap<String, f64> = HashMap::with_capacity(cfg.services.len());

        // Query CPU% for all configured services in parallel via a
        // `JoinSet`. The previous sequential loop was N × request_time,
        // which at 1024 services × 20ms easily exceeded the 15s scrape
        // interval and caused ticker backlog. Parallelism lets us fan
        // out the queries and collect them as they return. The hyper
        // client holds an Arc internally so `.clone()` is cheap.
        let mut set: tokio::task::JoinSet<(String, Result<f64, CloudScraperError>)> =
            tokio::task::JoinSet::new();
        for (service, svc_cfg) in &cfg.services {
            let query = resolve_cpu_query(svc_cfg, &cfg).into_owned();
            let client_clone = client.clone();
            let uri_clone = uri.clone();
            let service_clone = service.clone();
            // Option::clone is a no-op on None and one Arc bump on Some.
            let auth_clone = parsed_auth.clone();
            set.spawn(async move {
                let result =
                    fetch_cpu_percent(&client_clone, &uri_clone, &query, auth_clone.as_deref())
                        .await;
                (service_clone, result)
            });
        }

        while let Some(join_result) = set.join_next().await {
            match join_result {
                Ok((service, Ok(cpu_pct))) if cpu_pct.is_finite() => {
                    cpu_readings.insert(service, cpu_pct);
                    any_success = true;
                }
                Ok((service, Ok(cpu_pct))) => {
                    tracing::debug!(
                        service = %service,
                        value = %cpu_pct,
                        "Cloud energy: Prometheus returned non-finite CPU%, skipping"
                    );
                }
                Ok((service, Err(e))) => {
                    tracing::debug!(
                        service = %service,
                        error = %e,
                        "Cloud energy: failed to fetch CPU% for service"
                    );
                }
                Err(e) => {
                    // Task panicked or was cancelled. Neither should
                    // happen in normal operation but don't bring the
                    // scraper loop down.
                    tracing::debug!(
                        error = %e,
                        "Cloud energy: CPU% fetch task panicked or was cancelled"
                    );
                }
            }
        }

        if any_success {
            first_failure_warned = false;
            consecutive_failures = 0;
            apply_cloud_scrape(&state, &cpu_readings, &deltas, &cfg, monotonic_ms());
            metrics.cloud_energy_last_scrape_age_seconds.set(0.0);
            tracing::debug!(
                readings = cpu_readings.len(),
                elapsed_ms = tick_start.elapsed().as_millis() as u64,
                "Cloud energy scrape succeeded"
            );
        } else if !cfg.services.is_empty() {
            consecutive_failures = consecutive_failures.saturating_add(1);
            log_scrape_failure(
                &redacted,
                &mut first_failure_warned,
                &mut unreachable_warned,
                consecutive_failures,
            );
        }
        warn_if_slow(&cfg, tick_start.elapsed());
    }
}

fn log_scrape_failure(
    redacted: &str,
    first_warned: &mut bool,
    unreachable_warned: &mut bool,
    consecutive: u32,
) {
    if *first_warned {
        tracing::debug!("Cloud energy scrape failed for all services");
    } else {
        tracing::warn!(
            endpoint = %redacted,
            "Cloud energy scrape failed for all services; subsequent failures at debug level"
        );
        *first_warned = true;
    }
    if !*unreachable_warned && consecutive >= FAILURE_THRESHOLD {
        tracing::warn!(
            endpoint = %redacted,
            consecutive,
            "Prometheus endpoint unreachable for {FAILURE_THRESHOLD} consecutive scrapes. \
             The daemon is falling back to the proxy model for all cloud-configured services."
        );
        *unreachable_warned = true;
    }
}

fn warn_if_slow(cfg: &CloudEnergyConfig, elapsed: Duration) {
    let warn_threshold = cfg.scrape_interval.mul_f64(0.8);
    if elapsed > warn_threshold {
        tracing::warn!(
            elapsed_ms = elapsed.as_millis() as u64,
            interval_ms = cfg.scrape_interval.as_millis() as u64,
            services = cfg.services.len(),
            "Cloud energy scrape took > 80% of the scrape interval"
        );
    }
}

/// Resolve the `PromQL` query for a service.
///
/// Returns a `Cow` to avoid allocating when the query is a `&'static str`
/// default or a borrowed reference from config.
fn resolve_cpu_query<'a>(
    svc_cfg: &'a ServiceCloudConfig,
    cfg: &'a CloudEnergyConfig,
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    // 1. Per-service custom query
    if let Some(q) = svc_cfg.cpu_query() {
        return Cow::Borrowed(q);
    }
    // 2. Top-level cpu_metric override
    if let Some(ref m) = cfg.cpu_metric {
        return Cow::Borrowed(m.as_str());
    }
    // 3. Default per provider
    let provider = match svc_cfg {
        ServiceCloudConfig::InstanceType { provider, .. } => provider.as_deref(),
        ServiceCloudConfig::ManualWatts { .. } => None,
    };
    let provider = provider
        .or(cfg.default_provider.as_deref())
        .unwrap_or("generic");
    match provider {
        "aws" => Cow::Borrowed(DEFAULT_CPU_QUERY_AWS),
        "gcp" => Cow::Borrowed(DEFAULT_CPU_QUERY_GCP),
        "azure" => Cow::Borrowed(DEFAULT_CPU_QUERY_AZURE),
        _ => Cow::Borrowed(DEFAULT_CPU_QUERY_GENERIC),
    }
}

/// Apply CPU% readings to the cloud energy state.
///
/// For each configured service, looks up `(idle_watts, max_watts)`
/// from the `SPECpower` table (or manual config), interpolates watts
/// from the CPU% reading, and computes `energy_per_op_kwh`. Services
/// with zero ops in the current window keep their previous entry.
fn apply_cloud_scrape(
    state: &CloudEnergyState,
    cpu_readings: &HashMap<String, f64>,
    op_deltas: &HashMap<String, u64>,
    cfg: &CloudEnergyConfig,
    now_ms: u64,
) {
    let interval_secs = cfg.scrape_interval.as_secs_f64();
    let mut next = state.current_owned();
    let mut any_change = false;

    for (service, svc_cfg) in &cfg.services {
        let Some(&cpu_pct) = cpu_readings.get(service) else {
            continue; // no reading for this service this tick
        };
        let Some(&ops) = op_deltas.get(service) else {
            continue; // service had no ops this window
        };

        let (idle_watts, max_watts) = resolve_power(svc_cfg, cfg);
        let watts = table::interpolate_watts(idle_watts, max_watts, cpu_pct);

        let Some(energy_per_op) = table::compute_cloud_energy_per_op_kwh(watts, interval_secs, ops)
        else {
            continue;
        };

        next.insert(
            service.clone(),
            ServiceEnergy {
                energy_per_op_kwh: energy_per_op,
                last_update_ms: now_ms,
            },
        );
        any_change = true;
    }

    if any_change {
        state.publish(next);
    }
}

/// Resolve `(idle_watts, max_watts)` for a service config entry.
///
/// For `InstanceType` entries, falls back to `cfg.default_instance_type`
/// when the per-service instance type is empty, then to the provider
/// default.
fn resolve_power(svc_cfg: &ServiceCloudConfig, cfg: &CloudEnergyConfig) -> (f64, f64) {
    match svc_cfg {
        ServiceCloudConfig::ManualWatts {
            idle_watts,
            max_watts,
            ..
        } => (*idle_watts, *max_watts),
        ServiceCloudConfig::InstanceType {
            provider,
            instance_type,
            ..
        } => {
            let provider_key = provider
                .as_deref()
                .or(cfg.default_provider.as_deref())
                .unwrap_or("generic");
            let effective_type = if instance_type.is_empty() {
                cfg.default_instance_type.as_deref().unwrap_or("")
            } else {
                instance_type.as_str()
            };
            table::lookup_instance_power(effective_type, provider_key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Prometheus JSON parsing
    // ------------------------------------------------------------------

    #[test]
    fn parse_valid_prom_response() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1234567890.123,"42.5"]}]}}"#;
        let val = parse_prom_scalar(body, "test_query").unwrap();
        assert!((val - 42.5).abs() < 1e-10);
    }

    #[test]
    fn parse_empty_result() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let err = parse_prom_scalar(body, "test_query").unwrap_err();
        assert!(
            matches!(err, CloudScraperError::EmptyResult { .. }),
            "expected EmptyResult, got {err:?}"
        );
    }

    #[test]
    fn parse_error_status() {
        let body = r#"{"status":"error","errorType":"bad_data","error":"invalid query"}"#;
        let err = parse_prom_scalar(body, "test_query").unwrap_err();
        assert!(matches!(err, CloudScraperError::QueryError(_)));
    }

    #[test]
    fn parse_malformed_json() {
        let err = parse_prom_scalar("not json", "test_query").unwrap_err();
        assert!(matches!(err, CloudScraperError::JsonParse(_)));
    }

    #[test]
    fn parse_nan_value_returns_nan() {
        // Prometheus returns "NaN" for absent series. parse_prom_scalar
        // returns Ok(NaN), but the scraper loop filters non-finite
        // values before inserting into cpu_readings.
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1234567890.123,"NaN"]}]}}"#;
        let val = parse_prom_scalar(body, "test_query").unwrap();
        assert!(val.is_nan());
    }

    // ------------------------------------------------------------------
    // percent_encode_query
    // ------------------------------------------------------------------

    #[test]
    fn encode_simple_metric() {
        assert_eq!(
            percent_encode_query("aws_ec2_cpuutilization_average"),
            "aws_ec2_cpuutilization_average"
        );
    }

    #[test]
    fn encode_promql_with_braces_and_quotes() {
        let input = r#"rate(node_cpu{mode="idle"}[5m])"#;
        let encoded = percent_encode_query(input);
        assert!(encoded.contains("%7B")); // {
        assert!(encoded.contains("%7D")); // }
        assert!(encoded.contains("%22")); // "
        assert!(encoded.contains("%5B")); // [
        assert!(encoded.contains("%5D")); // ]
    }

    // ------------------------------------------------------------------
    // resolve_cpu_query
    // ------------------------------------------------------------------

    #[test]
    fn custom_cpu_query_takes_precedence() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("aws".into()),
            instance_type: "m5.large".into(),
            cpu_query: Some("custom_metric".into()),
        };
        let cfg = make_test_config();
        assert_eq!(resolve_cpu_query(&svc, &cfg), "custom_metric");
    }

    #[test]
    fn top_level_cpu_metric_used_when_no_per_service_query() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("aws".into()),
            instance_type: "m5.large".into(),
            cpu_query: None,
        };
        let mut cfg = make_test_config();
        cfg.cpu_metric = Some("global_cpu_metric".into());
        assert_eq!(resolve_cpu_query(&svc, &cfg), "global_cpu_metric");
    }

    #[test]
    fn provider_default_used_as_fallback() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("gcp".into()),
            instance_type: "n2-standard-2".into(),
            cpu_query: None,
        };
        let cfg = make_test_config();
        assert_eq!(resolve_cpu_query(&svc, &cfg), DEFAULT_CPU_QUERY_GCP);
    }

    // ------------------------------------------------------------------
    // resolve_power
    // ------------------------------------------------------------------

    #[test]
    fn manual_watts_used_directly() {
        let svc = ServiceCloudConfig::ManualWatts {
            idle_watts: 45.0,
            max_watts: 120.0,
            cpu_query: None,
        };
        let cfg = make_test_config();
        let (idle, max) = resolve_power(&svc, &cfg);
        assert!((idle - 45.0).abs() < 1e-10);
        assert!((max - 120.0).abs() < 1e-10);
    }

    #[test]
    fn instance_type_looked_up_in_table() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("aws".into()),
            instance_type: "c5.4xlarge".into(),
            cpu_query: None,
        };
        let cfg = make_test_config();
        let (idle, max) = resolve_power(&svc, &cfg);
        assert!((idle - 21.3).abs() < 0.01);
        assert!((max - 143.7).abs() < 0.1);
    }

    // ------------------------------------------------------------------
    // apply_cloud_scrape
    // ------------------------------------------------------------------

    #[test]
    fn apply_scrape_computes_energy() {
        let state = CloudEnergyState::new();
        let mut cpu = HashMap::new();
        cpu.insert("svc-a".to_string(), 50.0); // 50% CPU
        let mut ops = HashMap::new();
        ops.insert("svc-a".to_string(), 1000_u64);

        let mut services = HashMap::new();
        services.insert(
            "svc-a".to_string(),
            ServiceCloudConfig::ManualWatts {
                idle_watts: 2.0,
                max_watts: 20.0,
                cpu_query: None,
            },
        );
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services,
            auth_header: None,
        };

        apply_cloud_scrape(&state, &cpu, &ops, &cfg, 100);
        let snap = state.snapshot(200, 500);
        assert_eq!(snap.len(), 1);
        // 50% CPU on 2/20W = 11W. energy = (11/1000) * (15/3600) / 1000
        let expected_watts = 11.0;
        let expected = (expected_watts / 1000.0) * (15.0 / 3600.0) / 1000.0;
        assert!(
            (snap["svc-a"] - expected).abs() < 1e-15,
            "expected {expected}, got {}",
            snap["svc-a"]
        );
    }

    #[test]
    fn apply_scrape_zero_ops_keeps_previous() {
        let state = CloudEnergyState::new();
        state.insert_for_test("svc-a".into(), 1e-7, 50);

        let cpu = HashMap::new(); // no readings
        let ops = HashMap::new(); // no ops

        let cfg = make_test_config();
        apply_cloud_scrape(&state, &cpu, &ops, &cfg, 100);

        // Previous entry should still be there.
        let snap = state.snapshot(100, 500);
        assert_eq!(snap.len(), 1);
        assert!((snap["svc-a"] - 1e-7).abs() < 1e-15);
    }

    fn make_test_config() -> CloudEnergyConfig {
        CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: Some("aws".into()),
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: None,
        }
    }

    // ------------------------------------------------------------------
    // resolve_cpu_query: exhaustive provider-branch coverage
    // ------------------------------------------------------------------

    #[test]
    fn azure_provider_uses_azure_default_query() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("azure".into()),
            instance_type: "Standard_D4s_v3".into(),
            cpu_query: None,
        };
        let cfg = make_test_config();
        assert_eq!(resolve_cpu_query(&svc, &cfg), DEFAULT_CPU_QUERY_AZURE);
    }

    #[test]
    fn aws_provider_uses_aws_default_query() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("aws".into()),
            instance_type: "m5.large".into(),
            cpu_query: None,
        };
        let cfg = make_test_config();
        assert_eq!(resolve_cpu_query(&svc, &cfg), DEFAULT_CPU_QUERY_AWS);
    }

    #[test]
    fn unknown_provider_falls_back_to_generic_query() {
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("hetzner".into()),
            instance_type: "cx21".into(),
            cpu_query: None,
        };
        let cfg = make_test_config();
        assert_eq!(resolve_cpu_query(&svc, &cfg), DEFAULT_CPU_QUERY_GENERIC);
    }

    #[test]
    fn manual_watts_variant_uses_default_provider_for_query() {
        // ManualWatts has no provider field; resolve_cpu_query must
        // fall back to cfg.default_provider, exercising the second
        // match arm in resolve_cpu_query.
        let svc = ServiceCloudConfig::ManualWatts {
            idle_watts: 10.0,
            max_watts: 100.0,
            cpu_query: None,
        };
        let mut cfg = make_test_config();
        cfg.default_provider = Some("azure".into());
        assert_eq!(resolve_cpu_query(&svc, &cfg), DEFAULT_CPU_QUERY_AZURE);
    }

    #[test]
    fn manual_watts_variant_no_default_provider_uses_generic() {
        let svc = ServiceCloudConfig::ManualWatts {
            idle_watts: 10.0,
            max_watts: 100.0,
            cpu_query: None,
        };
        let mut cfg = make_test_config();
        cfg.default_provider = None;
        assert_eq!(resolve_cpu_query(&svc, &cfg), DEFAULT_CPU_QUERY_GENERIC);
    }

    // ------------------------------------------------------------------
    // apply_cloud_scrape: missing-readings and non-finite branches
    // ------------------------------------------------------------------

    #[test]
    fn apply_scrape_skips_service_without_cpu_reading() {
        let state = CloudEnergyState::new();
        let cpu = HashMap::new(); // no CPU reading for svc-a
        let mut ops = HashMap::new();
        ops.insert("svc-a".to_string(), 1000_u64);

        let mut services = HashMap::new();
        services.insert(
            "svc-a".to_string(),
            ServiceCloudConfig::ManualWatts {
                idle_watts: 2.0,
                max_watts: 20.0,
                cpu_query: None,
            },
        );
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services,
            auth_header: None,
        };

        apply_cloud_scrape(&state, &cpu, &ops, &cfg, 100);
        // No CPU reading → continue → no insertion → empty snapshot.
        let snap = state.snapshot(200, 500);
        assert!(snap.is_empty());
    }

    #[test]
    fn apply_scrape_skips_service_without_ops_delta() {
        let state = CloudEnergyState::new();
        let mut cpu = HashMap::new();
        cpu.insert("svc-a".to_string(), 50.0);
        let ops = HashMap::new(); // no ops delta for svc-a

        let mut services = HashMap::new();
        services.insert(
            "svc-a".to_string(),
            ServiceCloudConfig::ManualWatts {
                idle_watts: 2.0,
                max_watts: 20.0,
                cpu_query: None,
            },
        );
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services,
            auth_header: None,
        };

        apply_cloud_scrape(&state, &cpu, &ops, &cfg, 100);
        // No ops delta → continue → no insertion.
        let snap = state.snapshot(200, 500);
        assert!(snap.is_empty());
    }

    #[test]
    fn apply_scrape_zero_ops_non_finite_energy_skipped() {
        // ops=0 makes `compute_cloud_energy_per_op_kwh` return None,
        // which hits the `continue` on line 421. The service should
        // NOT be inserted into state.
        let state = CloudEnergyState::new();
        let mut cpu = HashMap::new();
        cpu.insert("svc-a".to_string(), 50.0);
        let mut ops = HashMap::new();
        ops.insert("svc-a".to_string(), 0_u64); // zero ops → None

        let mut services = HashMap::new();
        services.insert(
            "svc-a".to_string(),
            ServiceCloudConfig::ManualWatts {
                idle_watts: 2.0,
                max_watts: 20.0,
                cpu_query: None,
            },
        );
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services,
            auth_header: None,
        };

        apply_cloud_scrape(&state, &cpu, &ops, &cfg, 100);
        let snap = state.snapshot(200, 500);
        assert!(snap.is_empty(), "zero-ops service must not be inserted");
    }

    #[test]
    fn resolve_power_falls_back_to_default_instance_type() {
        // svc_cfg.instance_type is empty → uses cfg.default_instance_type.
        let svc = ServiceCloudConfig::InstanceType {
            provider: Some("aws".into()),
            instance_type: String::new(), // empty → fallback path
            cpu_query: None,
        };
        let mut cfg = make_test_config();
        cfg.default_instance_type = Some("c5.4xlarge".into());
        let (idle, max) = resolve_power(&svc, &cfg);
        assert!((idle - 21.3).abs() < 0.01);
        assert!((max - 143.7).abs() < 0.1);
    }

    // ------------------------------------------------------------------
    // warn_if_slow
    // ------------------------------------------------------------------

    #[test]
    fn warn_if_slow_does_not_panic_when_elapsed_under_threshold() {
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: None,
        };
        warn_if_slow(&cfg, Duration::from_millis(100));
    }

    #[test]
    fn warn_if_slow_does_not_panic_when_elapsed_exceeds_threshold() {
        // 13s > 15s * 0.8 = 12s → emits a tracing::warn but must not panic.
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://localhost:9090".into(),
            scrape_interval: Duration::from_secs(15),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: None,
        };
        warn_if_slow(&cfg, Duration::from_secs(13));
    }

    // ------------------------------------------------------------------
    // log_scrape_failure
    // ------------------------------------------------------------------

    #[test]
    fn log_scrape_failure_first_invocation_flips_first_warned() {
        let mut first_warned = false;
        let mut unreachable_warned = false;
        log_scrape_failure("http://fake", &mut first_warned, &mut unreachable_warned, 1);
        assert!(first_warned, "first invocation must flip first_warned");
        assert!(!unreachable_warned, "unreachable threshold not reached yet");
    }

    #[test]
    fn log_scrape_failure_threshold_sets_unreachable() {
        let mut first_warned = true;
        let mut unreachable_warned = false;
        log_scrape_failure(
            "http://fake",
            &mut first_warned,
            &mut unreachable_warned,
            FAILURE_THRESHOLD,
        );
        assert!(
            unreachable_warned,
            "threshold reached must flip unreachable_warned"
        );
    }

    #[test]
    fn log_scrape_failure_is_idempotent_after_unreachable_flagged() {
        let mut first_warned = true;
        let mut unreachable_warned = true;
        log_scrape_failure(
            "http://fake",
            &mut first_warned,
            &mut unreachable_warned,
            10,
        );
        // Both flags stay true, no panic.
        assert!(first_warned && unreachable_warned);
    }

    // ------------------------------------------------------------------
    // Integration tests with a mock HTTP server on an ephemeral port
    // ------------------------------------------------------------------
    //
    // Shared helpers from `crate::test_helpers`, same pattern used by
    // scaphandre, electricity_maps, and tempo tests.

    use crate::test_helpers::{http_200_text, http_status, spawn_one_shot_server};

    /// Wrap the shared `http_200_text` with the JSON content type.
    fn http_200_json(body: &str) -> Vec<u8> {
        http_200_text("application/json", body)
    }

    #[tokio::test]
    async fn fetch_cpu_percent_happy_path() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1234567890.0,"42.5"]}]}}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200_json(body)).await;

        let client = http_client::build_client();
        let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
        let val = fetch_cpu_percent(&client, &uri, "test_query", None)
            .await
            .expect("valid response should parse");
        assert!((val - 42.5).abs() < 1e-10);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_cpu_percent_non_200_surfaces_http_status() {
        let (endpoint, server) =
            spawn_one_shot_server(http_status(503, "Service Unavailable")).await;

        let client = http_client::build_client();
        let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
        let err = fetch_cpu_percent(&client, &uri, "q", None)
            .await
            .expect_err("503 must error");
        match err {
            CloudScraperError::Fetch(http_client::FetchError::HttpStatus(503)) => {}
            other => panic!("expected Fetch(HttpStatus(503)), got {other:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_cpu_percent_malformed_json_surfaces_json_parse() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json("not json")).await;

        let client = http_client::build_client();
        let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
        let err = fetch_cpu_percent(&client, &uri, "q", None)
            .await
            .expect_err("malformed JSON must error");
        assert!(matches!(err, CloudScraperError::JsonParse(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_cpu_percent_empty_result_surfaces_empty_result_error() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200_json(body)).await;

        let client = http_client::build_client();
        let uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
        let err = fetch_cpu_percent(&client, &uri, "query-alpha", None)
            .await
            .expect_err("empty result must error");
        match err {
            CloudScraperError::EmptyResult { query } => {
                assert_eq!(query, "query-alpha");
            }
            other => panic!("expected EmptyResult, got {other:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn spawn_cloud_scraper_invalid_endpoint_exits_cleanly() {
        // `from_str` on the invalid URI returns Err, which makes
        // run_cloud_scraper_loop emit an error and return immediately.
        // The spawned task should complete without panic.
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "not a uri :: bad".into(),
            scrape_interval: Duration::from_millis(50),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: None,
        };
        let state = CloudEnergyState::new();
        let metrics = Arc::new(MetricsState::new());
        let handle = spawn_cloud_scraper(cfg, state, metrics);
        // The task returns almost immediately on the InvalidUri branch.
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("invalid URI task must exit within 2s")
            .expect("task must not panic");
    }

    #[tokio::test]
    async fn spawn_cloud_scraper_empty_services_never_fails() {
        // With no services configured, each tick runs to completion
        // without querying anything. `any_success == false` but the
        // `!cfg.services.is_empty()` guard skips `log_scrape_failure`.
        // Verify the task stays alive and we can abort cleanly.
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://127.0.0.1:1".into(), // unreachable
            scrape_interval: Duration::from_millis(50),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services: HashMap::new(),
            auth_header: None,
        };
        let state = CloudEnergyState::new();
        let metrics = Arc::new(MetricsState::new());
        let handle = spawn_cloud_scraper(cfg, state, metrics);
        // Let a couple of ticks fire.
        tokio::time::sleep(Duration::from_millis(180)).await;
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn spawn_cloud_scraper_unreachable_endpoint_keeps_running() {
        // Configured services but the endpoint refuses connections.
        // The scraper must stay alive, go through log_scrape_failure,
        // eventually bump consecutive_failures past the threshold.
        let mut services = HashMap::new();
        services.insert(
            "svc-a".to_string(),
            ServiceCloudConfig::ManualWatts {
                idle_watts: 2.0,
                max_watts: 20.0,
                cpu_query: None,
            },
        );
        let cfg = CloudEnergyConfig {
            prometheus_endpoint: "http://127.0.0.1:1".into(),
            scrape_interval: Duration::from_millis(30),
            default_provider: None,
            default_instance_type: None,
            cpu_metric: None,
            services,
            auth_header: None,
        };
        let state = CloudEnergyState::new();
        let metrics = Arc::new(MetricsState::new());
        let handle = spawn_cloud_scraper(cfg, state.clone(), metrics);
        // Enough ticks to step past the FAILURE_THRESHOLD.
        tokio::time::sleep(Duration::from_millis(220)).await;
        handle.abort();
        let _ = handle.await;

        // State stays empty since nothing ever succeeded.
        assert!(state.snapshot(monotonic_ms(), 5_000).is_empty());
    }

    #[test]
    fn cloud_scraper_error_display_messages_are_informative() {
        use crate::http_client::FetchError;
        let e1 = CloudScraperError::Fetch(FetchError::BodyRead("oops".to_string()));
        let e2 = CloudScraperError::Fetch(FetchError::HttpStatus(503));
        let e3 = CloudScraperError::Fetch(FetchError::Timeout);
        let e4 = CloudScraperError::JsonParse("bad".to_string());
        let e5 = CloudScraperError::EmptyResult {
            query: "q".to_string(),
        };
        let e6 = CloudScraperError::QueryError("err".to_string());
        assert!(format!("{e1}").contains("fetch failed"));
        assert!(format!("{e2}").contains("fetch failed"));
        assert!(format!("{e3}").contains("fetch failed"));
        assert!(format!("{e4}").contains("parse"));
        assert!(format!("{e5}").contains("no result"));
        assert!(format!("{e6}").contains("error status"));
    }

    /// End-to-end check that a configured `auth_header` lands on the
    /// Prometheus request wire when `fetch_cpu_percent` is invoked.
    /// Mirrors the shape of `search_sends_auth_header_on_wire` in
    /// `jaeger_query.rs`.
    #[tokio::test]
    async fn cloud_scraper_sends_auth_header_on_wire() {
        use crate::ingest::auth_header::AuthHeader;

        let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1234567890.0,"42.5"]}]}}"#;
        let response = http_200_json(body);
        let (endpoint, mut rx, server) = crate::test_helpers::spawn_capture_server(response).await;

        let client = http_client::build_client();
        let base_uri = <hyper::Uri as std::str::FromStr>::from_str(&endpoint).unwrap();
        let auth = AuthHeader::parse("Authorization: Bearer topsecret").expect("valid");
        let val = fetch_cpu_percent(&client, &base_uri, "up", Some(&auth))
            .await
            .expect("fetch_cpu_percent must succeed");
        assert!((val - 42.5).abs() < 1e-10);

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).expect("utf8");
        assert!(
            text.contains("authorization: Bearer topsecret")
                || text.contains("Authorization: Bearer topsecret"),
            "auth header missing from request, got:\n{text}"
        );
        assert!(
            text.contains("/api/v1/query?query=up"),
            "PromQL query path missing, got:\n{text}"
        );
        server.await.expect("server join");
    }
}
