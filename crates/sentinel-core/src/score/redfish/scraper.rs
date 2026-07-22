//! Redfish scraper task, HTTP client, and error types. Iterates the
//! configured chassis endpoints per tick, parses the wattage gauge for
//! whichever schema each endpoint declares (`legacy_power` →
//! `PowerControl[0].PowerConsumedWatts`, `environment_metrics` →
//! `PowerWatts.Reading`), and publishes per-service coefficients via
//! [`apply_chassis_scrape`]. TLS uses the shared webpki client, the
//! `ca_bundle_path` deferral rationale lives in design doc 05.

use std::sync::Arc;
use std::time::Duration;

use crate::http_client::{self, FetchError, HttpClient};
use crate::ingest::auth_header::{AuthHeader, ScraperAuthOutcome, parse_scraper_auth_header};
use crate::report::metrics::{MetricsState, RedfishScrapeReason};
use crate::score::ops_snapshot_diff::OpsSnapshotDiff;

use std::collections::HashMap;

use super::apply::{apply_chassis_scrape, build_chassis_services};
use super::config::{RedfishConfig, RedfishSchema};
use super::parser::{ParseOutcome, parse_redfish_power};
use super::state::{RedfishState, ServiceEnergy, monotonic_ms};

/// Three consecutive failures across all chassis trigger the one-shot
/// "endpoint likely misconfigured" warning.
const UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD: u32 = 3;

/// Errors the scraper task might emit. Logged via `tracing` and folded
/// into the scrape-reason counter, never returned to the caller.
#[derive(Debug, thiserror::Error)]
pub(super) enum ScraperError {
    #[error("Redfish fetch failed")]
    Fetch(#[source] FetchError),
    #[error("Redfish response was not valid UTF-8")]
    Utf8(#[source] std::string::FromUtf8Error),
    #[error("Redfish JSON parse failed")]
    InvalidJson,
    #[error("Redfish power path missing from response")]
    PathMissing,
    #[error("Redfish power value was non-finite, null, or non-positive")]
    InvalidValue,
}

pub(super) fn scraper_error_reason(err: &ScraperError) -> RedfishScrapeReason {
    match err {
        ScraperError::Fetch(fe) => fetch_error_reason(fe),
        ScraperError::Utf8(_) => RedfishScrapeReason::InvalidUtf8,
        ScraperError::InvalidJson => RedfishScrapeReason::InvalidJson,
        ScraperError::PathMissing => RedfishScrapeReason::PathMissing,
        ScraperError::InvalidValue => RedfishScrapeReason::InvalidValue,
    }
}

fn fetch_error_reason(err: &FetchError) -> RedfishScrapeReason {
    match err {
        FetchError::Transport(_) => RedfishScrapeReason::Unreachable,
        FetchError::Timeout => RedfishScrapeReason::Timeout,
        FetchError::HttpStatus(_) => RedfishScrapeReason::HttpError,
        FetchError::BodyRead(_) => RedfishScrapeReason::BodyReadError,
        FetchError::RequestBuild(_) => RedfishScrapeReason::RequestError,
    }
}

/// Spawn the periodic Redfish scraper task.
#[must_use]
pub fn spawn_scraper(
    cfg: RedfishConfig,
    state: Arc<RedfishState>,
    metrics: Arc<MetricsState>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_scraper_loop(cfg, state, metrics).await;
    })
}

async fn fetch_chassis_once(
    client: &HttpClient,
    uri: &hyper::Uri,
    auth: Option<&AuthHeader>,
) -> Result<String, ScraperError> {
    let bytes = http_client::fetch_get(
        client,
        uri,
        "perf-sentinel/redfish-scraper",
        Duration::from_secs(5),
        auth,
    )
    .await
    .map_err(ScraperError::Fetch)?;
    String::from_utf8(bytes.to_vec()).map_err(ScraperError::Utf8)
}

/// One scrape attempt against a single chassis. Returns the parsed
/// wattage on success.
async fn scrape_chassis(
    client: &HttpClient,
    uri: &hyper::Uri,
    auth: Option<&AuthHeader>,
    schema: RedfishSchema,
) -> Result<f64, ScraperError> {
    let body = fetch_chassis_once(client, uri, auth).await?;
    match parse_redfish_power(&body, schema) {
        ParseOutcome::Ok(w) => Ok(w),
        ParseOutcome::InvalidJson => Err(ScraperError::InvalidJson),
        ParseOutcome::PathMissing => Err(ScraperError::PathMissing),
        ParseOutcome::InvalidValue => Err(ScraperError::InvalidValue),
    }
}

