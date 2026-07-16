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
use super::state::{KeplerState, monotonic_ms};
use crate::score::alumet::scraper::WarnOnceStreak;
use crate::score::prom_parser::parse_metric_samples;

/// Number of consecutive scrape failures before [`run_scraper_loop`]
/// emits the one-shot "likely misconfigured endpoint" warning. Same
/// rationale as Scaphandre's threshold.
const UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD: u32 = 3;

/// Consecutive HTTP-200 ticks with zero matching samples before the
/// warn-once fires. Catches the Kepler v1 endpoint pitfall (legacy
/// metric names, no `_cpu_` infix).
const ZERO_SAMPLE_WARN_THRESHOLD: u32 = 3;

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
    // "HTTP 200, nothing usable" warn-once latches, reset on HTTP
    // error so a flapping endpoint does not falsely trip them. One
    // independent streak per cause, same rationale as the Alumet
    // scraper: an empty exposition and a mappings table that matches
    // nothing are distinct misconfigurations with distinct fixes.
    let mut no_samples_streak = WarnOnceStreak::default();
    let mut no_match_streak = WarnOnceStreak::default();
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
    if cfg.service_mappings.is_empty() {
        tracing::warn!(
            endpoint = %redacted,
            "[green.kepler] service_mappings is empty: the scraper will \
             poll the endpoint but can never attribute energy to a \
             service. Add mappings to publish measured coefficients."
        );
    }

    loop {
        ticker.tick().await;

        let current_ops = metrics.snapshot_service_io_ops();
        let deltas = snapshot_diff.delta_and_advance(current_ops);

        match fetch_metrics_once(&client, &uri, parsed_auth.as_ref()).await {
            Ok(body) => {
                failure_streak_warned = false;
                consecutive_failures = 0;
                let samples = parse_metric_samples(&body, metric_name, label_key);
                // Mirror Scaphandre: timestamp after the fetch resolves
                // so `last_update_ms` reflects when the data landed.
                let now = monotonic_ms();
                let matched =
                    process_scrape(&state, &samples, &deltas, &cfg, &mut last_raw_joules, now);
                last_success_ms = now;
                metrics.kepler_last_scrape_age_seconds.set(0.0);
                metrics.kepler_scrape_success.inc();
                track_zero_sample_streak(
                    samples.len(),
                    matched,
                    deltas.len(),
                    cfg.service_mappings.len(),
                    &redacted,
                    metric_name,
                    label_key,
                    &mut no_samples_streak,
                    &mut no_match_streak,
                );
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                // Reset both latches on HTTP failure: the failure-side
                // warning covers flapping endpoints.
                no_samples_streak.reset();
                no_match_streak.reset();
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

/// Success-branch latches: warn once per streak of
/// [`ZERO_SAMPLE_WARN_THRESHOLD`] consecutive HTTP-200 ticks that
/// produced nothing usable. Extracted for the line-count limit and to
/// unit-test the warn-once edges.
///
/// Same two-cause split as the Alumet scraper: `no_samples` fires when
/// the metric is absent from the wire (legacy Kepler names, wrong
/// `metric_kind`), `no_match` fires when samples flow but zero
/// `service_mappings` label values are present (mistyped values, or
/// every mapped workload absent from the exposition). A tick with no
/// samples says nothing about the mappings, so it neither advances nor
/// resets `no_match`.
#[allow(clippy::too_many_arguments)]
pub(super) fn track_zero_sample_streak(
    samples_len: usize,
    services_matched: usize,
    services_with_ops: usize,
    mapping_count: usize,
    redacted: &str,
    metric_name: &str,
    label_key: &str,
    no_samples: &mut WarnOnceStreak,
    no_match: &mut WarnOnceStreak,
) {
    if samples_len == 0 {
        if no_samples.tick() {
            tracing::warn!(
                endpoint = %redacted,
                metric = metric_name,
                label = label_key,
                "Kepler endpoint replied HTTP 200 but no samples matched \
                 the configured metric across the last {ZERO_SAMPLE_WARN_THRESHOLD} ticks. \
                 Most common cause: the cluster runs a Kepler exporter \
                 older than v0.10 (legacy metric names without the \
                 '_cpu_' infix). Other cause: metric_kind mismatched \
                 with the deployment topology.",
            );
        }
    } else {
        no_samples.reset();
        if services_matched == 0 && mapping_count > 0 {
            if no_match.tick() {
                tracing::warn!(
                    endpoint = %redacted,
                    metric = metric_name,
                    label = label_key,
                    samples = samples_len,
                    "Kepler endpoint replied HTTP 200 and the metric matched \
                     samples, but none of the configured service_mappings \
                     label values were present under the label across the \
                     last {ZERO_SAMPLE_WARN_THRESHOLD} such ticks, so no measured \
                     coefficient is being published. Either the mapping \
                     values are mistyped (container name for 'container', \
                     kernel comm for 'process', matched verbatim), or every \
                     mapped workload is currently absent from the \
                     exposition (scaled to zero, not yet scheduled).",
                );
            }
        } else {
            no_match.reset();
        }
    }
    tracing::debug!(
        samples = samples_len,
        services_matched = services_matched,
        services_with_ops = services_with_ops,
        "Kepler scrape succeeded"
    );
}
