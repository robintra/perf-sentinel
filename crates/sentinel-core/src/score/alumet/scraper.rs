//! Alumet scraper task, HTTP client, and error types. See
//! `docs/design/05-GREENOPS-AND-CARBON.md` for the methodology and
//! `docs/LIMITATIONS.md#alumet-precision-bounds` for the precision
//! bounds.

use std::sync::Arc;
use std::time::Duration;

use crate::http_client::{self, FetchError, HttpClient};
use crate::ingest::auth_header::{AuthHeader, ScraperAuthOutcome, parse_scraper_auth_header};
use crate::report::metrics::{AlumetScrapeReason, MetricsState};
use crate::score::ops_snapshot_diff::OpsSnapshotDiff;
use crate::score::prom_parser::parse_metric_samples;

use super::apply::apply_scrape;
use super::config::AlumetConfig;
use super::state::{AlumetState, monotonic_ms};

/// Number of consecutive scrape failures before [`run_scraper_loop`]
/// emits the one-shot "likely misconfigured endpoint" warning. Same
/// rationale as Scaphandre's threshold.
const UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD: u32 = 3;

/// Consecutive HTTP-200 ticks with zero matching samples before the
/// warn-once fires. Catches an operator-supplied `metric_name` that does
/// not exist on the wire, the most likely Alumet misconfiguration since
/// the exporter's `prefix`/`suffix` shape the name.
const ZERO_SAMPLE_WARN_THRESHOLD: u32 = 3;

