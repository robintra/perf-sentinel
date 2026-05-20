//! Kepler scraper task, HTTP client, and error types. Direct-scrape
//! mode only this round; Prometheus-mediated mode is deferred. See
//! `docs/design/05-GREENOPS-AND-CARBON.md` for the methodology and
//! `docs/LIMITATIONS.md` for the precision bounds.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::http_client::{self, FetchError, HttpClient};
use crate::ingest::auth_header::{AuthHeader, ScraperAuthOutcome, parse_scraper_auth_header};
use crate::report::metrics::{KeplerScrapeReason, MetricsState};
use crate::score::ops_snapshot_diff::OpsSnapshotDiff;

use super::apply::process_scrape;
use super::config::KeplerConfig;
use super::parser::parse_kepler_metrics;
use super::state::{KeplerState, monotonic_ms};

/// Number of consecutive scrape failures before [`run_scraper_loop`]
/// emits the one-shot "likely misconfigured endpoint" warning. Same
/// rationale as Scaphandre's threshold.
const UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD: u32 = 3;

/// Scrape the Kepler endpoint once via the hyper-util client.
pub(super) async fn fetch_metrics_once(
    client: &HttpClient,
    uri: &hyper::Uri,
    auth: Option<&AuthHeader>,
) -> Result<String, ScraperError> {
    let bytes = http_client::fetch_get(
        client,
        uri,
        "perf-sentinel/kepler-scraper",
        Duration::from_secs(3),
        auth,
    )
    .await
    .map_err(ScraperError::Fetch)?;
    String::from_utf8(bytes.to_vec()).map_err(ScraperError::Utf8)
}

/// Errors the scraper task might emit. Never returned to the caller,
/// logged via `tracing` with the warn-once pattern and the task keeps
/// running. URI parsing failures are handled separately at startup and
/// never reach this enum.
#[derive(Debug, thiserror::Error)]
pub(super) enum ScraperError {
    #[error("Kepler fetch failed")]
    Fetch(#[source] FetchError),
    #[error("Kepler response was not valid UTF-8")]
    Utf8(#[source] std::string::FromUtf8Error),
}

/// Map a [`ScraperError`] to the `reason` label used by
/// `perf_sentinel_kepler_scrape_failed_total`.
pub(super) fn scraper_error_reason(err: &ScraperError) -> KeplerScrapeReason {
    match err {
        ScraperError::Fetch(fe) => fetch_error_reason(fe),
        ScraperError::Utf8(_) => KeplerScrapeReason::InvalidUtf8,
    }
}

fn fetch_error_reason(err: &FetchError) -> KeplerScrapeReason {
    match err {
        FetchError::Transport(_) => KeplerScrapeReason::Unreachable,
        FetchError::Timeout => KeplerScrapeReason::Timeout,
        FetchError::HttpStatus(_) => KeplerScrapeReason::HttpError,
        FetchError::BodyRead(_) => KeplerScrapeReason::BodyReadError,
        FetchError::RequestBuild(_) => KeplerScrapeReason::RequestError,
    }
}

/// Spawn the periodic Kepler scraper task.
///
/// Returns a `JoinHandle` the daemon captures and aborts on Ctrl-C.
/// The task reads per-service op counts from `MetricsState`, scrapes
/// the Kepler endpoint, computes joule deltas vs the previous scrape
/// (held in an internal `last_raw_joules` table), and publishes
/// refreshed coefficients via [`KeplerState::publish`].
#[must_use]
pub fn spawn_scraper(
    cfg: KeplerConfig,
    state: Arc<KeplerState>,
    metrics: Arc<MetricsState>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_scraper_loop(cfg, state, metrics).await;
    })
}

