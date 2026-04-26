//! Daemon main event loop: ingest batches, evict expired traces, and route
//! the resulting traces through detect + score + metrics + findings store.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, interval};

use crate::correlate::Trace;
use crate::correlate::window::TraceWindow;
use crate::detect;
use crate::detect::{Confidence, DetectConfig};
use crate::event::SpanEvent;
use crate::normalize;
use crate::report::GreenSummary;
use crate::report::metrics::MetricsState;
use crate::score;
use crate::score::cloud_energy::CloudEnergyState;
use crate::score::electricity_maps::ElectricityMapsState;
use crate::score::scaphandre::ScaphandreState;

use super::findings_store;
use super::sampling::apply_sampling;

/// Config slice the main event loop needs, the values that are pulled out
/// of `Config` once at startup and never change.
#[derive(Clone, Copy)]
pub(super) struct EventLoopConfig {
    pub(super) green_enabled: bool,
    pub(super) sampling_rate: f64,
    pub(super) evict_ms: u64,
    pub(super) confidence: Confidence,
}

/// Bundle of handles aborted on Ctrl-C.
pub(super) struct ShutdownTargets<'a> {
    pub(super) energy: EnergyScraperHandles<'a>,
    pub(super) listeners: ListenerHandles<'a>,
}

/// `JoinHandle`s for the optional energy / intensity scrapers.
#[derive(Clone, Copy)]
pub(super) struct EnergyScraperHandles<'a> {
    pub(super) scaphandre: Option<&'a tokio::task::JoinHandle<()>>,
    pub(super) cloud: Option<&'a tokio::task::JoinHandle<()>>,
    pub(super) emaps: Option<&'a tokio::task::JoinHandle<()>>,
}

/// `JoinHandle`s for the listener tasks bound at startup.
#[derive(Clone, Copy)]
pub(super) struct ListenerHandles<'a> {
    pub(super) grpc: &'a tokio::task::JoinHandle<()>,
    pub(super) http: &'a tokio::task::JoinHandle<()>,
    pub(super) json_socket: Option<&'a tokio::task::JoinHandle<()>>,
}

/// Lifetime-bound bundle of energy/intensity scraper state used to build
/// the per-tick `CarbonContext`. Borrowed by `flush_evicted`.
pub(super) struct EnergySources<'a> {
    pub(super) base_carbon_ctx: &'a score::carbon::CarbonContext,
    pub(super) scaphandre_state: Option<&'a ScaphandreState>,
    pub(super) scaphandre_staleness_ms: u64,
    pub(super) cloud_state: Option<&'a CloudEnergyState>,
    pub(super) cloud_staleness_ms: u64,
    pub(super) emaps_state: Option<&'a ElectricityMapsState>,
    pub(super) emaps_staleness_ms: u64,
}

/// Borrowed parts of `ProcessTracesCtx` shared across all flush sites.
struct ProcessTracesCtxParts<'a> {
    detect_config: &'a DetectConfig,
    green_enabled: bool,
    metrics: &'a MetricsState,
    confidence: Confidence,
    findings_store: &'a findings_store::FindingsStore,
    correlator: Option<&'a Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
}