/// Scrape the Alumet endpoint once via the hyper-util client.
pub(super) async fn fetch_metrics_once(
    client: &HttpClient,
    uri: &hyper::Uri,
    auth: Option<&AuthHeader>,
) -> Result<String, ScraperError> {
    let bytes = http_client::fetch_get(
        client,
        uri,
        "perf-sentinel/alumet-scraper",
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
    #[error("Alumet fetch failed")]
    Fetch(#[source] FetchError),
    #[error("Alumet response was not valid UTF-8")]
    Utf8(#[source] std::string::FromUtf8Error),
}

/// Map a [`ScraperError`] to the `reason` label used by
/// `perf_sentinel_alumet_scrape_failed_total`.
pub(super) fn scraper_error_reason(err: &ScraperError) -> AlumetScrapeReason {
    match err {
        ScraperError::Fetch(fe) => fetch_error_reason(fe),
        ScraperError::Utf8(_) => AlumetScrapeReason::InvalidUtf8,
    }
}

fn fetch_error_reason(err: &FetchError) -> AlumetScrapeReason {
    match err {
        FetchError::Transport(_) => AlumetScrapeReason::Unreachable,
        FetchError::Timeout => AlumetScrapeReason::Timeout,
        FetchError::HttpStatus(_) => AlumetScrapeReason::HttpError,
        FetchError::BodyRead(_) => AlumetScrapeReason::BodyReadError,
        FetchError::RequestBuild(_) => AlumetScrapeReason::RequestError,
    }
}

/// Spawn the periodic Alumet scraper task.
///
/// Returns a `JoinHandle` the daemon captures and aborts on Ctrl-C.
/// The task reads per-service op counts from `MetricsState`, scrapes the
/// Alumet endpoint, converts each interval-energy reading into a
/// kWh-per-op coefficient, and publishes via [`AlumetState::publish`].
#[must_use]
pub fn spawn_scraper(
    cfg: AlumetConfig,
    state: Arc<AlumetState>,
    metrics: Arc<MetricsState>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_scraper_loop(cfg, state, metrics).await;
    })
}

async fn run_scraper_loop(cfg: AlumetConfig, state: Arc<AlumetState>, metrics: Arc<MetricsState>) {
    use std::str::FromStr;

    let uri = match hyper::Uri::from_str(&cfg.endpoint) {
        Ok(u) => u,
        Err(e) => {
            // Defense in depth: validate_alumet already rejects `@` in
            // the authority, but log the redacted string in case a
            // future caller skips validation.
            tracing::error!(
                endpoint = %http_client::redact_endpoint_str(&cfg.endpoint),
                error = %e,
                "Alumet scraper aborting on invalid endpoint URI"
            );
            return;
        }
    };
    let redacted = http_client::redact_endpoint(&uri);

    let parsed_auth: Option<AuthHeader> = match parse_scraper_auth_header(
        cfg.auth_header.as_deref(),
        &cfg.endpoint,
        &redacted,
        "alumet",
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
    let mut failure_streak_warned = false;
    let mut consecutive_failures: u32 = 0;
    let mut unsupported_platform_warned = false;
    // "HTTP 200, samples empty" warn-once latch, reset on HTTP error
    // so a flapping endpoint does not falsely trip it. One independent
    // streak per cause: a metric_name that matches nothing and a
    // service_mappings table that matches nothing are distinct
    // misconfigurations with distinct fixes, and sharing one counter
    // would let a streak of the first cause fire the second cause's
    // message (or a latched first warn suppress the second forever).
    let mut no_samples_streak = WarnOnceStreak::default();
    let mut no_match_streak = WarnOnceStreak::default();
    // Track the last successful scrape so the `last_scrape_age_seconds`
    // gauge advances on every failure tick. Seeded to scraper-start time
    // so an Alumet endpoint broken from boot still climbs the gauge.
    let mut last_success_ms: u64 = monotonic_ms();

    // `energy_interval_secs` is echoed at startup on purpose: it is the
    // one config value that silently rescales every reading when it
    // drifts from the Alumet-side `poll_interval`, and it cannot be
    // cross-checked against the wire.
    tracing::info!(
        endpoint = %redacted,
        scrape_interval_secs = cfg.scrape_interval.as_secs(),
        metric = %cfg.metric_name,
        label = %cfg.label_key,
        energy_interval_secs = cfg.energy_interval_secs,
        service_count = cfg.service_mappings.len(),
        "Alumet scraper started"
    );
    if cfg.service_mappings.is_empty() {
        tracing::warn!(
            endpoint = %redacted,
            "[green.alumet] service_mappings is empty: the scraper will \
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
                let samples = parse_metric_samples(&body, &cfg.metric_name, &cfg.label_key);
                // Mirror Scaphandre: timestamp after the fetch resolves
                // so `last_update_ms` reflects when the data landed.
                let now = monotonic_ms();
                let matched = apply_scrape(&state, &samples, &deltas, &cfg, now);
                last_success_ms = now;
                metrics.alumet_last_scrape_age_seconds.set(0.0);
                metrics.alumet_scrape_success.inc();
                track_zero_sample_streak(
                    samples.len(),
                    matched,
                    deltas.len(),
                    cfg.service_mappings.len(),
                    &redacted,
                    &cfg.metric_name,
                    &cfg.label_key,
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
                handle_alumet_failure(
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
fn handle_alumet_failure(
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
    metrics.alumet_last_scrape_age_seconds.set(age_secs);
    let reason = scraper_error_reason(err);
    metrics.alumet_scrape_failed.inc();
    metrics
        .alumet_scrape_failed_total
        .with_label_values(&[reason.as_str()])
        .inc();
    if *failure_streak_warned {
        tracing::debug!(error = %err, "Alumet scrape failed again");
    } else {
        tracing::warn!(
            error = %err,
            endpoint = %redacted,
            "Alumet scrape failed; subsequent failures will log at debug level"
        );
        *failure_streak_warned = true;
    }
    if !*unsupported_platform_warned
        && consecutive_failures >= UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD
    {
        tracing::warn!(
            endpoint = %redacted,
            consecutive_failures = consecutive_failures,
            "Alumet endpoint has been unreachable for {UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD} consecutive scrapes. \
             Check that the Alumet agent is running with the prometheus-exporter plugin enabled \
             and serving metrics at the configured endpoint. \
             The daemon is falling back through the precedence chain for affected services. \
             See docs/LIMITATIONS.md#alumet-precision-bounds."
        );
        *unsupported_platform_warned = true;
    }
}

/// One warn-once streak: counts consecutive bad ticks for a single
/// cause and fires at most once per streak.
#[derive(Default)]
pub(super) struct WarnOnceStreak {
    ticks: u32,
    warned: bool,
}

impl WarnOnceStreak {
    /// Record one bad tick. Returns `true` exactly when the warn should
    /// fire now (threshold reached, not yet warned this streak).
    fn tick(&mut self) -> bool {
        self.ticks = self.ticks.saturating_add(1);
        if !self.warned && self.ticks >= ZERO_SAMPLE_WARN_THRESHOLD {
            self.warned = true;
            return true;
        }
        false
    }

    pub(super) fn reset(&mut self) {
        self.ticks = 0;
        self.warned = false;
    }

    #[cfg(test)]
    pub(super) fn has_warned(&self) -> bool {
        self.warned
    }
}

/// Success-branch latches: warn once per streak of
/// [`ZERO_SAMPLE_WARN_THRESHOLD`] consecutive HTTP-200 ticks that
/// produced nothing usable. Extracted for the line-count limit and to
/// unit-test the warn-once edges.
///
/// Two independent causes, two independent streaks:
///
/// - `no_samples` fires when `metric_name`/`label_key` match nothing on
///   the wire. A tick with no samples says nothing about the mappings,
///   so it neither advances nor resets `no_match`.
/// - `no_match` fires when samples flow but zero `service_mappings`
///   label values are present. That is either mistyped mapping values
///   or every mapped workload currently absent from the exposition, the
///   message names both since the wire cannot tell them apart. Gated on
///   a non-empty mappings table (an empty table trivially matches
///   nothing and gets its own startup warning instead).
///
/// A partially wrong table (some mappings match, others never do) trips
/// neither latch, the per-tick `services_matched` debug field and the
/// report-level `per_service_energy_model` are the signals for that.
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
                "Alumet endpoint replied HTTP 200 but no samples matched \
                 the configured metric across the last {ZERO_SAMPLE_WARN_THRESHOLD} ticks. \
                 Most common cause: metric_name does not match the wire. \
                 Alumet's prometheus-exporter prepends `prefix` and \
                 appends `suffix` (default '_alumet') to every metric \
                 name, and an energy-attribution series is named after \
                 the operator's formula. Run \
                 `curl <endpoint> | grep -i energy` and copy the name \
                 verbatim. Other cause: label_key absent from the series.",
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
                    "Alumet endpoint replied HTTP 200 and metric_name matched \
                     samples, but none of the configured service_mappings \
                     label values were present under label_key across the \
                     last {ZERO_SAMPLE_WARN_THRESHOLD} such ticks, so no measured \
                     coefficient is being published. Either the mapping \
                     values are mistyped (they must match the label value \
                     verbatim), or every mapped workload is currently absent \
                     from the exposition (scaled to zero, not yet scheduled). \
                     Inspect the live values for the configured label key \
                     with curl against the endpoint.",
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
        "Alumet scrape succeeded"
    );
}
