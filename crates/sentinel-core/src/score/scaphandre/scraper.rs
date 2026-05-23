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

use crate::http_client::{self, FetchError, HttpClient};
use crate::ingest::auth_header::{AuthHeader, ScraperAuthOutcome, parse_scraper_auth_header};
use crate::report::metrics::{MetricsState, ScaphandreScrapeReason};

use crate::score::ops_snapshot_diff::OpsSnapshotDiff;

use super::config::ScaphandreConfig;
use super::ops::apply_scrape;
use super::parser::parse_scaphandre_metrics;
use super::state::{ScaphandreState, monotonic_ms};

/// Number of consecutive scrape failures before [`run_scraper_loop`]
/// emits the one-shot "likely unsupported platform" warning. 3 is
/// enough to rule out transient network blips on a working host
/// without delaying the diagnostic too long on a misconfigured host.
const UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD: u32 = 3;

/// Number of consecutive HTTP-200 ticks with zero parsed
/// `ProcessPower` readings before [`run_scraper_loop`] emits the
/// one-shot "no samples matched" warning. Aimed at silent upstream
/// renames of `scaph_process_power_consumption_microwatts` or the
/// `exe` label that the parser would silently filter out, leaving
/// every mapped service on the proxy model with no operator signal.
/// Same threshold as Kepler's analogous net.
const ZERO_SAMPLE_WARN_THRESHOLD: u32 = 3;

/// Substring grep'd by the CI wire-conformance workflow
/// (`.github/workflows/upstream-wire-conformance.yml`) to detect that
/// the zero-sample warn has NOT fired on a healthy run. Wired through
/// the warn's `format_args` capture so a doc-pass rename of the warn
/// text propagates to the constant. The unit test
/// `zero_sample_warn_marker_is_stable` anchors the literal value so a
/// reviewer renaming the constant has to also update the CI workflow.
pub(super) const ZERO_SAMPLE_WARN_MARKER: &str = "no samples matched the configured metric";

/// Scrape the Scaphandre endpoint once via the hyper-util client.
/// Returns the response body as a `String` on success.
///
/// A 3-second hard timeout guards against hung endpoints. Non-2xx
/// responses are treated as errors. The body is wrapped in
/// [`http_body_util::Limited`] with [`MAX_BODY_BYTES`] so a
/// runaway endpoint cannot OOM the daemon. The function takes the
/// pre-parsed `Uri` and the long-lived [`HttpClient`] by
/// reference so connection pooling actually kicks in.
pub(super) async fn fetch_metrics_once(
    client: &HttpClient,
    uri: &hyper::Uri,
    auth: Option<&AuthHeader>,
) -> Result<String, ScraperError> {
    let bytes = http_client::fetch_get(
        client,
        uri,
        "perf-sentinel/scaphandre-scraper",
        Duration::from_secs(3),
        auth,
    )
    .await
    .map_err(ScraperError::Fetch)?;
    String::from_utf8(bytes.to_vec()).map_err(ScraperError::Utf8)
}