/// Drive the daemon's main `tokio::select!` loop: receive events, run the
/// TTL ticker, and handle Ctrl-C. Returns when Ctrl-C is received.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_event_loop(
    rx: &mut mpsc::Receiver<Vec<SpanEvent>>,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
    findings_store: &findings_store::FindingsStore,
    correlator: Option<&Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
    detect_config: &DetectConfig,
    energy_sources: &EnergySources<'_>,
    shutdown: ShutdownTargets<'_>,
    loop_cfg: EventLoopConfig,
) {
    let mut ticker = interval(Duration::from_millis(loop_cfg.evict_ms.max(100)));
    // Prevent burst-catchup if process_traces takes longer than the tick
    // interval. The Scaphandre and cloud scrapers already use Delay.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Cardinality cap on the per-service Prometheus counter prevents OOM
    // from a malicious OTLP sender injecting millions of unique
    // `service.name` values.
    let mut service_meter = ServiceMeter {
        known_services: std::collections::HashSet::new(),
        max_service_cardinality: 1024,
        service_cap_warned: false,
    };
    let parts = || ProcessTracesCtxParts {
        detect_config,
        green_enabled: loop_cfg.green_enabled,
        metrics,
        confidence: loop_cfg.confidence,
        findings_store,
        correlator,
    };

    loop {
        tokio::select! {
            Some(events) = rx.recv() => {
                let lru_evicted = ingest_event_batch(
                    events,
                    loop_cfg.sampling_rate,
                    window,
                    metrics,
                    &mut service_meter,
                ).await;
                flush_evicted(lru_evicted, energy_sources, parts()).await;
            }
            _ = ticker.tick() => {
                let expired = evict_expired_traces(window, metrics).await;
                flush_evicted(expired, energy_sources, parts()).await;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Shutting down daemon, processing remaining traces...");
                shutdown_listeners(shutdown.energy, shutdown.listeners);
                let remaining = {
                    let mut w = window.lock().await;
                    w.drain_all()
                };
                flush_evicted(remaining, energy_sources, parts()).await;
                break;
            }
        }
    }
}

/// Per-service I/O op counter state with a cardinality cap. Prevents OOM
/// from a malicious OTLP sender injecting millions of unique
/// `service.name` values.
struct ServiceMeter {
    known_services: std::collections::HashSet<String>,
    max_service_cardinality: usize,
    service_cap_warned: bool,
}

impl ServiceMeter {
    fn record(&mut self, service: &str, metrics: &MetricsState) {
        if self.known_services.contains(service) {
            metrics
                .service_io_ops_total
                .with_label_values(&[service])
                .inc();
        } else if self.known_services.len() < self.max_service_cardinality {
            self.known_services.insert(service.to_string());
            metrics
                .service_io_ops_total
                .with_label_values(&[service])
                .inc();
        } else if !self.service_cap_warned {
            tracing::warn!(
                cap = self.max_service_cardinality,
                "Service cardinality cap reached; new services will \
                 not have per-service I/O op counters"
            );
            self.service_cap_warned = true;
        }
    }
}

/// Sample, normalize, meter, and push a batch of events into the window.
/// Returns the LRU-evicted traces so the caller can route them through
/// detect+score+store.
async fn ingest_event_batch(
    events: Vec<SpanEvent>,
    sampling_rate: f64,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
    service_meter: &mut ServiceMeter,
) -> Vec<(String, Vec<normalize::NormalizedEvent>)> {
    let events = apply_sampling(events, sampling_rate);
    let event_count = events.len();
    // Normalize OUTSIDE the lock to minimize lock hold time.
    let normalized: Vec<_> = events.into_iter().map(normalize::normalize).collect();
    for event in &normalized {
        service_meter.record(event.event.service.as_str(), metrics);
    }
    let now_ms = current_time_ms();
    let mut lru_evicted = Vec::new();
    {
        // Lock held for O(batch_size) push() calls. Each push is O(1)
        // amortized (LRU insert/promote). Batch size is bounded by the
        // mpsc channel capacity (1024) and max_payload_size.
        let mut w = window.lock().await;
        for event in normalized {
            if let Some(evicted) = w.push(event, now_ms) {
                lru_evicted.push(evicted);
            }
        }
        metrics.active_traces.set(w.active_traces() as f64);
    }
    metrics.events_processed_total.inc_by(event_count as f64);
    lru_evicted
}

/// Pop TTL-expired traces under the lock and refresh the active gauge.
async fn evict_expired_traces(
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
) -> Vec<(String, Vec<normalize::NormalizedEvent>)> {
    let now_ms = current_time_ms();
    let mut w = window.lock().await;
    let expired = w.evict_expired(now_ms);
    metrics.active_traces.set(w.active_traces() as f64);
    expired
}