async fn run_scraper_loop(cfg: KeplerConfig, state: Arc<KeplerState>, metrics: Arc<MetricsState>) {
    use std::str::FromStr;

    let uri = match hyper::Uri::from_str(&cfg.endpoint) {
        Ok(u) => u,
        Err(e) => {
            // Defense in depth: validate_kepler already rejects `@` in
            // the authority, but log the redacted string in case a
            // future caller skips validation.
            tracing::error!(
                endpoint = %http_client::redact_endpoint_str(&cfg.endpoint),
                error = %e,
                "Kepler scraper aborting on invalid endpoint URI"
            );
            return;
        }
    };
    let redacted = http_client::redact_endpoint(&uri);

    let parsed_auth: Option<AuthHeader> = match parse_scraper_auth_header(
        cfg.auth_header.as_deref(),
        &cfg.endpoint,
        &redacted,
        "kepler",
    ) {
        ScraperAuthOutcome::Invalid => return,
        ScraperAuthOutcome::None => None,
        ScraperAuthOutcome::Some(h) => Some(h),
    };

    let client = http_client::build_client();

    let mut ticker = tokio::time::interval(cfg.scrape_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    let mut snapshot_diff = OpsSnapshotDiff::default();
    // Size by the mapped-services count, not by samples returned: idle
    // services may not show up in a given scrape, but we still want to
    // remember their raw counter once they become active.
    let mut last_raw_joules: HashMap<String, f64> =
        HashMap::with_capacity(cfg.service_mappings.len());
    let mut failure_streak_warned = false;
    let mut consecutive_failures: u32 = 0;
    let mut unsupported_platform_warned = false;
    // Track the last successful scrape so the `last_scrape_age_seconds`
    // gauge advances on every failure tick. Seeded to scraper-start time
    // so a Kepler endpoint broken from boot still climbs the gauge
    // (otherwise the never-succeeded case would leave it frozen at 0).
    let mut last_success_ms: u64 = monotonic_ms();
    let metric_name = cfg.metric_kind.metric_name();
    let label_key = cfg.metric_kind.label_key();

    tracing::info!(
        endpoint = %redacted,
        scrape_interval_secs = cfg.scrape_interval.as_secs(),
        metric = metric_name,
        service_count = cfg.service_mappings.len(),
        "Kepler scraper started"
    );

    loop {
        ticker.tick().await;

        let current_ops = metrics.snapshot_service_io_ops();
        let deltas = snapshot_diff.delta_and_advance(current_ops);

        match fetch_metrics_once(&client, &uri, parsed_auth.as_ref()).await {
            Ok(body) => {
                failure_streak_warned = false;
                consecutive_failures = 0;
                let samples = parse_kepler_metrics(&body, metric_name, label_key);
                // Mirror Scaphandre: timestamp after the fetch resolves
                // so `last_update_ms` reflects when the data landed.
                let now = monotonic_ms();
                process_scrape(&state, &samples, &deltas, &cfg, &mut last_raw_joules, now);
                last_success_ms = now;
                metrics.kepler_last_scrape_age_seconds.set(0.0);
                metrics.kepler_scrape_success.inc();
                tracing::debug!(
                    samples = samples.len(),
                    services_updated = deltas.len(),
                    "Kepler scrape succeeded"
                );
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                handle_kepler_failure(
                    &e,
                    &metrics,
                    &redacted,
                    last_success_ms,
                    monotonic_ms(),
                    consecutive_failures,
                    &mut failure_streak_warned,
                    &mut unsupported_platform_warned,
                );
            }
        }
    }
}

/// Failure-branch bookkeeping: advance the staleness gauge, bump the
/// reason counter, log once at warn then debug, and emit the one-shot
/// "likely misconfigured" warning after three consecutive failures.
/// Extracted so [`run_scraper_loop`] stays under the line-count limit.
#[allow(clippy::too_many_arguments)]
fn handle_kepler_failure(
    err: &ScraperError,
    metrics: &MetricsState,
    redacted: &str,
    last_success_ms: u64,
    now_ms: u64,
    consecutive_failures: u32,
    failure_streak_warned: &mut bool,
    unsupported_platform_warned: &mut bool,
) {
    // Wall-clock age since the last successful scrape (or scraper
    // start time, when no success has happened yet) so Grafana alerts
    // on a hung scraper fire reliably from boot.
    let age_secs = now_ms.saturating_sub(last_success_ms) as f64 / 1000.0;
    metrics.kepler_last_scrape_age_seconds.set(age_secs);
    let reason = scraper_error_reason(err);
    metrics.kepler_scrape_failed.inc();
    metrics
        .kepler_scrape_failed_total
        .with_label_values(&[reason.as_str()])
        .inc();
    if *failure_streak_warned {
        tracing::debug!(error = %err, "Kepler scrape failed again");
    } else {
        tracing::warn!(
            error = %err,
            endpoint = %redacted,
            "Kepler scrape failed; subsequent failures will log at debug level"
        );
        *failure_streak_warned = true;
    }
    if !*unsupported_platform_warned
        && consecutive_failures >= UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD
    {
        tracing::warn!(
            endpoint = %redacted,
            consecutive_failures = consecutive_failures,
            "Kepler endpoint has been unreachable for {UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD} consecutive scrapes. \
             Check that Kepler is installed and serving metrics at the configured endpoint. \
             The daemon is falling back through the precedence chain for affected services. \
             See docs/LIMITATIONS.md#kepler-precision-bounds."
        );
        *unsupported_platform_warned = true;
    }
}