/// Errors the scraper task might emit. They are never returned to the
/// caller, the scraper task logs them via `tracing` with the
/// warn-once pattern and continues running.
///
/// `#[from]` and `#[source]` attributes preserve the underlying error
/// chain so `tracing::warn!(error = %e)` walks down to the root cause
/// (hyper / http / `FromUtf8` / etc.) instead of throwing it away in a
/// `format!`.
#[derive(Debug, thiserror::Error)]
pub(super) enum ScraperError {
    /// Endpoint URI failed to parse at config-load or scraper-startup time.
    /// The `endpoint` field is the redacted URL (no userinfo), see
    /// [`http_client::redact_endpoint`].
    #[error("invalid Scaphandre endpoint URI '{endpoint}'")]
    InvalidUri {
        endpoint: String,
        #[source]
        source: hyper::http::uri::InvalidUri,
    },
    /// HTTP fetch failed (request build, transport, timeout, body
    /// read or non-2xx status). Delegates to the shared
    /// [`FetchError`].
    #[error("Scaphandre fetch failed")]
    Fetch(#[source] FetchError),
    #[error("Scaphandre response was not valid UTF-8")]
    Utf8(#[source] std::string::FromUtf8Error),
}

/// Map a [`ScraperError`] from a single scrape attempt to the
/// `reason` label used by `perf_sentinel_scaphandre_scrape_failed_total`.
///
/// `InvalidUri` cannot reach this path: `run_scraper_loop` aborts at
/// startup if URI parsing fails, before the first tick. We still
/// match it explicitly so a future refactor that surfaces the error
/// elsewhere does not silently lose its accounting.
pub(super) fn scraper_error_reason(err: &ScraperError) -> ScaphandreScrapeReason {
    match err {
        ScraperError::Fetch(fe) => fetch_error_reason(fe),
        ScraperError::Utf8(_) => ScaphandreScrapeReason::InvalidUtf8,
        ScraperError::InvalidUri { .. } => ScaphandreScrapeReason::RequestError,
    }
}

/// Map a [`FetchError`] to the corresponding scrape failure reason.
/// Cloud-energy keeps a parallel mapping with its own reason enum;
/// merging would require a trait or a wider enum, kept separate today.
fn fetch_error_reason(err: &FetchError) -> ScaphandreScrapeReason {
    match err {
        FetchError::Transport(_) => ScaphandreScrapeReason::Unreachable,
        FetchError::Timeout => ScaphandreScrapeReason::Timeout,
        FetchError::HttpStatus(_) => ScaphandreScrapeReason::HttpError,
        FetchError::BodyRead(_) => ScaphandreScrapeReason::BodyReadError,
        FetchError::RequestBuild(_) => ScaphandreScrapeReason::RequestError,
    }
}

/// Spawn the periodic Scaphandre scraper task.
///
/// Returns a `JoinHandle` that the daemon captures and aborts on
/// Ctrl-C shutdown. The task runs until aborted or until the endpoint
/// produces an unrecoverable error (the current implementation keeps
/// running across failures, see the warn-once log pattern below).
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
/// Owns one [`HttpClient`] for the lifetime of the scraper task so
/// hyper-util can pool the underlying TCP connection between scrapes.
/// Parses the endpoint URI once at startup; if the URI is malformed
/// the task logs and exits cleanly without retrying, a user-facing
/// config error should fail loud, not warn-spam every 5 s.
// Length sits just above the default cap because of the cumulative
// state (auth, ticker, multiple warn-once latches) the loop has to
// own. Splitting now would just move the state through more function
// boundaries without making the control flow clearer.
#[allow(clippy::too_many_lines)]
async fn run_scraper_loop(
    cfg: ScaphandreConfig,
    state: Arc<ScaphandreState>,
    metrics: Arc<MetricsState>,
) {
    use std::str::FromStr;

    // Parse the URI once. The config validator at config.rs::validate_green
    // already runs the same parse at config-load time, so this should never
    // fail in practice, but we keep the runtime guard so the scraper task
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
    let redacted = http_client::redact_endpoint(&uri);

    let parsed_auth: Option<AuthHeader> = match parse_scraper_auth_header(
        cfg.auth_header.as_deref(),
        &cfg.endpoint,
        &redacted,
        "scaphandre",
    ) {
        ScraperAuthOutcome::Invalid => return,
        ScraperAuthOutcome::None => None,
        ScraperAuthOutcome::Some(h) => Some(h),
    };

    let client = http_client::build_client();

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
    // Per-service latch for the ambiguous-matcher warn: warn once on
    // entering the ambiguous state, debug on subsequent ambiguous
    // ticks, clear on a clean match so a later flap re-warns. Carried
    // across loop iterations by the scraper.
    let mut multi_match_warned: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // "HTTP 200, samples empty" warn-once latch, reset on HTTP error
    // so a flapping endpoint does not falsely trip it. Detects silent
    // upstream renames of the metric or the `exe` label.
    let mut consecutive_zero_sample_ticks: u32 = 0;
    let mut zero_sample_warned = false;
    // Wall-clock anchor for the staleness gauge. Seeded at scraper
    // start so a never-succeeded scraper still climbs the gauge from
    // boot (the failure branch reads this on every tick to set
    // `scaphandre_last_scrape_age_seconds`). Mirrors Kepler's idiom
    // (kepler/scraper.rs::last_success_ms).
    let mut last_success_ms: u64 = monotonic_ms();

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

        match fetch_metrics_once(&client, &uri, parsed_auth.as_ref()).await {
            Ok(body) => {
                first_failure_warned = false;
                consecutive_failures = 0;
                let readings = parse_scaphandre_metrics(&body);
                let now = monotonic_ms();
                apply_scrape(
                    &state,
                    &readings,
                    &deltas,
                    &cfg,
                    &mut multi_match_warned,
                    now,
                );
                // Update the "last successful scrape age" gauge to 0 ,
                // Grafana rate() / alerting rules catch hung scrapers
                // by watching the gauge climb.
                metrics.scaphandre_last_scrape_age_seconds.set(0.0);
                last_success_ms = now;
                metrics.scaphandre_scrape_success.inc();
                track_zero_sample_streak(
                    readings.len(),
                    deltas.len(),
                    &redacted,
                    &mut consecutive_zero_sample_ticks,
                    &mut zero_sample_warned,
                );
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                // Advance the staleness gauge so Grafana alerts on a
                // hung scraper still fire on the failure branch (Kepler
                // parity, kepler/scraper.rs::handle_kepler_failure).
                #[allow(clippy::cast_precision_loss)]
                let age_secs = monotonic_ms().saturating_sub(last_success_ms) as f64 / 1000.0;
                metrics.scaphandre_last_scrape_age_seconds.set(age_secs);
                // Reset the empty-sample latch on HTTP failure: the
                // failure-side warning covers flapping endpoints.
                consecutive_zero_sample_ticks = 0;
                zero_sample_warned = false;
                let reason = scraper_error_reason(&e);
                // Two counters per failure: status rate (cached) and
                // reason breakdown (cold lookup). PromQL invariant:
                // sum(failed_total) == total{status="failed"}.
                metrics.scaphandre_scrape_failed.inc();
                metrics
                    .scaphandre_scrape_failed_total
                    .with_label_values(&[reason.as_str()])
                    .inc();
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

/// Success-branch latch: warn once after [`ZERO_SAMPLE_WARN_THRESHOLD`]
/// consecutive HTTP-200 ticks with zero parsed `ProcessPower` readings.
/// Always emits a `debug` line summarising the scrape, so a normal run
/// still produces a trace at `RUST_LOG=debug`.
///
/// Mirrors the Kepler analogue (`kepler/scraper.rs::track_zero_sample_streak`).
/// The Scaphandre variant takes no per-call metric/label arguments
/// because both are fixed (`scaph_process_power_consumption_microwatts`
/// + `exe`), the warn message hard-codes them.
pub(super) fn track_zero_sample_streak(
    samples_len: usize,
    services_updated: usize,
    redacted: &str,
    consecutive_zero_sample_ticks: &mut u32,
    zero_sample_warned: &mut bool,
) {
    if samples_len == 0 {
        *consecutive_zero_sample_ticks = consecutive_zero_sample_ticks.saturating_add(1);
        if !*zero_sample_warned && *consecutive_zero_sample_ticks >= ZERO_SAMPLE_WARN_THRESHOLD {
            let ticks = *consecutive_zero_sample_ticks;
            tracing::warn!(
                endpoint = %redacted,
                metric = "scaph_process_power_consumption_microwatts",
                label = "exe",
                ticks,
                "Scaphandre endpoint replied HTTP 200 but {ZERO_SAMPLE_WARN_MARKER} \
                 across the last {ticks} ticks. Most common cause: an upstream \
                 rename of the metric name or the `exe` label. Other causes: \
                 Scaphandre started with `--no-procfs`, a host-only build that \
                 omits the per-process exposition, or a topology refresh that \
                 has not yet enumerated any process. A host without RAPL is a \
                 SEPARATE failure mode where per-process lines DO appear with \
                 `power=0`, that case is not caught by this warn. The daemon \
                 is falling back to the proxy model for every mapped service. \
                 See docs/LIMITATIONS.md#scaphandre-precision-bounds."
            );
            *zero_sample_warned = true;
        }
    } else {
        *consecutive_zero_sample_ticks = 0;
        *zero_sample_warned = false;
    }
    tracing::debug!(
        samples = samples_len,
        services_updated = services_updated,
        "Scaphandre scrape succeeded"
    );
}