/// Build a tick `CarbonContext` and route the traces through detect+score.
/// No-op when `traces` is empty.
async fn flush_evicted(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    sources: &EnergySources<'_>,
    parts: ProcessTracesCtxParts<'_>,
) {
    if traces.is_empty() {
        return;
    }
    let tick_ctx = build_tick_ctx(
        sources.base_carbon_ctx,
        sources.scaphandre_state,
        sources.scaphandre_staleness_ms,
        sources.cloud_state,
        sources.cloud_staleness_ms,
        sources.emaps_state,
        sources.emaps_staleness_ms,
    );
    process_traces(
        traces,
        ProcessTracesCtx {
            detect_config: parts.detect_config,
            green_enabled: parts.green_enabled,
            carbon_ctx: &tick_ctx,
            metrics: parts.metrics,
            confidence: parts.confidence,
            findings_store: parts.findings_store,
            correlator: parts.correlator,
        },
    )
    .await;
}

/// Abort all spawned tasks before the daemon returns. Order matters:
/// scrapers first so their log lines don't interleave with the shutdown
/// message, then the listeners.
fn shutdown_listeners(energy: EnergyScraperHandles<'_>, listeners: ListenerHandles<'_>) {
    if let Some(handle) = energy.emaps {
        handle.abort();
    }
    if let Some(handle) = energy.cloud {
        handle.abort();
    }
    if let Some(handle) = energy.scaphandre {
        handle.abort();
    }
    listeners.grpc.abort();
    listeners.http.abort();
    if let Some(handle) = listeners.json_socket {
        handle.abort();
    }
}

/// Build a per-tick `CarbonContext` by optionally patching the base
/// context with a fresh energy snapshot merged from all configured
/// energy sources (Scaphandre RAPL and/or cloud `SPECpower`) plus
/// real-time Electricity Maps intensity.
///
/// Returns `Cow::Borrowed(base)` when no scraper produced fresh data
/// (the common case when all three scrapers are either disabled or
/// still warming up), avoiding the `CarbonContext::clone` on every
/// tick. Materializes an owned clone only when at least one scraper
/// has a reading to inject. `process_traces` takes `&CarbonContext`
/// so the Cow is cheap to use at the call site via `&*ctx`.
///
/// Scaphandre entries take precedence over cloud entries for the same
/// service (direct RAPL measurement beats `SPECpower` interpolation).
fn build_tick_ctx<'a>(
    base: &'a score::carbon::CarbonContext,
    scaphandre_state: Option<&ScaphandreState>,
    scaphandre_staleness_ms: u64,
    cloud_state: Option<&CloudEnergyState>,
    cloud_staleness_ms: u64,
    emaps_state: Option<&ElectricityMapsState>,
    emaps_staleness_ms: u64,
) -> std::borrow::Cow<'a, score::carbon::CarbonContext> {
    let now = score::scaphandre::monotonic_ms();

    // Cloud entries first (lower precedence).
    let cloud_snap = cloud_state
        .map(|s| s.snapshot(now, cloud_staleness_ms))
        .unwrap_or_default();
    // Scaphandre entries override cloud for the same service.
    let scaph_snap = scaphandre_state
        .map(|s| s.snapshot(now, scaphandre_staleness_ms))
        .unwrap_or_default();
    // Electricity Maps real-time intensity (independent of energy snapshot).
    let emaps_snap = emaps_state
        .map(|s| s.snapshot(now, emaps_staleness_ms))
        .unwrap_or_default();

    // Fast path: nothing fresh this tick → no clone, just borrow base.
    if cloud_snap.is_empty() && scaph_snap.is_empty() && emaps_snap.is_empty() {
        return std::borrow::Cow::Borrowed(base);
    }

    // Slow path: materialize a merged snapshot and clone base.
    let mut merged: std::collections::HashMap<String, score::carbon::EnergyEntry> =
        std::collections::HashMap::with_capacity(cloud_snap.len() + scaph_snap.len());
    for (service, energy_kwh) in cloud_snap {
        merged.insert(service, score::carbon::EnergyEntry::cloud(energy_kwh));
    }
    for (service, energy_kwh) in scaph_snap {
        merged.insert(service, score::carbon::EnergyEntry::scaphandre(energy_kwh));
    }

    let mut ctx = base.clone();
    ctx.energy_snapshot = if merged.is_empty() {
        None
    } else {
        Some(merged)
    };
    if !emaps_snap.is_empty() {
        ctx.real_time_intensity = Some(emaps_snap);
    }

    std::borrow::Cow::Owned(ctx)
}