/// Pre-parse the configured endpoints into `(chassis_id, Uri, schema)`
/// triples once at startup. Invalid URIs are surfaced via an
/// error-level log and the chassis is dropped from the rotation, so a
/// single malformed entry does not silently kill the whole scraper.
/// Whether a configured auth header would travel to any chassis over
/// cleartext `http://`. Extracted for testing: the caller only logs.
pub(super) fn credentials_travel_cleartext(
    has_auth: bool,
    uris: &[(String, hyper::Uri, RedfishSchema)],
) -> bool {
    has_auth
        && uris
            .iter()
            .any(|(_, uri, _)| uri.scheme_str() == Some("http"))
}

fn parse_chassis_uris(cfg: &RedfishConfig) -> Vec<(String, hyper::Uri, RedfishSchema)> {
    use std::str::FromStr;
    let mut out = Vec::with_capacity(cfg.endpoints.len());
    for (id, endpoint) in &cfg.endpoints {
        match hyper::Uri::from_str(&endpoint.url) {
            Ok(u) => out.push((id.clone(), u, endpoint.schema)),
            Err(e) => {
                tracing::error!(
                    chassis = %id,
                    error = %e,
                    "Redfish scraper dropping chassis: endpoint URI failed to parse"
                );
            }
        }
    }
    out
}

/// Per-chassis path within one tick. Refactored out of
/// [`run_scraper_loop`] so the loop stays under the line-count limit
/// without an explicit allow.
fn record_chassis_failure(
    chassis_id: &str,
    uri: &hyper::Uri,
    err: &ScraperError,
    metrics: &MetricsState,
    failure_streak_warned: &mut bool,
) {
    let reason = scraper_error_reason(err);
    metrics.redfish_scrape_failed.inc();
    metrics
        .redfish_scrape_failed_total
        .with_label_values(&[reason.as_str()])
        .inc();
    if *failure_streak_warned {
        tracing::debug!(
            chassis = %chassis_id,
            error = %err,
            "Redfish scrape failed again"
        );
    } else {
        tracing::warn!(
            chassis = %chassis_id,
            error = %err,
            endpoint = %http_client::redact_endpoint(uri),
            "Redfish scrape failed; subsequent failures will log at debug level"
        );
        *failure_streak_warned = true;
    }
}

/// Per-tick context kept on the stack and reused across chassis. All
/// fields are read-only inside `run_tick`, so `deltas` is borrowed to
/// make the read-only contract explicit (the owned `HashMap` stays in
/// the caller's stack frame).
struct TickContext<'a> {
    client: &'a HttpClient,
    auth: Option<&'a AuthHeader>,
    chassis_services: &'a HashMap<String, Vec<String>>,
    deltas: &'a HashMap<String, u64>,
    scrape_interval_secs: f64,
    now_ms: u64,
}

/// Outcome of one tick's chassis sweep. Named struct rather than a
/// `(bool, bool)` tuple so the call-site reads as
/// `outcome.any_success` / `outcome.any_change` instead of `.0` / `.1`.
struct TickOutcome {
    any_success: bool,
    any_change: bool,
}

/// Iterate the chassis URIs in one tick and mutate `next` in place.
/// Factored out of [`run_scraper_loop`] so the loop body stays under
/// the line cap.
async fn run_tick(
    uris: &[(String, hyper::Uri, RedfishSchema)],
    ctx: &TickContext<'_>,
    next: &mut HashMap<String, ServiceEnergy>,
    metrics: &MetricsState,
    failure_streak_warned: &mut bool,
) -> TickOutcome {
    let mut outcome = TickOutcome {
        any_success: false,
        any_change: false,
    };
    for (chassis_id, uri, schema) in uris {
        match scrape_chassis(ctx.client, uri, ctx.auth, *schema).await {
            Ok(watts) => {
                outcome.any_success = true;
                metrics.redfish_scrape_success.inc();
                // Chassis without mapped services produce no work, fall
                // through to an empty slice without allocating.
                let services: &[String] = ctx
                    .chassis_services
                    .get(chassis_id)
                    .map_or(&[], Vec::as_slice);
                if apply_chassis_scrape(
                    next,
                    services,
                    watts,
                    ctx.scrape_interval_secs,
                    ctx.deltas,
                    ctx.now_ms,
                ) {
                    outcome.any_change = true;
                }
                tracing::debug!(chassis = %chassis_id, watts = watts, "Redfish scrape succeeded");
            }
            Err(e) => {
                record_chassis_failure(chassis_id, uri, &e, metrics, failure_streak_warned);
            }
        }
    }
    outcome
}

