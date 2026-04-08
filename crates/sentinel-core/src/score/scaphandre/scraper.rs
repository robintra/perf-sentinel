//! Scaphandre scraper task, HTTP client, and error types.
//!
//! This module owns the runtime machinery: the hyper-util legacy
//! client, the periodic scrape loop, the typed errors with preserved
//! source chains, and the credential-redaction helper used by every
//! scraper log line.
//!
//! The public entry point is [`spawn_scraper`]; everything else is
//! either `pub(super)` (shared with `mod.rs` test re-exports) or
//! private to this file.

use std::sync::Arc;
use std::time::Duration;

use crate::report::metrics::MetricsState;

use super::config::ScaphandreConfig;
use super::ops::{OpsSnapshotDiff, apply_scrape};
use super::parser::parse_scaphandre_metrics;
use super::state::{ScaphandreState, monotonic_ms};

/// Hyper-util legacy client used by the Scaphandre scraper. Built once
/// per scraper task in [`run_scraper_loop`] and reused across every
/// scrape so the underlying connection pool stays warm.
pub(super) type ScraperClient = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    http_body_util::Empty<bytes::Bytes>,
>;

/// Maximum response body size accepted from the Scaphandre endpoint.
///
/// 8 MiB is generous: real Scaphandre exports are typically <1 MiB
/// even on hosts with thousands of processes. The cap exists to
/// prevent a misbehaving (or attacker-redirected) endpoint from
/// exhausting the daemon's RAM by streaming a multi-GB response —
/// the 3 s fetch timeout only bounds latency, not bandwidth.
pub(super) const MAX_SCRAPE_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Number of consecutive scrape failures before [`run_scraper_loop`]
/// emits the one-shot "likely unsupported platform" warning. 3 is
/// enough to rule out transient network blips on a working host
/// without delaying the diagnostic too long on a misconfigured host.
const UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD: u32 = 3;

/// Build a fresh hyper-util client for the scraper. Called once per
/// scraper task at startup; the client is then reused for every fetch.
pub(super) fn build_scraper_client() -> ScraperClient {
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;
    Client::builder(TokioExecutor::new()).build_http()
}

/// Scrape the Scaphandre endpoint once via the hyper-util client.
/// Returns the response body as a `String` on success.
///
/// A 3-second hard timeout guards against hung endpoints. Non-2xx
/// responses are treated as errors. The body is wrapped in
/// [`http_body_util::Limited`] with [`MAX_SCRAPE_BODY_BYTES`] so a
/// runaway endpoint cannot OOM the daemon. The function takes the
/// pre-parsed `Uri` and the long-lived [`ScraperClient`] by
/// reference so connection pooling actually kicks in.
pub(super) async fn fetch_metrics_once(
    client: &ScraperClient,
    uri: &hyper::Uri,
) -> Result<String, ScraperError> {
    use http_body_util::{BodyExt, Empty, Limited};

    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(uri.clone())
        .header(
            hyper::header::USER_AGENT,
            "perf-sentinel/scaphandre-scraper",
        )
        .body(Empty::<bytes::Bytes>::new())
        .map_err(ScraperError::RequestBuild)?;

    let response = tokio::time::timeout(Duration::from_secs(3), client.request(req))
        .await
        .map_err(|_| ScraperError::Timeout)?
        .map_err(ScraperError::Transport)?;

    if !response.status().is_success() {
        return Err(ScraperError::HttpStatus(response.status().as_u16()));
    }

    // Cap the body at MAX_SCRAPE_BODY_BYTES. `Limited` produces a
    // `LengthLimitError` when the cap is exceeded; we map it to the
    // dedicated `BodyRead` variant so operators can distinguish it
    // from a transport error.
    let limited_body = Limited::new(response.into_body(), MAX_SCRAPE_BODY_BYTES);
    let collected = limited_body
        .collect()
        .await
        .map_err(|e| ScraperError::BodyRead(format!("{e}")))?;
    let bytes = collected.to_bytes();
    String::from_utf8(bytes.to_vec()).map_err(ScraperError::Utf8)
}