/// Record slow span durations into a Prometheus histogram.
///
/// `histogram_quantile()` can then compute accurate global percentiles
/// across sharded daemon instances. Handles resolved once before the loop
/// to avoid per-span `HashMap` lookups in `with_label_values`.
fn record_slow_durations(traces: &[Trace], detect_config: &DetectConfig, metrics: &MetricsState) {
    let slow_threshold_us = detect_config.slow_threshold_ms.saturating_mul(1000);
    let hist_sql = metrics.slow_duration_seconds.with_label_values(&["sql"]);
    let hist_http = metrics
        .slow_duration_seconds
        .with_label_values(&["http_out"]);
    for trace in traces {
        for span in &trace.spans {
            if span.event.duration_us > slow_threshold_us {
                let hist = match span.event.event_type {
                    crate::event::EventType::Sql => &hist_sql,
                    crate::event::EventType::HttpOut => &hist_http,
                };
                hist.observe(span.event.duration_us as f64 / 1_000_000.0);
            }
        }
    }
}

/// Update Prometheus counters, gauges, and exemplars, then emit findings
/// as NDJSON to stdout.
fn emit_findings_and_update_metrics(
    trace_count: usize,
    findings: &[detect::Finding],
    green_summary: &GreenSummary,
    metrics: &MetricsState,
) {
    use std::io::Write;

    metrics.traces_analyzed_total.inc_by(trace_count as f64);
    metrics
        .total_io_ops
        .inc_by(green_summary.total_io_ops as f64);
    metrics
        .avoidable_io_ops
        .inc_by(green_summary.avoidable_io_ops as f64);
    let cumulative_total = metrics.total_io_ops.get();
    if cumulative_total > 0.0 {
        metrics
            .io_waste_ratio
            .set(metrics.avoidable_io_ops.get() / cumulative_total);
    }
    metrics.record_exemplars(findings, green_summary);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for finding in findings {
        metrics
            .findings_total
            .with_label_values(&[finding.finding_type.as_str(), finding.severity.as_str()])
            .inc();
        if serde_json::to_writer(&mut lock, finding).is_ok() {
            let _ = writeln!(lock);
        }
    }
}

/// Process a batch of completed/expired traces: detect, score, emit NDJSON.
///
/// Shared context passed to [`process_traces`] on every tick.
///
/// Groups the configuration, state, and downstream sinks so the function
/// signature stays readable. All fields are borrowed for the duration of
/// the call, no ownership transfer.
struct ProcessTracesCtx<'a> {
    detect_config: &'a DetectConfig,
    green_enabled: bool,
    carbon_ctx: &'a score::carbon::CarbonContext,
    metrics: &'a MetricsState,
    confidence: Confidence,
    findings_store: &'a findings_store::FindingsStore,
    correlator: Option<&'a Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
}