async fn run_scraper_loop(
    cfg: RedfishConfig,
    state: Arc<RedfishState>,
    metrics: Arc<MetricsState>,
) {
    // Fail loud: ca_bundle_path is reserved for a follow-up. See
    // design doc 05 "Redfish TLS limitation".
    if cfg.ca_bundle_path.is_some() {
        tracing::error!(
            "[green.redfish] ca_bundle_path is set but custom-CA TLS support \
             is not yet implemented. The scraper will not start. Front the BMC \
             with a reverse proxy presenting a publicly-signed cert, or remove \
             ca_bundle_path to use the public TLS roots. See \
             docs/LIMITATIONS.md#redfish-bmc-precision-bounds."
        );
        return;
    }

    let uris = parse_chassis_uris(&cfg);
    if uris.is_empty() {
        tracing::error!(
            "[green.redfish] no valid chassis endpoints after URI parse, \
             scraper will not start"
        );
        return;
    }

    // parse_scraper_auth_header keys its cleartext-credential warning off the
    // endpoint string, but we pass a synthetic label below, so it can never
    // see a real http:// BMC URL. Warn here on the actual chassis URIs when
    // credentials would travel over cleartext.
    if credentials_travel_cleartext(cfg.auth_header.is_some(), &uris) {
        tracing::warn!(
            "[green.redfish] sending auth header over cleartext HTTP to one or more \
             BMC chassis, prefer https:// to avoid credential leak"
        );
    }

    // The auth header is shared across every chassis. Pass a synthetic
    // identifier so the parse log line does not single out chassis-0.
    let auth_context = format!("redfish ({} chassis)", uris.len());
    let parsed_auth: Option<AuthHeader> = match parse_scraper_auth_header(
        cfg.auth_header.as_deref(),
        &auth_context,
        &auth_context,
        "redfish",
    ) {
        ScraperAuthOutcome::Invalid => return,
        ScraperAuthOutcome::None => None,
        ScraperAuthOutcome::Some(h) => Some(h),
    };

    // chassis_id → services-on-this-chassis, computed once. Avoids a
    // per-tick walk of cfg.service_mappings inside apply_chassis_scrape.
    let chassis_services = build_chassis_services(&cfg.service_mappings);

    let client = http_client::build_client();
    let mut ticker = tokio::time::interval(cfg.scrape_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    let mut snapshot_diff = OpsSnapshotDiff::default();
    let mut failure_streak_warned = false;
    let mut consecutive_failures: u32 = 0;
    let mut unsupported_platform_warned = false;
    // Track the last tick where at least one chassis succeeded so the
    // staleness gauge advances when every chassis starts failing.
    // Seeded to scraper-start time so a BMC fleet broken from boot
    // still climbs the gauge from the first failure onwards.
    let mut last_success_ms: u64 = monotonic_ms();
    let scrape_interval_secs = cfg.scrape_interval.as_secs_f64();

    tracing::info!(
        chassis_count = uris.len(),
        scrape_interval_secs = cfg.scrape_interval.as_secs(),
        service_count = cfg.service_mappings.len(),
        "Redfish scraper started"
    );

    loop {
        ticker.tick().await;

        let current_ops = metrics.snapshot_service_io_ops();
        let deltas = snapshot_diff.delta_and_advance(current_ops);
        let now = monotonic_ms();

        // Hoist current_owned() and publish() out of the chassis loop:
        // pay one deep clone + one ArcSwap per tick instead of C.
        let mut next = state.current_owned();
        let ctx = TickContext {
            client: &client,
            auth: parsed_auth.as_ref(),
            chassis_services: &chassis_services,
            deltas: &deltas,
            scrape_interval_secs,
            now_ms: now,
        };
        let TickOutcome {
            any_success,
            any_change,
        } = run_tick(&uris, &ctx, &mut next, &metrics, &mut failure_streak_warned).await;
        if any_change {
            state.publish(next);
        }

        if any_success {
            failure_streak_warned = false;
            consecutive_failures = 0;
            last_success_ms = now;
            metrics.redfish_last_scrape_age_seconds.set(0.0);
        } else {
            consecutive_failures = consecutive_failures.saturating_add(1);
            // Wall-clock age since the last successful scrape (or
            // scraper-start time, when no chassis has ever succeeded).
            let age_secs = now.saturating_sub(last_success_ms) as f64 / 1000.0;
            metrics.redfish_last_scrape_age_seconds.set(age_secs);
            if !unsupported_platform_warned
                && consecutive_failures >= UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD
            {
                tracing::warn!(
                    consecutive_failures = consecutive_failures,
                    "Every Redfish chassis has been unreachable for \
                     {UNSUPPORTED_PLATFORM_FAILURE_THRESHOLD} consecutive scrapes. \
                     Check the BMC endpoints, credentials, and TLS. \
                     The daemon is falling back through the precedence chain. \
                     See docs/LIMITATIONS.md#redfish-bmc-precision-bounds."
                );
                unsupported_platform_warned = true;
            }
        }
    }
}
