//! Daemon main event loop: ingest batches, evict expired traces, and route
//! the resulting traces through detect + score + metrics + findings store.

use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::{Duration, interval};

use crate::correlate::Trace;
use crate::correlate::window::TraceWindow;
use crate::detect;
#[cfg(test)]
use crate::detect::sanitizer_aware::SanitizerAwareMode;
use crate::detect::{Confidence, DetectConfig};
use crate::event::SpanEvent;
use crate::normalize;
use crate::report::GreenSummary;
use crate::report::metrics::MetricsState;
use crate::score;
use crate::score::cloud_energy::CloudEnergyState;
use crate::score::electricity_maps::ElectricityMapsState;
use crate::score::kepler::KeplerState;
use crate::score::redfish::RedfishState;
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
    /// Capacity of the bounded analysis worker queue. From
    /// `[daemon] analysis_queue_capacity`.
    pub(super) analysis_queue_capacity: usize,
}

/// Bundle of handles aborted on shutdown (SIGINT, or SIGTERM on Unix).
pub(super) struct ShutdownTargets<'a> {
    pub(super) energy: EnergyScraperHandles<'a>,
    pub(super) listeners: ListenerHandles<'a>,
}

/// `JoinHandle`s for the optional energy / intensity scrapers.
#[derive(Clone, Copy)]
pub(super) struct EnergyScraperHandles<'a> {
    pub(super) scaphandre: Option<&'a tokio::task::JoinHandle<()>>,
    pub(super) kepler: Option<&'a tokio::task::JoinHandle<()>>,
    pub(super) redfish: Option<&'a tokio::task::JoinHandle<()>>,
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
/// the per-tick `CarbonContext`. Borrowed by `enqueue_for_analysis`.
pub(super) struct EnergySources<'a> {
    pub(super) base_carbon_ctx: Arc<score::carbon::CarbonContext>,
    pub(super) scaphandre_state: Option<&'a ScaphandreState>,
    pub(super) scaphandre_staleness_ms: u64,
    pub(super) kepler_state: Option<&'a KeplerState>,
    pub(super) kepler_staleness_ms: u64,
    pub(super) redfish_state: Option<&'a RedfishState>,
    pub(super) redfish_staleness_ms: u64,
    pub(super) cloud_state: Option<&'a CloudEnergyState>,
    pub(super) cloud_staleness_ms: u64,
    pub(super) emaps_state: Option<&'a ElectricityMapsState>,
    pub(super) emaps_staleness_ms: u64,
}

/// One evicted/expired/drained batch handed to the analysis worker. The
/// `CarbonContext` is built on the loop side at eviction time, so energy
/// scraper readings keep their current sampling instant.
struct AnalysisBatch {
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    carbon_ctx: Arc<score::carbon::CarbonContext>,
}

impl AnalysisBatch {
    /// Build a batch from evicted/expired/drained traces, sampling the
    /// energy sources at eviction time so the snapshot travels with the
    /// batch. Single construction site shared by both the non-blocking
    /// enqueue and the shutdown drain.
    fn new(
        traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
        sources: &EnergySources<'_>,
    ) -> Self {
        Self {
            traces,
            carbon_ctx: build_owned_tick_ctx(sources),
        }
    }
}

/// Owned/`Arc` state the analysis worker needs. Everything crossing the
/// task boundary is owned or shared via `Arc` so the spawned worker is
/// `'static`. Mirrors the borrowed fields of [`ProcessTracesCtx`].
struct AnalysisWorkerCtx {
    detect_config: DetectConfig,
    green_enabled: bool,
    confidence: Confidence,
    metrics: Arc<MetricsState>,
    findings_store: Arc<findings_store::FindingsStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    green_summary_cell: Arc<RwLock<GreenSummary>>,
    archive_tx: Option<mpsc::Sender<super::archive::OwnedArchive>>,
}