/// stamps `confidence` on every finding after detection. The
/// value is derived from `config.daemon_environment` in `run()` and passed
/// here unchanged. `analyze` batch mode does not call this function; it
/// uses `pipeline::analyze_with_traces` which hardcodes
/// `Confidence::CiBatch`.
async fn process_traces(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    ctx: ProcessTracesCtx<'_>,
) {
    if traces.is_empty() {
        return;
    }

    let trace_count = traces.len();
    let trace_structs: Vec<Trace> = traces
        .into_iter()
        .map(|(trace_id, spans)| Trace { trace_id, spans })
        .collect();

    let findings = detect::run_full_detection(&trace_structs, ctx.detect_config);

    record_slow_durations(&trace_structs, ctx.detect_config, ctx.metrics);

    // The daemon path discards `per_endpoint_io_ops` (third tuple
    // element): it is consumed by the batch `diff` subcommand, not by
    // the daemon's NDJSON / metrics surface. Bind it to `_` so the
    // hot-path span iteration in `score_green` is still a single pass.
    let (mut findings, green_summary, _per_endpoint_io_ops) = if ctx.green_enabled {
        score::score_green(&trace_structs, findings, Some(ctx.carbon_ctx))
    } else {
        let total_io_ops = trace_structs.iter().map(|t| t.spans.len()).sum();
        (findings, GreenSummary::disabled(total_io_ops), Vec::new())
    };

    // Stamp the daemon's confidence label. Same shared helper as
    // `pipeline::analyze`, so the two paths cannot drift on the loop.
    detect::apply_confidence(&mut findings, ctx.confidence);
    let findings = findings;

    let now_ms = current_time_ms();
    if !findings.is_empty() {
        ctx.findings_store.push_batch(&findings, now_ms).await;
    }

    if let Some(correlator) = ctx.correlator {
        correlator.lock().await.ingest(&findings, now_ms);
    }

    emit_findings_and_update_metrics(trace_count, &findings, &green_summary, ctx.metrics);
}