/// Errors the scraper task might emit. They are never returned to the
/// caller — the scraper task logs them via `tracing` with the
/// warn-once pattern and continues running.
///
/// `#[from]` and `#[source]` attributes preserve the underlying error
/// chain so `tracing::warn!(error = %e)` walks down to the root cause
/// (hyper / http / `FromUtf8` / etc.) instead of throwing it away in a
/// `format!`.
#[derive(Debug, thiserror::Error)]
pub(super) enum ScraperError {
    /// Endpoint URI failed to parse at config-load or scraper-startup time.
    /// The `endpoint` field is the redacted URL (no userinfo) — see
    /// [`redact_endpoint`].
    #[error("invalid Scaphandre endpoint URI '{endpoint}'")]
    InvalidUri {
        endpoint: String,
        #[source]
        source: hyper::http::uri::InvalidUri,
    },
    #[error("failed to build HTTP request")]
    RequestBuild(#[source] hyper::http::Error),
    #[error("HTTP transport error")]
    Transport(#[source] hyper_util::client::legacy::Error),
    /// Body read failed or exceeded `MAX_SCRAPE_BODY_BYTES`.
    /// The inner string is the upstream error message — typically a
    /// `LengthLimitError` from `http_body_util::Limited` when the cap
    /// trips, or a connection-level read failure otherwise.
    #[error("Scaphandre body read failed: {0}")]
    BodyRead(String),
    #[error("Scaphandre endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("Scaphandre endpoint timed out (3s)")]
    Timeout,
    #[error("Scaphandre response was not valid UTF-8")]
    Utf8(#[source] std::string::FromUtf8Error),
}

/// Strip userinfo (`http://user:pass@host/`) from a `Uri` before
/// logging. perf-sentinel does not support authenticated Scaphandre
/// endpoints, but operators may put credentials in the URL out of
/// habit when fronting Scaphandre with a reverse proxy. Logging the
/// raw URI would leak those credentials to journald / log
/// aggregators. This helper rebuilds the URL with only the safe parts
/// (scheme, host, port, path).
fn redact_endpoint(uri: &hyper::Uri) -> String {
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = uri.host().unwrap_or("?");
    let path_and_query = uri.path_and_query().map_or("/", |p| p.as_str());
    if let Some(port) = uri.port_u16() {
        format!("{scheme}://{host}:{port}{path_and_query}")
    } else {
        format!("{scheme}://{host}{path_and_query}")
    }
}

/// Spawn the periodic Scaphandre scraper task.
///
/// Returns a `JoinHandle` that the daemon captures and aborts on
/// Ctrl-C shutdown. The task runs until aborted or until the endpoint
/// produces an unrecoverable error (the current implementation keeps
/// running across failures — see the warn-once log pattern below).
///
/// The task reads the per-service op counter from the `Arc<MetricsState>`
/// (shared with the daemon's event intake path) at each tick, computes
/// a per-service op delta via `OpsSnapshotDiff`, scrapes the endpoint,
/// parses the response, and applies the new readings to the shared
/// `ScaphandreState`.
///
/// Updates the `scaphandre_last_scrape_age_seconds` gauge on each
/// successful scrape so Grafana dashboards can detect hung scrapers.
#[must_use]
pub fn spawn_scraper(
    cfg: ScaphandreConfig,
    state: Arc<ScaphandreState>,
    metrics: Arc<MetricsState>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_scraper_loop(cfg, state, metrics).await;
    })
}

/// Inner loop for [`spawn_scraper`]. Extracted so tests can drive a
/// single tick without the full spawn machinery.
///
/// Owns one [`ScraperClient`] for the lifetime of the scraper task so
/// hyper-util can pool the underlying TCP connection between scrapes.
/// Parses the endpoint URI once at startup; if the URI is malformed
/// the task logs and exits cleanly without retrying — a user-facing
/// config error should fail loud, not warn-spam every 5 s.
async fn run_scraper_loop(
    cfg: ScaphandreConfig,
    state: Arc<ScaphandreState>,
    metrics: Arc<MetricsState>,
) {
    use std::str::FromStr;

    // Parse the URI once. The config validator at config.rs::validate_green
    // already runs the same parse at config-load time, so this should never
    // fail in practice — but we keep the runtime guard so the scraper task
    // exits cleanly instead of panicking on a hot-reloaded bad URL.
    let uri = match hyper::Uri::from_str(&cfg.endpoint) {
        Ok(u) => u,
        Err(e) => {
            let err = ScraperError::InvalidUri {
                endpoint: cfg.endpoint.clone(),
                source: e,
            };
            tracing::error!(error = %err, "Scaphandre scraper aborting on invalid endpoint");
            return;
        }
    };
    let redacted = redact_endpoint(&uri);
    let client = build_scraper_client();

    let mut ticker = tokio::time::interval(cfg.scrape_interval);
    // Skip ticks instead of bursting if the scraper falls behind.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; absorb it before the loop so
    // the first real scrape happens after `scrape_interval`.
    ticker.tick().await;

    let mut snapshot_diff = OpsSnapshotDiff::default();
    let mut first_failure_warned = false;
    // Track consecutive scrape failures so we can emit a one-shot
    // "likely unsupported platform" warn after 3 failures in a row.
    // This is aimed at operators who configured `[green.scaphandre]`
    // on an ARM64 host or a cloud VM without RAPL passthrough and
    // would otherwise silently get the proxy model without knowing
    // why. Reset on the first success.
    let mut consecutive_failures: u32 = 0;
    let mut unsupported_platform_warned = false;

    tracing::info!(
        endpoint = %redacted,
        scrape_interval_secs = cfg.scrape_interval.as_secs(),
        process_count = cfg.process_map.len(),
        "Scaphandre scraper started"
    );

    loop {
        ticker.tick().await;

        // Snapshot current per-service counters and compute the delta.
        // `delta_and_advance` takes `current_ops` by value and stores
        // it internally as an Arc, avoiding a per-tick deep clone.
        let current_ops = metrics.snapshot_service_io_ops();
        let deltas = snapshot_diff.delta_and_advance(current_ops);

        match fetch_metrics_once(&client, &uri).await {
            Ok(body) => {
                first_failure_warned = false;
                consecutive_failures = 0;
                let readings = parse_scaphandre_metrics(&body);
                let now = monotonic_ms();
                apply_scrape(&state, &readings, &deltas, &cfg, now);
                // Update the "last successful scrape age" gauge to 0 —
                // Grafana rate() / alerting rules catch hung scrapers
                // by watching the gauge climb.
                metrics.scaphandre_last_scrape_age_seconds.set(0.0);
                tracing::debug!(
                    readings = readings.len(),
                    services_updated = deltas.len(),
                    "Scaphandre scrape succeeded"
                );
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                if first_failure_warned {
                    tracing::debug!(error = %e, "Scaphandre scrape failed again");
                } else {
                    tracing::warn!(
                        error = %e,
                        endpoint = %redacted,
                        "Scaphandre scrape failed; subsequent failures will log at debug level"
                    );
                    first_failure_warned = true;
                }
                // One-shot diagnostic after N consecutive failures: the
                // likely root cause is an unsupported host, not a
                // transient issue. Emit once, then stay silent.
                if !unsupported_platform_warned
                    && consecutive_failures >= UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD
                {
                    tracing::warn!(
                        endpoint = %redacted,
                        consecutive_failures = consecutive_failures,
                        "Scaphandre endpoint has been unreachable for {UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD} consecutive scrapes. \
                         Check that Scaphandre is installed and serving metrics at the configured endpoint. \
                         Scaphandre requires Linux with Intel/AMD RAPL support; ARM64 and most cloud VMs without RAPL \
                         passthrough are not supported. See docs/LIMITATIONS.md#scaphandre-precision-bounds. \
                         The daemon is falling back to the proxy model for all services."
                    );
                    unsupported_platform_warned = true;
                }
            }
        }
    }
}