/// Drive the daemon's main `tokio::select!` loop: receive events, run the
/// TTL ticker, and handle shutdown signals.
///
/// # Errors
///
/// Returns [`super::DaemonError::AnalysisWorkerStopped`] if the analysis
/// worker dies (e.g. a detector panics) while the daemon is running, so a
/// supervisor restarts the process instead of leaving it up while it
/// silently analyzes nothing. Returns `Ok(())` on a graceful shutdown
/// (SIGINT, or SIGTERM on Unix) after draining the in-flight window.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_event_loop(
    rx: &mut mpsc::Receiver<Vec<SpanEvent>>,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: Arc<MetricsState>,
    findings_store: Arc<findings_store::FindingsStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    detect_config: &DetectConfig,
    energy_sources: &EnergySources<'_>,
    shutdown: ShutdownTargets<'_>,
    loop_cfg: EventLoopConfig,
    green_summary_cell: Arc<RwLock<GreenSummary>>,
    archive_tx: Option<mpsc::Sender<super::archive::OwnedArchive>>,
) -> Result<(), super::DaemonError> {
    // detect+score run on this single worker, off the select! loop, so a
    // long analysis pass can no longer stall ingestion (rx) or TTL
    // eviction (ticker). One channel, one worker, FIFO: the stateful
    // cross-trace correlator still sees a deterministic batch sequence.
    let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(loop_cfg.analysis_queue_capacity);
    let worker = tokio::spawn(run_analysis_worker(
        work_rx,
        AnalysisWorkerCtx {
            detect_config: detect_config.clone(),
            green_enabled: loop_cfg.green_enabled,
            confidence: loop_cfg.confidence,
            metrics: metrics.clone(),
            findings_store,
            correlator,
            green_summary_cell,
            archive_tx,
        },
    ));

    // The shutdown future and the spawned worker are injected into
    // `drive_event_loop` so tests can drive the loop with a controllable
    // shutdown trigger and a worker that stops on demand (graceful-drain and
    // fail-loud paths). Production wires the real SIGINT/SIGTERM signal.
    drive_event_loop(
        rx,
        window,
        &metrics,
        energy_sources,
        shutdown,
        loop_cfg,
        work_tx,
        worker,
        crate::shutdown::shutdown_signal(),
    )
    .await
}

/// Inner select! loop, split out from [`run_event_loop`] so the worker
/// handle and shutdown future are parameters (testable). Returns
/// [`super::DaemonError::AnalysisWorkerStopped`] if `worker` stops before
/// `shutdown_fut` fires; otherwise drains the window into the worker and
/// returns `Ok(())`.
#[allow(clippy::too_many_arguments)]
async fn drive_event_loop(
    rx: &mut mpsc::Receiver<Vec<SpanEvent>>,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
    energy_sources: &EnergySources<'_>,
    shutdown: ShutdownTargets<'_>,
    loop_cfg: EventLoopConfig,
    work_tx: mpsc::Sender<AnalysisBatch>,
    mut worker: tokio::task::JoinHandle<()>,
    shutdown_fut: impl Future<Output = ()>,
) -> Result<(), super::DaemonError> {
    let mut ticker = interval(Duration::from_millis(loop_cfg.evict_ms.max(100)));
    // Prevent burst-catchup if a tick is delayed. With analysis off the
    // loop, the loop rarely lags, but the scrapers already use Delay.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Cardinality cap on the per-service Prometheus counter prevents OOM
    // from a malicious OTLP sender injecting millions of unique
    // `service.name` values.
    let mut service_meter = ServiceMeter {
        known_services: std::collections::HashSet::new(),
        max_service_cardinality: 1024,
        service_cap_warned: false,
    };

    // Pin the shutdown future once so the SIGTERM/SIGINT listeners are
    // registered a single time rather than re-registered on every loop
    // iteration. Same idiom as the Tempo fetch drain in `ingest::tempo`.
    tokio::pin!(shutdown_fut);

    let graceful = loop {
        tokio::select! {
            Some(events) = rx.recv() => {
                let lru_evicted = ingest_event_batch(
                    events,
                    loop_cfg.sampling_rate,
                    window,
                    metrics,
                    &mut service_meter,
                ).await;
                enqueue_for_analysis(lru_evicted, energy_sources, &work_tx, metrics);
            }
            _ = ticker.tick() => {
                let expired = evict_expired_traces(window, metrics).await;
                enqueue_for_analysis(expired, energy_sources, &work_tx, metrics);
            }
            () = &mut shutdown_fut => {
                tracing::info!("Shutting down daemon, processing remaining traces...");
                break true;
            }
            res = &mut worker => {
                // The single analysis worker finished before shutdown, so it
                // panicked or aborted. Fail loud: exit instead of running on
                // while silently analyzing nothing, so a supervisor restarts
                // the process (the inline-detection design crashed the daemon
                // on the same fault).
                tracing::error!(result = ?res, "analysis worker stopped unexpectedly; daemon exiting for restart");
                break false;
            }
        }
    };

    shutdown_listeners(shutdown.energy, shutdown.listeners);
    if !graceful {
        return Err(super::DaemonError::AnalysisWorkerStopped);
    }
    drain_to_worker_and_join(window, energy_sources, work_tx, worker, metrics).await;
    Ok(())
}