/// Get current time in milliseconds since epoch.
///
/// Returns 0 and logs a warning if the system clock is set before the
/// Unix epoch (effectively a configuration error). Downstream code treats
/// the timestamp as a monotonic-ish sort key; a single zero tick produces
/// visible bucketing but no correctness issue.
fn current_time_ms() -> u64 {
    if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    } else {
        tracing::warn!(
            "System clock is before Unix epoch; using 0 as current_time_ms. \
             Check system time configuration."
        );
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::window::WindowConfig;
    use crate::event::{EventSource, EventType, SpanEvent};

    fn make_normalized(trace_id: &str, target: &str) -> normalize::NormalizedEvent {
        normalize::normalize(SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        })
    }

    fn default_detect_config() -> DetectConfig {
        DetectConfig {
            n_plus_one_threshold: 5,
            window_ms: 500,
            slow_threshold_ms: 500,
            slow_min_occurrences: 3,
            max_fanout: 20,
            chatty_service_min_calls: 15,
            pool_saturation_concurrent_threshold: 10,
            serialized_min_sequential: 3,
        }
    }

    fn empty_carbon_ctx() -> score::carbon::CarbonContext {
        score::carbon::CarbonContext::default()
    }

    /// Build a `ProcessTracesCtx` for tests with sensible defaults.
    fn test_ctx<'a>(
        detect_config: &'a DetectConfig,
        carbon_ctx: &'a score::carbon::CarbonContext,
        metrics: &'a MetricsState,
        findings_store: &'a findings_store::FindingsStore,
        green_enabled: bool,
    ) -> ProcessTracesCtx<'a> {
        ProcessTracesCtx {
            detect_config,
            green_enabled,
            carbon_ctx,
            metrics,
            confidence: Confidence::DaemonStaging,
            findings_store,
            correlator: None,
        }
    }

    #[tokio::test]
    async fn process_traces_empty_does_nothing() {
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;
    }

    #[tokio::test]
    async fn process_traces_with_n_plus_one() {
        // 6 events with different params -> N+1 finding
        let events: Vec<_> = (1..=6)
            .map(|i| {
                make_normalized(
                    "t1",
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                )
            })
            .collect();
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;
    }

    #[tokio::test]
    async fn process_traces_clean_no_finding() {
        // 2 events with different templates -> no finding
        let events = vec![
            make_normalized("t1", "SELECT * FROM users WHERE id = 1"),
            make_normalized("t1", "SELECT * FROM orders WHERE id = 2"),
        ];
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;
    }

    #[test]
    fn current_time_ms_returns_nonzero() {
        let ms = current_time_ms();
        assert!(ms > 0, "current_time_ms should return a positive value");
    }

    #[test]
    fn evict_expired_returns_traces() {
        let config = WindowConfig {
            trace_ttl_ms: 100,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);

        let event = normalize::normalize(SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: "t1".to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        });

        w.push(event, 0);
        assert_eq!(w.active_traces(), 1);

        // Not yet expired
        let expired = w.evict_expired(50);
        assert!(expired.is_empty());
        assert_eq!(w.active_traces(), 1);

        // Now expired (150 - 0 = 150 > 100)
        let expired = w.evict_expired(150);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "t1");
        assert_eq!(expired[0].1.len(), 1);
        assert_eq!(w.active_traces(), 0);
    }

    #[tokio::test]
    async fn process_traces_updates_metrics() {
        let events: Vec<_> = (1..=6)
            .map(|i| {
                make_normalized(
                    "t1",
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                )
            })
            .collect();
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;

        let output = metrics.render();
        assert!(output.contains("perf_sentinel_traces_analyzed_total"));
        assert!(output.contains("perf_sentinel_findings_total"));
    }

    #[tokio::test]
    async fn process_traces_green_disabled() {
        let events: Vec<_> = (1..=6)
            .map(|i| {
                make_normalized(
                    "t1",
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                )
            })
            .collect();
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, false),
        )
        .await;
        // avoidable_io_ops counter should stay at 0 when green is disabled
        assert!((metrics.avoidable_io_ops.get() - 0.0).abs() < f64::EPSILON);
        // but total_io_ops should still be counted
        assert!(metrics.total_io_ops.get() > 0.0);
    }

    #[test]
    fn build_tick_ctx_no_scrapers_yields_borrowed_cow() {
        // Fast path: no scrapers → Cow::Borrowed, no clone.
        let base = score::carbon::CarbonContext::default();
        let ctx = build_tick_ctx(&base, None, 0, None, 0, None, 0);
        assert!(matches!(ctx, std::borrow::Cow::Borrowed(_)));
        assert!(ctx.energy_snapshot.is_none());
    }

    #[test]
    fn build_tick_ctx_scaphandre_only() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let ctx = build_tick_ctx(&base, Some(&scaph), 500, None, 0, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
    }

    #[test]
    fn build_tick_ctx_cloud_only() {
        let base = score::carbon::CarbonContext::default();
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-b".into(), 2e-7, 100);
        let ctx = build_tick_ctx(&base, None, 0, Some(&cloud), 500, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-b"].model_tag, "cloud_specpower");
    }

    #[test]
    fn build_tick_ctx_scaphandre_overrides_cloud_for_same_service() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-a".into(), 5e-7, 100);
        cloud.insert_for_test("svc-b".into(), 3e-7, 100);
        let ctx = build_tick_ctx(&base, Some(&scaph), 500, Some(&cloud), 500, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 2);
        // svc-a: Scaphandre wins (1e-7, not 5e-7)
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
        assert!((snap["svc-a"].energy_per_op_kwh - 1e-7).abs() < 1e-15);
        // svc-b: cloud only
        assert_eq!(snap["svc-b"].model_tag, "cloud_specpower");
    }

    #[test]
    fn build_tick_ctx_stale_entries_filtered() {
        // Test staleness via the state's snapshot() method directly.
        // An entry at time 0 with a staleness of 1ms should be stale
        // when queried at time 100.
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("stale-svc".into(), 1e-7, 0);
        let snap = scaph.snapshot(100, 1);
        assert!(
            snap.is_empty(),
            "entry at time 0 should be stale when now=100, staleness=1"
        );
        // A fresh entry should appear.
        scaph.insert_for_test("fresh-svc".into(), 2e-7, 99);
        let snap2 = scaph.snapshot(100, 50);
        assert!(snap2.contains_key("fresh-svc"));
        assert!(!snap2.contains_key("stale-svc"));
    }
}