/// Single analysis worker: pulls batches in FIFO order and runs the
/// CPU-heavy detect+score path that used to run inline on the select!
/// loop. Exits when the channel closes (shutdown), after draining every
/// buffered batch.
async fn run_analysis_worker(mut work_rx: mpsc::Receiver<AnalysisBatch>, wctx: AnalysisWorkerCtx) {
    while let Some(batch) = work_rx.recv().await {
        wctx.metrics.analysis_queue_depth.dec();
        process_traces(
            batch.traces,
            ProcessTracesCtx {
                detect_config: &wctx.detect_config,
                green_enabled: wctx.green_enabled,
                carbon_ctx: batch.carbon_ctx.as_ref(),
                metrics: &wctx.metrics,
                confidence: wctx.confidence,
                findings_store: &wctx.findings_store,
                correlator: wctx.correlator.as_deref(),
                green_summary_cell: &wctx.green_summary_cell,
                archive_tx: wctx.archive_tx.as_ref(),
            },
        )
        .await;
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
        } else {
            // Keep the ongoing drop visible: the warn fires once, the
            // overflow counter moves on every unattributed op.
            metrics.service_io_ops_overflow_total.inc();
            if !self.service_cap_warned {
                tracing::warn!(
                    cap = self.max_service_cardinality,
                    "Service cardinality cap reached; new services will \
                     not have per-service I/O op counters"
                );
                self.service_cap_warned = true;
            }
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
        service_meter.record(event.event.service.as_ref(), metrics);
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

/// Build the per-tick `CarbonContext` from the current scraper snapshots,
/// owned so it can travel to the worker. Sampling the energy sources here
/// (on the loop side, at eviction time) preserves the previous timing.
fn build_owned_tick_ctx(sources: &EnergySources<'_>) -> Arc<score::carbon::CarbonContext> {
    match build_tick_ctx(
        &sources.base_carbon_ctx,
        sources.scaphandre_state,
        sources.scaphandre_staleness_ms,
        sources.kepler_state,
        sources.kepler_staleness_ms,
        sources.redfish_state,
        sources.redfish_staleness_ms,
        sources.cloud_state,
        sources.cloud_staleness_ms,
        sources.emaps_state,
        sources.emaps_staleness_ms,
    ) {
        // Fast path (no scraper produced fresh data, the common case):
        // share the base context by refcount instead of deep-cloning the
        // region map and calibration table on every evicted batch.
        std::borrow::Cow::Borrowed(_) => Arc::clone(&sources.base_carbon_ctx),
        std::borrow::Cow::Owned(ctx) => Arc::new(ctx),
    }
}

/// Hand an evicted/expired batch to the analysis worker without blocking.
/// Synchronous and `try_reserve`-based on purpose: the select! loop never
/// awaits analysis, so ingestion and eviction stay live. When the queue is
/// full (or the worker has stopped) the whole batch is shed and counted
/// (batches + traces) instead of being silently dropped. The owned
/// `CarbonContext` is built only once a slot is reserved, so a shed never
/// pays for a discarded clone. No-op when `traces` is empty.
fn enqueue_for_analysis(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    sources: &EnergySources<'_>,
    work_tx: &mpsc::Sender<AnalysisBatch>,
    metrics: &MetricsState,
) {
    if traces.is_empty() {
        return;
    }
    let trace_count = traces.len();
    match work_tx.try_reserve() {
        Ok(permit) => {
            metrics.analysis_queue_depth.inc();
            permit.send(AnalysisBatch::new(traces, sources));
        }
        Err(mpsc::error::TrySendError::Full(())) => {
            metrics.record_shed(trace_count);
            tracing::warn!(traces = trace_count, "analysis queue full, shedding batch");
        }
        Err(mpsc::error::TrySendError::Closed(())) => {
            metrics.record_shed(trace_count);
            tracing::error!(
                traces = trace_count,
                "analysis worker stopped, shedding batch"
            );
        }
    }
}

/// Shutdown handshake: drain the in-flight window into the worker with a
/// blocking `send` (guaranteed delivery, no shedding), close the channel,
/// then join the worker so every buffered and in-flight batch is fully
/// processed before `run_event_loop` returns.
async fn drain_to_worker_and_join(
    window: &Arc<Mutex<TraceWindow>>,
    sources: &EnergySources<'_>,
    work_tx: mpsc::Sender<AnalysisBatch>,
    worker: tokio::task::JoinHandle<()>,
    metrics: &MetricsState,
) {
    let remaining = {
        let mut w = window.lock().await;
        w.drain_all()
    };
    if !remaining.is_empty() {
        let trace_count = remaining.len();
        // Blocking send: a live worker keeps draining, so capacity frees up
        // and the final window is delivered rather than shed.
        let batch = AnalysisBatch::new(remaining, sources);
        if work_tx.send(batch).await.is_ok() {
            metrics.analysis_queue_depth.inc();
        } else {
            // The worker stopped before the drain (e.g. it panicked): the
            // window cannot be delivered, so count it instead of losing it
            // silently.
            metrics.record_shed(trace_count);
            tracing::error!(
                traces = trace_count,
                "analysis worker stopped before shutdown drain"
            );
        }
    }
    drop(work_tx);
    let _ = worker.await;
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
    if let Some(handle) = energy.redfish {
        handle.abort();
    }
    if let Some(handle) = energy.kepler {
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
/// Precedence (highest to lowest): Scaphandre RAPL, Kepler eBPF,
/// Redfish BMC, cloud `SPECpower`. Inserted in reverse order so the
/// highest-fidelity entry wins for any service that appears in
/// multiple snapshots.
#[allow(clippy::too_many_arguments)]
fn build_tick_ctx<'a>(
    base: &'a score::carbon::CarbonContext,
    scaphandre_state: Option<&ScaphandreState>,
    scaphandre_staleness_ms: u64,
    kepler_state: Option<&KeplerState>,
    kepler_staleness_ms: u64,
    redfish_state: Option<&RedfishState>,
    redfish_staleness_ms: u64,
    cloud_state: Option<&CloudEnergyState>,
    cloud_staleness_ms: u64,
    emaps_state: Option<&ElectricityMapsState>,
    emaps_staleness_ms: u64,
) -> std::borrow::Cow<'a, score::carbon::CarbonContext> {
    let now = score::scaphandre::monotonic_ms();

    // Cloud entries first (lowest precedence).
    let cloud_snap = cloud_state
        .map(|s| s.snapshot(now, cloud_staleness_ms))
        .unwrap_or_default();
    // Redfish entries override cloud for the same service.
    let redfish_snap = redfish_state
        .map(|s| s.snapshot(now, redfish_staleness_ms))
        .unwrap_or_default();
    // Kepler entries override Redfish and cloud for the same service.
    let kepler_snap = kepler_state
        .map(|s| s.snapshot(now, kepler_staleness_ms))
        .unwrap_or_default();
    // Scaphandre entries override every lower-tier source.
    let scaph_snap = scaphandre_state
        .map(|s| s.snapshot(now, scaphandre_staleness_ms))
        .unwrap_or_default();
    // Electricity Maps real-time intensity (independent of energy snapshot).
    let emaps_snap = emaps_state
        .map(|s| s.snapshot_with_metadata(now, emaps_staleness_ms))
        .unwrap_or_default();

    // Fast path: nothing fresh this tick → no clone, just borrow base.
    if cloud_snap.is_empty()
        && redfish_snap.is_empty()
        && kepler_snap.is_empty()
        && scaph_snap.is_empty()
        && emaps_snap.is_empty()
    {
        return std::borrow::Cow::Borrowed(base);
    }

    // Slow path: materialize a merged snapshot and clone base.
    let mut merged: std::collections::HashMap<String, score::carbon::EnergyEntry> =
        std::collections::HashMap::with_capacity(
            cloud_snap.len() + redfish_snap.len() + kepler_snap.len() + scaph_snap.len(),
        );
    for (service, energy_kwh) in cloud_snap {
        merged.insert(service, score::carbon::EnergyEntry::cloud(energy_kwh));
    }
    for (service, energy_kwh) in redfish_snap {
        merged.insert(service, score::carbon::EnergyEntry::redfish(energy_kwh));
    }
    for (service, energy_kwh) in kepler_snap {
        merged.insert(service, score::carbon::EnergyEntry::kepler(energy_kwh));
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
    green_summary_cell: &'a Arc<RwLock<GreenSummary>>,
    archive_tx: Option<&'a mpsc::Sender<super::archive::OwnedArchive>>,
}

/// stamps `confidence` on every finding after detection. The
/// value is derived from `config.daemon.environment` in `run()` and passed
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

    // Keep `per_endpoint_io_ops` for the periodic-disclosure archive
    // (design doc 08). Already computed by `score_green`'s single pass.
    let (mut findings, green_summary, per_endpoint_io_ops) = if ctx.green_enabled {
        score::score_green(&trace_structs, findings, Some(ctx.carbon_ctx))
    } else {
        let total_io_ops = trace_structs.iter().map(|t| t.spans.len()).sum();
        (findings, GreenSummary::disabled(total_io_ops), Vec::new())
    };

    // Publish the per-batch summary on the shared cell so live daemon
    // snapshots served by `/api/export/report` carry the latest CO2
    // picture. `scoring_config` is also propagated here via
    // `score_green` (it travels through `CarbonContext`), but the
    // handler unconditionally re-applies it from `state.scoring_config`
    // so the audit-trail metadata cannot drift from the startup config.
    ctx.green_summary_cell
        .write()
        .await
        .clone_from(&green_summary);

    // Stamp the daemon's confidence label. Same shared helper as
    // `pipeline::analyze`, so the two paths cannot drift on the loop.
    detect::apply_confidence(&mut findings, ctx.confidence);
    // Stamp the canonical signature so a daemon snapshot piped into
    // `report --input` carries usable signatures for ack matching.
    crate::acknowledgments::enrich_with_signatures(&mut findings);
    let findings = findings;

    let now_ms = current_time_ms();
    if !findings.is_empty() {
        ctx.findings_store.push_batch(&findings, now_ms).await;
    }

    if let Some(correlator) = ctx.correlator {
        correlator.lock().await.ingest(&findings, now_ms);
    }

    emit_findings_and_update_metrics(trace_count, &findings, &green_summary, ctx.metrics);

    if let Some(archive_tx) = ctx.archive_tx {
        let events_processed = trace_structs.iter().map(|t| t.spans.len()).sum();
        // Operator + canonical avoidable tiers, archived side by side.
        // Skipped when green scoring produced no carbon: the tiers would
        // carry avoidable ops with zero energy/carbon, and the extra
        // canonical detection pass would be wasted. Computed before the
        // summary is moved into the report.
        let disclosure_waste = green_summary.co2.is_some().then(|| {
            score::canonical::compute_disclosure_waste(
                &trace_structs,
                &green_summary,
                ctx.detect_config,
            )
        });
        let report = crate::report::Report {
            analysis: crate::report::Analysis {
                duration_ms: 0,
                events_processed,
                traces_analyzed: trace_count,
            },
            // Move owned data into the archive; aggregator consumes
            // findings, green_summary, and per_endpoint_io_ops. Other
            // fields are placeholders, see design doc 08.
            findings,
            green_summary,
            quality_gate: crate::report::QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops,
            correlations: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            disclosure_waste,
        };
        let archive = super::archive::OwnedArchive {
            ts: chrono::Utc::now(),
            report,
        };
        super::archive::try_send(archive_tx, archive);
    }
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
    use std::sync::Arc;

    use super::*;
    use crate::correlate::window::WindowConfig;
    use crate::event::{EventSource, EventType, SpanEvent};
    use core::assert_matches;

    fn make_normalized(trace_id: &str, target: &str) -> normalize::NormalizedEvent {
        normalize::normalize(SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: Arc::from("test"),
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
            sanitizer_aware_classification: SanitizerAwareMode::default(),
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
        green_summary_cell: &'a Arc<RwLock<GreenSummary>>,
    ) -> ProcessTracesCtx<'a> {
        ProcessTracesCtx {
            detect_config,
            green_enabled,
            carbon_ctx,
            metrics,
            confidence: Confidence::DaemonStaging,
            findings_store,
            correlator: None,
            green_summary_cell,
            archive_tx: None,
        }
    }

    fn fresh_green_cell() -> Arc<RwLock<GreenSummary>> {
        Arc::new(RwLock::new(GreenSummary::disabled(0)))
    }

    #[tokio::test]
    async fn process_traces_empty_does_nothing() {
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        let cell = fresh_green_cell();
        process_traces(
            vec![],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
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
        let cell = fresh_green_cell();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
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
        let cell = fresh_green_cell();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
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
            service: Arc::from("test"),
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
        let cell = fresh_green_cell();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
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
        let cell = fresh_green_cell();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, false, &cell),
        )
        .await;
        // avoidable_io_ops counter should stay at 0 when green is disabled
        assert!((metrics.avoidable_io_ops.get() - 0.0).abs() < f64::EPSILON);
        // but total_io_ops should still be counted
        assert!(metrics.total_io_ops.get() > 0.0);
    }

    #[tokio::test]
    async fn process_traces_publishes_green_summary_to_cell() {
        // Asserts the contract behind /api/export/report: each batch
        // overwrites the shared cell so live snapshots pick up the
        // latest CO2 picture.
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
        let cell = fresh_green_cell();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
        )
        .await;
        let snapshot = cell.read().await.clone();
        assert!(snapshot.total_io_ops > 0, "cell should reflect the batch");
    }

    #[test]
    fn build_tick_ctx_no_scrapers_yields_borrowed_cow() {
        // Fast path: no scrapers → Cow::Borrowed, no clone.
        let base = score::carbon::CarbonContext::default();
        let ctx = build_tick_ctx(&base, None, 0, None, 0, None, 0, None, 0, None, 0);
        assert_matches!(ctx, std::borrow::Cow::Borrowed(_));
        assert!(ctx.energy_snapshot.is_none());
    }

    #[test]
    fn build_tick_ctx_scaphandre_only() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let ctx = build_tick_ctx(&base, Some(&scaph), 500, None, 0, None, 0, None, 0, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
    }

    #[test]
    fn build_tick_ctx_cloud_only() {
        let base = score::carbon::CarbonContext::default();
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-b".into(), 2e-7, 100);
        let ctx = build_tick_ctx(&base, None, 0, None, 0, None, 0, Some(&cloud), 500, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-b"].model_tag, "cloud_specpower");
    }

    #[test]
    fn build_tick_ctx_kepler_only() {
        let base = score::carbon::CarbonContext::default();
        let kepler = KeplerState::new();
        kepler.insert_for_test("svc-k".into(), 4e-7, 100);
        let ctx = build_tick_ctx(
            &base,
            None,
            0,
            Some(&kepler),
            500,
            None,
            0,
            None,
            0,
            None,
            0,
        );
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-k"].model_tag, "kepler_ebpf");
    }

    #[test]
    fn build_tick_ctx_redfish_only() {
        let base = score::carbon::CarbonContext::default();
        let redfish = RedfishState::new();
        redfish.insert_for_test("svc-r".into(), 6e-7, 100);
        let ctx = build_tick_ctx(
            &base,
            None,
            0,
            None,
            0,
            Some(&redfish),
            500,
            None,
            0,
            None,
            0,
        );
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-r"].model_tag, "redfish_bmc");
    }

    #[test]
    fn build_tick_ctx_scaphandre_overrides_kepler_overrides_cloud_for_same_service() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let kepler = KeplerState::new();
        kepler.insert_for_test("svc-a".into(), 2e-7, 100);
        kepler.insert_for_test("svc-k".into(), 4e-7, 100);
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-a".into(), 5e-7, 100);
        cloud.insert_for_test("svc-b".into(), 3e-7, 100);
        let ctx = build_tick_ctx(
            &base,
            Some(&scaph),
            500,
            Some(&kepler),
            500,
            None,
            0,
            Some(&cloud),
            500,
            None,
            0,
        );
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 3);
        // svc-a: Scaphandre wins (top of precedence).
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
        assert!((snap["svc-a"].energy_per_op_kwh - 1e-7).abs() < 1e-15);
        // svc-k: Kepler-only entry survives.
        assert_eq!(snap["svc-k"].model_tag, "kepler_ebpf");
        // svc-b: cloud only.
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

    /// `EnergySources` with no scrapers configured.
    fn no_scrapers(base: &Arc<score::carbon::CarbonContext>) -> EnergySources<'_> {
        EnergySources {
            base_carbon_ctx: base.clone(),
            scaphandre_state: None,
            scaphandre_staleness_ms: 0,
            kepler_state: None,
            kepler_staleness_ms: 0,
            redfish_state: None,
            redfish_staleness_ms: 0,
            cloud_state: None,
            cloud_staleness_ms: 0,
            emaps_state: None,
            emaps_staleness_ms: 0,
        }
    }

    fn one_trace_batch(id: &str) -> Vec<(String, Vec<normalize::NormalizedEvent>)> {
        vec![(id.to_string(), vec![make_normalized(id, "SELECT 1")])]
    }

    fn test_window() -> Arc<Mutex<TraceWindow>> {
        Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_events_per_trace: 1000,
            trace_ttl_ms: 30_000,
            max_active_traces: std::num::NonZeroUsize::new(10_000).expect("nonzero"),
        })))
    }

    fn test_worker_ctx(
        metrics: &Arc<MetricsState>,
        findings_store: &Arc<findings_store::FindingsStore>,
        green_summary_cell: &Arc<RwLock<GreenSummary>>,
    ) -> AnalysisWorkerCtx {
        AnalysisWorkerCtx {
            detect_config: default_detect_config(),
            green_enabled: true,
            confidence: Confidence::DaemonStaging,
            metrics: metrics.clone(),
            findings_store: findings_store.clone(),
            correlator: None,
            green_summary_cell: green_summary_cell.clone(),
            archive_tx: None,
        }
    }

    #[tokio::test]
    async fn ingestion_not_head_of_line_blocked_by_slow_analysis() {
        // The worker is "infinitely slow": we keep the receiver but never
        // poll it, so the queue cannot drain. The select! loop only ever
        // touches analysis through `enqueue_for_analysis`, which is
        // synchronous + `try_reserve`, so it can never block on a stuck
        // worker. The loop therefore keeps draining rx and the ticker.
        // Excess batches are shed and counted, never silently dropped.
        let metrics = MetricsState::new();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);
        let (work_tx, _work_rx) = mpsc::channel::<AnalysisBatch>(2);

        for i in 0..10u32 {
            enqueue_for_analysis(
                one_trace_batch(&format!("t{i}")),
                &sources,
                &work_tx,
                &metrics,
            );
        }

        // 2 fit the queue, 8 are shed, all without blocking.
        assert_eq!(metrics.analysis_queue_depth.get(), 2);
        assert_eq!(metrics.analysis_shed_batches_total.get(), 8);
        assert_eq!(metrics.analysis_shed_traces_total.get(), 8);
    }

    #[tokio::test]
    async fn saturated_queue_sheds_and_increments_metric() {
        // A full queue sheds the whole batch and records both the batch
        // and the trace count it represented.
        let metrics = MetricsState::new();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);
        let (work_tx, _work_rx) = mpsc::channel::<AnalysisBatch>(1);

        enqueue_for_analysis(one_trace_batch("t1"), &sources, &work_tx, &metrics);
        assert_eq!(metrics.analysis_queue_depth.get(), 1);
        assert_eq!(metrics.analysis_shed_batches_total.get(), 0);

        // Queue full: a 3-trace batch is shed.
        let batch = vec![
            ("t2".to_string(), vec![make_normalized("t2", "SELECT 1")]),
            ("t3".to_string(), vec![make_normalized("t3", "SELECT 1")]),
            ("t4".to_string(), vec![make_normalized("t4", "SELECT 1")]),
        ];
        enqueue_for_analysis(batch, &sources, &work_tx, &metrics);

        assert_eq!(metrics.analysis_shed_batches_total.get(), 1);
        assert_eq!(metrics.analysis_shed_traces_total.get(), 3);
        // The shed batch never entered the queue.
        assert_eq!(metrics.analysis_queue_depth.get(), 1);
    }

    #[tokio::test]
    async fn stopped_worker_counts_as_shed() {
        // Receiver gone (worker stopped): the batch is shed and counted,
        // not silently dropped, so shed-based alerts still fire.
        let metrics = MetricsState::new();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);
        let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(4);
        drop(work_rx);

        let batch = vec![
            ("t1".to_string(), vec![make_normalized("t1", "SELECT 1")]),
            ("t2".to_string(), vec![make_normalized("t2", "SELECT 1")]),
        ];
        enqueue_for_analysis(batch, &sources, &work_tx, &metrics);

        assert_eq!(metrics.analysis_shed_batches_total.get(), 1);
        assert_eq!(metrics.analysis_shed_traces_total.get(), 2);
        assert_eq!(metrics.analysis_queue_depth.get(), 0);
    }

    #[test]
    fn service_meter_overflow_counts_unattributed_ops() {
        let metrics = MetricsState::new();
        let mut meter = ServiceMeter {
            known_services: std::collections::HashSet::new(),
            max_service_cardinality: 2,
            service_cap_warned: false,
        };

        for service in ["svc-a", "svc-b", "svc-c"] {
            meter.record(service, &metrics);
            meter.record(service, &metrics);
        }

        // svc-c arrived after the cap: both its ops overflow, the two
        // attributed services keep counting.
        assert_eq!(metrics.service_io_ops_overflow_total.get(), 2);
        for service in ["svc-a", "svc-b"] {
            let count = metrics
                .service_io_ops_total
                .with_label_values(&[service])
                .get();
            assert!((count - 2.0).abs() < f64::EPSILON);
        }
        assert!(meter.service_cap_warned);
    }

    #[tokio::test]
    async fn shutdown_drains_window_and_inflight_queue() {
        // A batch already buffered in the queue plus the whole in-flight
        // window must both be fully analyzed before the shutdown handshake
        // returns.
        let metrics = Arc::new(MetricsState::new());
        let store = Arc::new(findings_store::FindingsStore::new(100));
        let cell = fresh_green_cell();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);

        let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(4);
        let worker = tokio::spawn(run_analysis_worker(
            work_rx,
            test_worker_ctx(&metrics, &store, &cell),
        ));

        // One in-flight batch (2 traces) already queued.
        let inflight = vec![
            ("q1".to_string(), vec![make_normalized("q1", "SELECT 1")]),
            ("q2".to_string(), vec![make_normalized("q2", "SELECT 1")]),
        ];
        enqueue_for_analysis(inflight, &sources, &work_tx, &metrics);

        // Three more traces sit in the window, to be drained on shutdown.
        let window = test_window();
        {
            let mut w = window.lock().await;
            for id in ["w1", "w2", "w3"] {
                w.push(make_normalized(id, "SELECT 1"), 0);
            }
        }

        drain_to_worker_and_join(&window, &sources, work_tx, worker, &metrics).await;

        // 2 in-flight + 3 drained = 5 traces, all processed before return.
        assert!((metrics.traces_analyzed_total.get() - 5.0).abs() < f64::EPSILON);
        assert_eq!(metrics.analysis_queue_depth.get(), 0);
    }

    /// Dummy listener handles for `drive_event_loop`: never-ending tasks the
    /// shutdown path aborts. Borrowed for the call's duration.
    fn dummy_shutdown<'a>(
        grpc: &'a tokio::task::JoinHandle<()>,
        http: &'a tokio::task::JoinHandle<()>,
    ) -> ShutdownTargets<'a> {
        ShutdownTargets {
            energy: EnergyScraperHandles {
                scaphandre: None,
                kepler: None,
                redfish: None,
                cloud: None,
                emaps: None,
            },
            listeners: ListenerHandles {
                grpc,
                http,
                json_socket: None,
            },
        }
    }

    fn test_loop_cfg() -> EventLoopConfig {
        EventLoopConfig {
            green_enabled: true,
            sampling_rate: 1.0,
            // Large interval; only the immediate first tick can fire, and on
            // an empty/fresh window it is a no-op.
            evict_ms: 60_000,
            confidence: Confidence::DaemonStaging,
            analysis_queue_capacity: 1024,
        }
    }

    #[tokio::test]
    async fn fail_loud_returns_error_when_worker_dies() {
        // The worker stops while the loop runs and no shutdown is requested.
        // drive_event_loop must fail loud so a supervisor restarts the
        // process, rather than looping on while analysis is dead.
        let metrics = MetricsState::new();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);
        let window = test_window();
        let (_tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        let (work_tx, _work_rx) = mpsc::channel::<AnalysisBatch>(4);
        // Stands in for a panicked detector: the worker is already finished.
        let worker = tokio::spawn(async {});
        let grpc = tokio::spawn(std::future::pending::<()>());
        let http = tokio::spawn(std::future::pending::<()>());

        let result = drive_event_loop(
            &mut rx,
            &window,
            &metrics,
            &sources,
            dummy_shutdown(&grpc, &http),
            test_loop_cfg(),
            work_tx,
            worker,
            std::future::pending::<()>(), // shutdown never fires
        )
        .await;

        assert!(matches!(
            result,
            Err(crate::DaemonError::AnalysisWorkerStopped)
        ));
    }

    #[tokio::test]
    async fn graceful_shutdown_drains_window_and_returns_ok() {
        // A live worker plus a shutdown trigger: the loop drains the window
        // through the worker and returns Ok, so the in-flight traces are
        // analyzed before exit.
        let metrics = Arc::new(MetricsState::new());
        let store = Arc::new(findings_store::FindingsStore::new(100));
        let cell = fresh_green_cell();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);
        let window = test_window();
        {
            let mut w = window.lock().await;
            // Fresh timestamps so the immediate ticker tick does not TTL-evict
            // them; the shutdown drain is what must process them.
            for id in ["w1", "w2", "w3"] {
                w.push(make_normalized(id, "SELECT 1"), current_time_ms());
            }
        }

        let (_tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(4);
        let worker = tokio::spawn(run_analysis_worker(
            work_rx,
            test_worker_ctx(&metrics, &store, &cell),
        ));
        let grpc = tokio::spawn(std::future::pending::<()>());
        let http = tokio::spawn(std::future::pending::<()>());

        // Shutdown already requested when the loop starts.
        let (sd_tx, sd_rx) = tokio::sync::oneshot::channel::<()>();
        sd_tx.send(()).expect("receiver alive");
        let shutdown_fut = async move {
            let _ = sd_rx.await;
        };

        let result = drive_event_loop(
            &mut rx,
            &window,
            &metrics,
            &sources,
            dummy_shutdown(&grpc, &http),
            test_loop_cfg(),
            work_tx,
            worker,
            shutdown_fut,
        )
        .await;

        assert!(result.is_ok());
        // The 3 in-flight traces were drained and analyzed before return.
        assert!((metrics.traces_analyzed_total.get() - 3.0).abs() < f64::EPSILON);
    }
}
