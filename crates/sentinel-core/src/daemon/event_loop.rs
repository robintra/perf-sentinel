//! Daemon main event loop: ingest batches, evict expired traces, and route
//! the resulting traces through detect + score + metrics + findings store.

use std::collections::{HashMap, HashSet};

use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::{Duration, interval};

use crate::correlate::Trace;
use crate::correlate::window::{SourceEndpointParentGroups, TraceWindow};
use crate::detect;
use crate::normalize;
use crate::report::metrics::MetricsState;
use crate::report::{DatabaseWaste, GreenSummary, MessagingWaste};
use crate::score;
use crate::score::alumet::{AlumetState, DbEnergyState};
use crate::score::cloud_energy::CloudEnergyState;
use crate::score::electricity_maps::ElectricityMapsState;
use crate::score::kepler::KeplerState;
use crate::score::redfish::RedfishState;
use crate::score::scaphandre::ScaphandreState;
#[cfg(test)]
use detect::sanitizer_aware::SanitizerAwareMode;
use detect::{Confidence, DetectConfig};

use super::findings_store;
use super::hub_export::HubExportBuffer;
use super::sampling::{apply_sampling, should_sample};

type TraceSourceEndpointGroups<T> = HashMap<String, HashMap<Arc<str>, HashMap<String, T>>>;

/// Config slice the main event loop needs, the values that are pulled out
/// of `Config` once at startup and never change.
#[derive(Clone, Copy)]
pub(super) struct EventLoopConfig {
    pub(super) green_enabled: bool,
    pub(super) sampling_rate: f64,
    pub(super) evict_ms: u64,
    pub(super) confidence: Confidence,
    /// How long the live cell keeps the last `database_waste` figure
    /// when newer batches carry none (`0` = never keep). Derived from
    /// the Alumet staleness window so a dead scraper's figure ages out.
    pub(super) waste_sticky_ttl_ms: u64,
    /// Capacity of the bounded analysis worker queue. From
    /// `[daemon] analysis_queue_capacity`.
    pub(super) analysis_queue_capacity: usize,
    /// Whether findings and slow-span histograms carry a `service`
    /// label. From `[daemon] per_service_labels`.
    pub(super) per_service_labels: bool,
}

/// Bundle of handles aborted on shutdown (SIGINT, or SIGTERM on Unix).
pub(super) struct ShutdownTargets<'a> {
    pub(super) energy: EnergyScraperHandles<'a>,
    pub(super) listeners: ListenerHandles<'a>,
}

/// `JoinHandle`s for the optional energy / intensity scrapers.
#[derive(Clone, Copy)]
pub(super) struct EnergyScraperHandles<'a> {
    pub(super) alumet: Option<&'a tokio::task::JoinHandle<()>>,
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
    pub(super) alumet_state: Option<&'a AlumetState>,
    pub(super) alumet_db_state: Option<&'a DbEnergyState>,
    pub(super) alumet_broker_state: Option<&'a DbEnergyState>,
    /// Declared cluster fallback, used only while the Alumet broker
    /// scraper is stale: a measurement always outranks a declaration.
    pub(super) static_broker: Option<(
        &'a score::broker_static::StaticBrokerConfig,
        &'a score::broker_static::StaticBrokerState,
    )>,
    pub(super) alumet_staleness_ms: u64,
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
    per_service_labels: bool,

    confidence: Confidence,
    metrics: Arc<MetricsState>,
    findings_store: Arc<findings_store::FindingsStore>,
    hub_export: Option<Arc<HubExportBuffer>>,
    traces_store: Arc<super::traces_store::TracesStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    green_summary_cell: Arc<RwLock<GreenSummary>>,
    archive_tx: Option<mpsc::Sender<super::archive::OwnedArchive>>,
    waste_sticky_ttl_ms: u64,
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
/// (SIGINT, or SIGTERM on Unix) after draining queued ingest and the
/// in-flight window.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_event_loop(
    rx: &mut mpsc::Receiver<super::IngestBatch>,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: Arc<MetricsState>,
    findings_store: Arc<findings_store::FindingsStore>,
    hub_export: Option<Arc<HubExportBuffer>>,
    traces_store: Arc<super::traces_store::TracesStore>,
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
            per_service_labels: loop_cfg.per_service_labels,

            confidence: loop_cfg.confidence,
            metrics: metrics.clone(),
            findings_store,
            hub_export,
            traces_store,
            correlator,
            green_summary_cell,
            archive_tx,
            waste_sticky_ttl_ms: loop_cfg.waste_sticky_ttl_ms,
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
/// `shutdown_fut` fires; otherwise drains queued ingest and the window into
/// the worker and returns `Ok(())`.
#[allow(clippy::too_many_arguments)]
async fn drive_event_loop(
    rx: &mut mpsc::Receiver<super::IngestBatch>,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

    // Pin the shutdown future once so the SIGTERM/SIGINT listeners are
    // registered a single time rather than re-registered on every loop
    // iteration. Same idiom as the Tempo fetch drain in `ingest::tempo`.
    tokio::pin!(shutdown_fut);

    let graceful = loop {
        tokio::select! {
            Some(batch) = rx.recv() => {
                let lru_evicted = ingest_event_batch(
                    batch,
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
    // Reject new sends after listener abort, then await every buffered batch
    // and any send permit acquired before close.
    rx.close();
    let mut queued_evictions = Vec::new();
    while let Some(batch) = rx.recv().await {
        queued_evictions.extend(
            ingest_event_batch(
                batch,
                loop_cfg.sampling_rate,
                window,
                metrics,
                &mut service_meter,
            )
            .await,
        );
    }
    drain_to_worker_and_join(
        window,
        queued_evictions,
        energy_sources,
        work_tx,
        worker,
        metrics,
    )
    .await;
    Ok(())
}

/// Single analysis worker: pulls batches in FIFO order and runs the
/// CPU-heavy detect+score path off the `select!` loop. Exits when the
/// channel closes (shutdown), after draining every buffered batch.
async fn run_analysis_worker(mut work_rx: mpsc::Receiver<AnalysisBatch>, wctx: AnalysisWorkerCtx) {
    let mut db_waste_sticky: Option<(DatabaseWaste, u64)> = None;
    let mut msg_waste_sticky: Option<(MessagingWaste, u64)> = None;
    let mut service_meter = AnalysisServiceMeter::new(wctx.per_service_labels, &wctx.metrics);

    while let Some(batch) = work_rx.recv().await {
        wctx.metrics.analysis_queue_depth.dec();
        process_traces(
            batch.traces,
            ProcessTracesCtx {
                detect_config: &wctx.detect_config,
                green_enabled: wctx.green_enabled,
                service_meter: &mut service_meter,

                carbon_ctx: batch.carbon_ctx.as_ref(),
                metrics: &wctx.metrics,
                confidence: wctx.confidence,
                findings_store: &wctx.findings_store,
                hub_export: wctx.hub_export.as_deref(),
                traces_store: &wctx.traces_store,
                correlator: wctx.correlator.as_deref(),
                green_summary_cell: &wctx.green_summary_cell,
                archive_tx: wctx.archive_tx.as_ref(),
                db_waste_sticky: &mut db_waste_sticky,
                msg_waste_sticky: &mut msg_waste_sticky,
                waste_sticky_ttl_ms: wctx.waste_sticky_ttl_ms,
            },
        )
        .await;
    }
}

/// Cardinality cap on `perf_sentinel_service_io_ops_total`. Shared with
/// the tuning advisor in `query_api` so its hint names the real cap.
pub(crate) const MAX_SERVICE_CARDINALITY: usize = 1024;

/// Effective service name for metric labels: anonymous spans (Zipkin
/// and Jaeger default to an empty name) take the OTLP default instead of
/// minting `service=""`, the `per_service_labels = false` sentinel.
fn normalize_service(service: &str) -> &str {
    if service.is_empty() {
        "unknown"
    } else {
        service
    }
}

/// Cardinality cap on the analysis-side `service` labels
/// (`findings_total`, `service_avoidable_io_ops_total`,
/// `service_analyzed_io_ops_total`). Lower than
/// [`MAX_SERVICE_CARDINALITY`]: these series multiply by type/severity.
pub(crate) const MAX_ANALYSIS_SERVICE_CARDINALITY: usize = 128;

/// Cardinality cap on the slow-duration histogram's `service` label.
/// Lower still: a histogram costs 14 series per (type, service) pair
/// (11 buckets plus `+Inf`, `_sum` and `_count`).
pub(crate) const MAX_HISTOGRAM_SERVICE_CARDINALITY: usize = 64;

/// Label value that series of services past a cap fold into, so the
/// global sums stay exact while cardinality stays bounded.
pub(crate) const SERVICE_OVERFLOW_LABEL: &str = "_other";

/// Admission policy shared by the service meters: a bounded name set, an
/// overflow counter, a one-shot warning. [`SERVICE_OVERFLOW_LABEL`] is
/// reserved and passes through without taking a slot.
struct CappedServices {
    admitted: HashSet<String>,
    cap: usize,
    warned: bool,
    /// Metric family named in the one-shot cap warning.
    what: &'static str,
}

impl CappedServices {
    fn new(cap: usize, what: &'static str) -> Self {
        Self {
            admitted: HashSet::new(),
            cap,
            warned: false,
            what,
        }
    }

    /// `Some(service)` while it has (or gets) a slot, `None` past the
    /// cap (counts the overflow, warns once).
    fn admit<'a>(
        &mut self,
        service: &'a str,
        overflow: &prometheus::IntCounter,
    ) -> Option<&'a str> {
        if service == SERVICE_OVERFLOW_LABEL || self.admitted.contains(service) {
            return Some(service);
        }
        if self.admitted.len() < self.cap {
            self.admitted.insert(service.to_string());
            return Some(service);
        }
        overflow.inc();
        if !self.warned {
            tracing::warn!(
                cap = self.cap,
                what = self.what,
                "service cardinality cap reached"
            );
            self.warned = true;
        }
        None
    }
}

/// Per-service I/O op counter cache over [`CappedServices`]. Caps
/// cardinality against hostile `service.name` floods and caches the
/// labeled children so the per-event path is one `HashMap` lookup plus
/// an atomic add. Ingest drops past the cap: the overflow counter moves
/// on every unattributed op.
struct ServiceMeter {
    children: HashMap<String, prometheus::Counter>,
    capped: CappedServices,
}

impl ServiceMeter {
    fn new(cap: usize) -> Self {
        Self {
            children: HashMap::new(),
            capped: CappedServices::new(cap, "ingest I/O ops"),
        }
    }

    fn record(&mut self, service: &str, metrics: &MetricsState) {
        let service = normalize_service(service);
        if let Some(child) = self.children.get(service) {
            child.inc();
            return;
        }
        if let Some(label) = self
            .capped
            .admit(service, &metrics.service_io_ops_overflow_total)
        {
            let child = metrics.service_io_ops_total.with_label_values(&[label]);
            child.inc();
            self.children.insert(label.to_string(), child);
        }
    }
}

/// Caps the `service` label on the analysis-side metrics. Single-owner
/// state of the analysis worker task, like [`ServiceMeter`]: no lock.
/// Past a cap, series fold into [`SERVICE_OVERFLOW_LABEL`] so sums stay
/// exact. With `[daemon] per_service_labels = false`, findings and
/// histogram series carry an empty `service`; the per-service I/O
/// counters ignore the knob.
struct AnalysisServiceMeter {
    per_service_labels: bool,
    names: CappedServices,
    hist_names: CappedServices,
    /// `[sql, http_out, messaging]` children per effective label, so the
    /// per-span path is one `HashMap` hit.
    hist_children: HashMap<String, [prometheus::Histogram; 3]>,
}

impl AnalysisServiceMeter {
    /// Materializes the default histogram label up front (0.17 resolved
    /// the children on every batch), so "series absent" keeps meaning
    /// "worker not running" rather than "no slow span yet".
    fn new(per_service_labels: bool, metrics: &MetricsState) -> Self {
        let mut meter = Self {
            per_service_labels,
            names: CappedServices::new(MAX_ANALYSIS_SERVICE_CARDINALITY, "analysis"),
            hist_names: CappedServices::new(
                MAX_HISTOGRAM_SERVICE_CARDINALITY,
                "slow-duration histogram",
            ),
            hist_children: HashMap::new(),
        };
        let prewarm = if per_service_labels {
            SERVICE_OVERFLOW_LABEL
        } else {
            ""
        };
        meter.mint_hist_children(prewarm, metrics);
        meter
    }

    fn mint_hist_children(&mut self, label: &str, metrics: &MetricsState) {
        let children = ["sql", "http_out", "messaging"].map(|kind| {
            metrics
                .slow_duration_seconds
                .with_label_values(&[kind, label])
        });
        self.hist_children.insert(label.to_string(), children);
    }

    /// Effective label under the shared analysis cap: the service, or
    /// [`SERVICE_OVERFLOW_LABEL`] past it.
    fn service_label<'a>(&mut self, service: &'a str, metrics: &MetricsState) -> &'a str {
        self.names
            .admit(
                normalize_service(service),
                &metrics.analysis_service_overflow_total,
            )
            .unwrap_or(SERVICE_OVERFLOW_LABEL)
    }

    /// Label for `findings_total`: empty with the knob off.
    fn finding_label<'a>(&mut self, service: &'a str, metrics: &MetricsState) -> &'a str {
        if self.per_service_labels {
            self.service_label(service, metrics)
        } else {
            ""
        }
    }

    /// Histogram children for a slow span's service, under the
    /// histogram's own cap.
    fn hist_children(
        &mut self,
        service: &str,
        metrics: &MetricsState,
    ) -> &[prometheus::Histogram; 3] {
        let service = if self.per_service_labels {
            normalize_service(service)
        } else {
            ""
        };
        if self.hist_children.contains_key(service) {
            return &self.hist_children[service];
        }

        let label = self
            .hist_names
            .admit(service, &metrics.slow_duration_service_overflow_total)
            .unwrap_or(SERVICE_OVERFLOW_LABEL);
        if !self.hist_children.contains_key(label) {
            self.mint_hist_children(label, metrics);
        }
        &self.hist_children[label]
    }
}

/// Merge one trace's sampled endpoint context and collect any LRU eviction.
fn retain_source_endpoint_context(
    window: &mut TraceWindow,
    trace_id: &str,
    service_root_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
    service_root_parents: &SourceEndpointParentGroups,
    now_ms: u64,
    lru_evicted: &mut Vec<(String, Vec<normalize::NormalizedEvent>)>,
    source_endpoint_generations: &mut HashMap<String, u64>,
) {
    if let Some(evicted) = window.retain_source_endpoint_context_groups(
        trace_id,
        service_root_endpoints,
        service_root_parents,
        now_ms,
    ) {
        lru_evicted.push(evicted);
    }
    if let Some(generation) = window.source_endpoint_generation(trace_id) {
        source_endpoint_generations.insert(trace_id.to_string(), generation);
    }
}

fn group_source_endpoint_updates(
    updates: Vec<super::SourceEndpointUpdate>,
    sampling_rate: f64,
) -> (
    TraceSourceEndpointGroups<String>,
    TraceSourceEndpointGroups<Option<String>>,
) {
    let mut endpoints = HashMap::new();
    let mut parents = HashMap::new();
    for update in updates
        .into_iter()
        .filter(|update| should_sample(&update.trace_id, sampling_rate))
    {
        parents
            .entry(update.trace_id.clone())
            .or_insert_with(HashMap::new)
            .entry(Arc::clone(&update.service))
            .or_insert_with(HashMap::new)
            .insert(update.span_id.clone(), update.parent_span_id);
        if let Some(endpoint) = update.endpoint {
            endpoints
                .entry(update.trace_id)
                .or_insert_with(HashMap::new)
                .entry(update.service)
                .or_insert_with(HashMap::new)
                .insert(update.span_id, endpoint);
        }
    }
    (endpoints, parents)
}

/// Sample, normalize, meter, and push a batch of events into the window.
/// Returns LRU-evicted traces for detect, score, and storage.
async fn ingest_event_batch(
    batch: super::IngestBatch,
    sampling_rate: f64,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
    service_meter: &mut ServiceMeter,
) -> Vec<(String, Vec<normalize::NormalizedEvent>)> {
    let super::IngestBatch {
        events,
        source_endpoint_updates,
    } = batch;
    let events = apply_sampling(events, sampling_rate);
    let event_count = events.len();
    // Normalize OUTSIDE the lock to minimize lock hold time.
    let normalized: Vec<_> = events.into_iter().map(normalize::normalize).collect();
    for event in &normalized {
        service_meter.record(event.event.service.as_ref(), metrics);
    }
    let (source_endpoint_groups, source_endpoint_parent_groups) =
        group_source_endpoint_updates(source_endpoint_updates, sampling_rate);
    let now_ms = current_time_ms();
    let mut lru_evicted = Vec::new();
    let mut source_endpoint_generations = HashMap::new();
    let empty_source_endpoint_groups = HashMap::new();
    {
        // Each push performs at most the fixed ancestor-depth bound of lookups;
        // payload and queue caps bound work held behind this lock.
        let mut w = window.lock().await;
        // Repair existing traces before a new context-only trace can evict them;
        // the second pass retains context that preceded the first I/O event.
        let existing_update_ids: Vec<_> = source_endpoint_parent_groups
            .keys()
            .filter(|trace_id| w.contains_trace(trace_id))
            .cloned()
            .collect();
        for trace_id in &existing_update_ids {
            retain_source_endpoint_context(
                &mut w,
                trace_id,
                source_endpoint_groups
                    .get(trace_id)
                    .unwrap_or(&empty_source_endpoint_groups),
                &source_endpoint_parent_groups[trace_id],
                now_ms,
                &mut lru_evicted,
                &mut source_endpoint_generations,
            );
        }
        let missing_update_ids: Vec<_> = source_endpoint_parent_groups
            .keys()
            .filter(|trace_id| !w.contains_trace(trace_id))
            .cloned()
            .collect();
        for trace_id in &missing_update_ids {
            retain_source_endpoint_context(
                &mut w,
                trace_id,
                source_endpoint_groups
                    .get(trace_id)
                    .unwrap_or(&empty_source_endpoint_groups),
                &source_endpoint_parent_groups[trace_id],
                now_ms,
                &mut lru_evicted,
                &mut source_endpoint_generations,
            );
        }
        for event in normalized {
            let trace_id = event.event.trace_id.as_str();
            if let Some(service_root_parents) = source_endpoint_parent_groups.get(trace_id) {
                let expected_generation = source_endpoint_generations.get(trace_id).copied();
                if expected_generation.is_none()
                    || w.source_endpoint_generation(trace_id) != expected_generation
                {
                    retain_source_endpoint_context(
                        &mut w,
                        trace_id,
                        source_endpoint_groups
                            .get(trace_id)
                            .unwrap_or(&empty_source_endpoint_groups),
                        service_root_parents,
                        now_ms,
                        &mut lru_evicted,
                        &mut source_endpoint_generations,
                    );
                }
            }
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
    match build_tick_ctx(sources, score::scaphandre::monotonic_ms()) {
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

/// Shutdown handshake: merge traces evicted while draining queued ingest
/// with the in-flight window, send them to the worker without shedding, then
/// join the worker so every buffered and in-flight batch is fully processed.
async fn drain_to_worker_and_join(
    window: &Arc<Mutex<TraceWindow>>,
    mut remaining: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    sources: &EnergySources<'_>,
    work_tx: mpsc::Sender<AnalysisBatch>,
    worker: tokio::task::JoinHandle<()>,
    metrics: &MetricsState,
) {
    let window_remaining = {
        let mut w = window.lock().await;
        w.drain_all()
    };
    remaining.extend(window_remaining);
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
    if let Some(handle) = energy.alumet {
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
/// Precedence (highest to lowest): Alumet RAPL, Scaphandre RAPL, Kepler
/// eBPF, Redfish BMC, cloud `SPECpower`. Inserted in reverse order so
/// the highest-fidelity entry wins for any service that appears in
/// multiple snapshots.
// Takes the whole `EnergySources` bundle rather than thirteen
// positional arguments: six of those were mutually type-compatible
// `u64` staleness windows, so a mis-paired argument compiled silently
// and gated one backend's readings by another's staleness.
fn build_tick_ctx<'s>(
    sources: &'s EnergySources<'_>,
    now: u64,
) -> std::borrow::Cow<'s, score::carbon::CarbonContext> {
    let base = &*sources.base_carbon_ctx;
    let EnergySources {
        alumet_state,
        alumet_db_state,
        alumet_broker_state,
        static_broker,
        alumet_staleness_ms,
        scaphandre_state,
        scaphandre_staleness_ms,
        kepler_state,
        kepler_staleness_ms,
        redfish_state,
        redfish_staleness_ms,
        cloud_state,
        cloud_staleness_ms,
        emaps_state,
        emaps_staleness_ms,
        ..
    } = *sources;

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
    // Scaphandre entries override Kepler and every lower-tier source.
    let scaph_snap = scaphandre_state
        .map(|s| s.snapshot(now, scaphandre_staleness_ms))
        .unwrap_or_default();
    // Alumet entries override every other measured source.
    let alumet_snap = alumet_state
        .map(|s| s.snapshot(now, alumet_staleness_ms))
        .unwrap_or_default();
    // Electricity Maps real-time intensity (independent of energy snapshot).
    let emaps_snap = emaps_state
        .map(|s| s.snapshot_with_metadata(now, emaps_staleness_ms))
        .unwrap_or_default();
    // Database energy accumulated since the previous scored batch.
    // Consuming here (once per built batch) keeps shed batches from
    // losing energy: they never build a context.
    let db_window_kwh = alumet_db_state.and_then(|db| db.take_window_kwh(now, alumet_staleness_ms));
    let (measured_broker_kwh, declared_broker_kwh) = take_broker_energy(
        alumet_broker_state,
        static_broker.map(|(_, state)| state),
        now,
        alumet_staleness_ms,
    );

    // Fast path: nothing fresh this tick → no clone, just borrow base.
    if cloud_snap.is_empty()
        && redfish_snap.is_empty()
        && kepler_snap.is_empty()
        && scaph_snap.is_empty()
        && alumet_snap.is_empty()
        && emaps_snap.is_empty()
        && db_window_kwh.is_none()
        && measured_broker_kwh.is_none()
        && declared_broker_kwh.is_none()
    {
        return std::borrow::Cow::Borrowed(base);
    }

    // Slow path: materialize a merged snapshot and clone base.
    let mut merged: HashMap<String, score::carbon::EnergyEntry> = HashMap::with_capacity(
        cloud_snap.len()
            + redfish_snap.len()
            + kepler_snap.len()
            + scaph_snap.len()
            + alumet_snap.len(),
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
    for (service, energy_kwh) in alumet_snap {
        merged.insert(service, score::carbon::EnergyEntry::alumet(energy_kwh));
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
    if let (Some(kwh), Some(db)) = (db_window_kwh, ctx.db_energy.as_mut()) {
        db.window_kwh = kwh;
    }
    if let Some(broker) = ctx.broker_energy.as_mut() {
        patch_broker_energy(
            broker,
            measured_broker_kwh,
            declared_broker_kwh.zip(static_broker.map(|(cfg, _)| cfg)),
        );
    }

    std::borrow::Cow::Owned(ctx)
}

/// Resolve the two broker energy sources for one tick, measured first.
///
/// The arbitration rules and why each is needed are in
/// `docs/design/05-GREENOPS-AND-CARBON.md`, "Broker energy attribution".
/// They are not obvious and three review passes were needed to settle
/// them, so change this against that section, not against intuition.
fn take_broker_energy(
    alumet_state: Option<&DbEnergyState>,
    declared: Option<&score::broker_static::StaticBrokerState>,
    now: u64,
    alumet_staleness_ms: u64,
) -> (Option<f64>, Option<f64>) {
    // The series, not the endpoint: a scrape answering without the broker
    // label measures nothing.
    let measured_owns_the_timeline =
        alumet_state.is_some_and(|b| b.has_recent_sample(now, alumet_staleness_ms));
    if !measured_owns_the_timeline {
        return take_broker_energy_stale(alumet_state, declared, now, alumet_staleness_ms);
    }
    if declared.is_some_and(score::broker_static::StaticBrokerState::clear_outage_billed)
        && let Some(state) = alumet_state
    {
        // Drop the recovery delta: it reaches back over wall clock the
        // declaration billed. The stale branch above drops for the same
        // reason, so both sites are gated on the same marker.
        state.discard_pending();
    }
    let measured = alumet_state.and_then(|b| b.take_window_kwh(now, alumet_staleness_ms));
    // Advance the declared marker without publishing it, so a later
    // fallback bills only time the measurement missed.
    if let Some(state) = declared {
        state.take_window_kwh(now);
    }
    (measured, None)
}

/// The stale half of `take_broker_energy`: the series stopped answering, so
/// the declaration may bill, unless the series banked joules while it was
/// still live. Same arbitration section as the caller.
fn take_broker_energy_stale(
    alumet_state: Option<&DbEnergyState>,
    declared: Option<&score::broker_static::StaticBrokerState>,
    now: u64,
    alumet_staleness_ms: u64,
) -> (Option<f64>, Option<f64>) {
    // Read, never consume: a sub-second stale tick bills nothing (see
    // MIN_BILLABLE_MS) and would otherwise erase the marker before the
    // recovery path can act on it.
    if declared.is_some_and(score::broker_static::StaticBrokerState::outage_billed) {
        // The declaration already billed this stretch, so whatever the
        // series banked since covers time someone else paid for.
        if let Some(state) = alumet_state {
            state.discard_pending();
        }
    } else if let Some(kwh) = alumet_state.and_then(|b| b.take_window_kwh(now, alumet_staleness_ms))
    {
        // Joules banked while the series was live are real, and nothing
        // else billed that stretch, so the declared marker may advance
        // over it. A label never seen banks nothing, so a typo still
        // falls through to the declaration below.
        if let Some(state) = declared {
            state.take_window_kwh(now);
        }
        return (Some(kwh), None);
    }
    let declared_kwh = declared.and_then(|state| state.take_window_kwh(now));
    if declared_kwh.is_some()
        && let Some(state) = declared
    {
        state.mark_outage_billed();
    }
    (None, declared_kwh)
}

/// The tag and region follow the source that actually filled the
/// window, so a fallback tick is never published as a measurement.
fn patch_broker_energy(
    broker: &mut score::carbon::DbEnergyContext,
    measured_kwh: Option<f64>,
    declared: Option<(f64, &score::broker_static::StaticBrokerConfig)>,
) {
    if let Some(kwh) = measured_kwh {
        broker.window_kwh = kwh;
        broker.model = score::carbon::CO2_MODEL_ALUMET;
    } else if let Some((kwh, cfg)) = declared {
        broker.window_kwh = kwh;
        broker.model = crate::report::BROKER_WASTE_MODEL_SPECPOWER;
        broker.region.clone_from(&cfg.region);
    }
}

/// Record slow span durations into a Prometheus histogram.
///
/// `histogram_quantile()` can then compute accurate global percentiles
/// across sharded daemon instances. The meter caches label children per
/// service, so the per-span path stays one `HashMap` lookup instead of
/// the `MetricVec` label-hash + lock of `with_label_values`.
fn record_slow_durations(
    traces: &[Trace],
    detect_config: &DetectConfig,
    metrics: &MetricsState,
    meter: &mut AnalysisServiceMeter,
) {
    let slow_threshold_us = detect_config.slow_threshold_ms.saturating_mul(1000);
    for trace in traces {
        for span in &trace.spans {
            if span.event.duration_us > slow_threshold_us {
                let hists = meter.hist_children(span.event.service.as_ref(), metrics);
                let hist = match span.event.event_type {
                    crate::event::EventType::Sql => &hists[0],
                    crate::event::EventType::HttpOut => &hists[1],
                    crate::event::EventType::Messaging => &hists[2],
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
    per_endpoint_io_ops: &[crate::report::PerEndpointIoOps],
    avoidable_per_service: &HashMap<String, usize>,
    ctx: &mut ProcessTracesCtx<'_>,
) {
    use std::io::Write;

    let metrics = ctx.metrics;
    let meter = &mut *ctx.service_meter;

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
    // Window-scoped energy/carbon scalars for the Grafana Trends panels.
    // Per-service/region breakdown stays off /metrics (cardinality); the
    // totals are bounded and safe to expose as gauges.
    metrics.energy_kwh.set(green_summary.energy_kwh);
    metrics
        .carbon_gco2
        .set(green_summary.regions.iter().map(|r| r.co2_gco2).sum());

    // Per-service avoidable and analysed I/O ops: the two series a
    // per-service waste ratio divides, from the same scoring pass and
    // under the same cap. Both empty when green is off, like the
    // global avoidable counter.
    for (service, ops) in avoidable_per_service {
        metrics
            .service_avoidable_io_ops_total
            .with_label_values(&[meter.service_label(service, metrics)])
            .inc_by(*ops as f64);
    }
    for entry in per_endpoint_io_ops {
        metrics
            .service_analyzed_io_ops_total
            .with_label_values(&[meter.service_label(&entry.service, metrics)])
            .inc_by(entry.io_ops as f64);
    }

    // Resolve effective service labels once; the counter and its
    // exemplars must land on the same series.
    let labeled: Vec<(&detect::Finding, &str)> = findings
        .iter()
        .map(|f| (f, meter.finding_label(&f.service, metrics)))
        .collect();
    metrics.record_exemplars_labeled(&labeled, green_summary);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for (finding, service_label) in &labeled {
        metrics
            .findings_total
            .with_label_values(&[
                finding.finding_type.as_str(),
                finding.severity.as_str(),
                service_label,
            ])
            .inc();
        if serde_json::to_writer(&mut lock, finding).is_ok() {
            let _ = writeln!(lock);
        }
    }
}

/// Count correlator pair evictions, warning once per process: under
/// steady cap pressure every batch loses pairs, and the counter already
/// carries the ongoing magnitude (same policy as the service cap warn).
fn record_correlator_evictions(evicted: usize, metrics: &MetricsState) {
    static CAP_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if evicted == 0 {
        return;
    }
    metrics
        .correlator_pairs_evicted_total
        .inc_by(evicted as u64);
    if !CAP_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            evicted,
            "correlator pair cap reached, dropping pairs (see \
             perf_sentinel_correlator_pairs_evicted_total)"
        );
    }
}

/// Green scoring for one batch, or the disabled envelope when green is
/// off (empty per-endpoint and per-service splits).
fn score_batch(
    traces: &[Trace],
    findings: Vec<detect::Finding>,
    ctx: &ProcessTracesCtx<'_>,
) -> (
    Vec<detect::Finding>,
    GreenSummary,
    Vec<crate::report::PerEndpointIoOps>,
    HashMap<String, usize>,
) {
    if ctx.green_enabled {
        score::score_green(traces, findings, Some(ctx.carbon_ctx))
    } else {
        let total_io_ops = traces.iter().map(|t| t.spans.len()).sum();
        (
            findings,
            GreenSummary::disabled(total_io_ops),
            Vec::new(),
            HashMap::new(),
        )
    }
}

/// Shared context passed to [`process_traces`] on every tick.
///
/// Groups the configuration, state, and downstream sinks so the function
/// signature stays readable. All fields are borrowed for the duration of
/// the call, no ownership transfer.
struct ProcessTracesCtx<'a> {
    detect_config: &'a DetectConfig,
    green_enabled: bool,
    service_meter: &'a mut AnalysisServiceMeter,

    carbon_ctx: &'a score::carbon::CarbonContext,
    metrics: &'a MetricsState,
    confidence: Confidence,
    findings_store: &'a findings_store::FindingsStore,
    hub_export: Option<&'a HubExportBuffer>,
    traces_store: &'a super::traces_store::TracesStore,
    correlator: Option<&'a Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
    green_summary_cell: &'a Arc<RwLock<GreenSummary>>,
    archive_tx: Option<&'a mpsc::Sender<super::archive::OwnedArchive>>,
    /// Worker-owned last `database_waste` figure with its wall-clock
    /// timestamp, see [`sticky_waste_figure`].
    db_waste_sticky: &'a mut Option<(DatabaseWaste, u64)>,
    /// Same for `messaging_waste`: the broker figure has the same duty
    /// cycle, it is filled only on batches where a scrape landed.
    msg_waste_sticky: &'a mut Option<(MessagingWaste, u64)>,
    waste_sticky_ttl_ms: u64,
}

/// Copy the batch summary onto the shared cell, with both waste figures
/// bridged over their scrape gaps. The per-window archive keeps the
/// batch-scoped truth, only the live cell gets the TTL-bounded figures.
async fn publish_live_summary(green_summary: &GreenSummary, ctx: &mut ProcessTracesCtx<'_>) {
    let now_ms = current_time_ms();
    let restored = sticky_waste_figure(
        green_summary.database_waste.as_ref(),
        ctx.db_waste_sticky,
        now_ms,
        ctx.waste_sticky_ttl_ms,
    );
    let restored_msg = sticky_waste_figure(
        green_summary.messaging_waste.as_ref(),
        ctx.msg_waste_sticky,
        now_ms,
        ctx.waste_sticky_ttl_ms,
    );
    let mut cell = ctx.green_summary_cell.write().await;
    cell.clone_from(green_summary);
    cell.database_waste = restored;
    cell.messaging_waste = restored_msg;
}

/// Live-cell stickiness for a waste figure: keep the last one for up to
/// `ttl_ms` so `/api/export/report` does not flap to `None` between
/// scrapes, without pinning a dead scraper's figure forever. The database
/// and broker figures share the shape because they share the cause, an
/// Alumet scrape cadence coarser than the batch cadence.
/// The restored ratio belongs to its own window, an accepted mismatch
/// with the current batch's counters (informational field).
fn sticky_waste_figure<T: Clone>(
    fresh: Option<&T>,
    sticky: &mut Option<(T, u64)>,
    now_ms: u64,
    ttl_ms: u64,
) -> Option<T> {
    if let Some(figure) = fresh {
        *sticky = Some((figure.clone(), now_ms));
        return Some(figure.clone());
    }
    match sticky {
        Some((figure, at)) if now_ms.saturating_sub(*at) <= ttl_ms && ttl_ms > 0 => {
            Some(figure.clone())
        }
        _ => {
            *sticky = None;
            None
        }
    }
}

/// stamps `confidence` on every finding after detection. The
/// value is derived from `config.daemon.environment` in `run()` and passed
/// here unchanged. `analyze` batch mode does not call this function; it
/// uses `pipeline::analyze_with_traces` which hardcodes
/// `Confidence::CiBatch`.
async fn process_traces(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    mut ctx: ProcessTracesCtx<'_>,
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

    record_slow_durations(
        &trace_structs,
        ctx.detect_config,
        ctx.metrics,
        ctx.service_meter,
    );

    // Keep `per_endpoint_io_ops` for the periodic-disclosure archive
    // (design doc 08) and `avoidable_per_service` for /metrics, both
    // computed by `score_green`'s single pass.
    let (mut findings, green_summary, per_endpoint_io_ops, avoidable_per_service) =
        score_batch(&trace_structs, findings, &ctx);

    // Publish the per-batch summary on the shared cell so live daemon
    // snapshots served by `/api/export/report` carry the latest CO2
    // picture. `scoring_config` is also propagated here via
    // `score_green` (it travels through `CarbonContext`), but the
    // handler unconditionally re-applies it from `state.scoring_config`
    // so the audit-trail metadata cannot drift from the startup config.
    publish_live_summary(&green_summary, &mut ctx).await;

    // Stamp the daemon's confidence label. Same shared helper as
    // `pipeline::analyze`, so the two paths cannot drift on the loop.
    detect::apply_confidence(&mut findings, ctx.confidence);
    // Stamp the canonical signature so a daemon snapshot piped into
    // `report --input` carries usable signatures for ack matching.
    crate::acknowledgments::enrich_with_signatures(&mut findings);
    let findings = findings;

    let now_ms = current_time_ms();
    if !findings.is_empty() {
        if let Some(export) = ctx.hub_export {
            let dropped = export.push_batch(&findings, now_ms);
            ctx.metrics.hub_export_dropped_total.inc_by(dropped);
            #[allow(clippy::cast_precision_loss)]
            ctx.metrics.hub_export_pending.set(export.len() as f64);
        }
        ctx.findings_store.push_batch(&findings, now_ms).await;
        ctx.traces_store.retain_for(&trace_structs, &findings).await;
        // Refresh the ring-buffer occupancy gauge (paired with the
        // max_retained_findings cap for the Grafana headroom panel).
        #[allow(clippy::cast_precision_loss)] // bounded by max_retained_findings
        ctx.metrics
            .stored_findings
            .set(ctx.findings_store.len().await as f64);
    }

    if let Some(correlator) = ctx.correlator {
        let evicted = correlator.lock().await.ingest(&findings, now_ms);
        record_correlator_evictions(evicted, ctx.metrics);
    }

    emit_findings_and_update_metrics(
        trace_count,
        &findings,
        &green_summary,
        &per_endpoint_io_ops,
        &avoidable_per_service,
        &mut ctx,
    );

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
                ingest: None,
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
            embedded_traces: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            detection_config: Some(ctx.detect_config.clone()),
            disclosure_waste,
        };
        let archive = super::archive::OwnedArchive {
            ts: chrono::Utc::now(),
            report,
        };
        super::archive::try_send(archive_tx, archive, ctx.metrics);
    }
}

/// Get current time in milliseconds since epoch.
///
/// Returns 0 and logs a warning if the system clock is set before the
/// Unix epoch (effectively a configuration error). Downstream code treats
/// the timestamp as a monotonic-ish sort key; a single zero tick produces
/// visible bucketing but no correctness issue.
///
/// Shared with `daemon::hub_export`: its hourly re-send suppression compares
/// its own stamps against the ones stamped here, so the two must read the
/// same clock the same way.
pub(super) fn current_time_ms() -> u64 {
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
        make_normalized_for_service(trace_id, "test", target)
    }

    fn make_normalized_for_service(
        trace_id: &str,
        service: &str,
        target: &str,
    ) -> normalize::NormalizedEvent {
        let mut event = crate::test_helpers::make_sql_event_with_duration(
            trace_id,
            "s1",
            target,
            "2025-07-10T14:32:01.123Z",
            100,
        );
        event.service = Arc::from(service);
        normalize::normalize(event)
    }

    fn otlp_kv(key: &str, value: &str) -> opentelemetry_proto::tonic::common::v1::KeyValue {
        use opentelemetry_proto::tonic::common::v1::{AnyValue, any_value};

        opentelemetry_proto::tonic::common::v1::KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.to_string())),
            }),
            ..Default::default()
        }
    }

    fn otlp_request(
        service: &str,
        spans: Vec<opentelemetry_proto::tonic::trace::v1::Span>,
    ) -> opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest {
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans};

        opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![otlp_kv("service.name", service)],
                    ..Resource::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans,
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        }
    }

    fn otlp_messaging_span(
        root_span_id: u8,
        span_id: u8,
        destination: &str,
    ) -> opentelemetry_proto::tonic::trace::v1::Span {
        use opentelemetry_proto::tonic::trace::v1::{Span, span::SpanKind};

        Span {
            trace_id: vec![9; 16],
            span_id: vec![span_id; 8],
            parent_span_id: vec![root_span_id; 8],
            name: "orders publish".to_string(),
            kind: SpanKind::Producer as i32,
            start_time_unix_nano: 1_720_621_921_000_000_000,
            end_time_unix_nano: 1_720_621_921_600_000_000,
            attributes: vec![
                otlp_kv("messaging.system", "kafka"),
                otlp_kv("messaging.destination.name", destination),
            ],
            ..Span::default()
        }
    }

    fn otlp_server_root(
        span_id: u8,
        endpoint: &str,
    ) -> opentelemetry_proto::tonic::trace::v1::Span {
        use opentelemetry_proto::tonic::trace::v1::{Span, span::SpanKind};

        Span {
            trace_id: vec![9; 16],
            span_id: vec![span_id; 8],
            name: format!("GET {endpoint}"),
            kind: SpanKind::Server as i32,
            start_time_unix_nano: 1_720_621_921_000_000_000,
            end_time_unix_nano: 1_720_621_922_000_000_000,
            attributes: vec![
                otlp_kv("http.request.method", "GET"),
                otlp_kv("url.path", endpoint),
            ],
            ..Span::default()
        }
    }

    fn make_normalized_messaging(
        trace_id: &str,
        span_id: &str,
        parent_span_id: &str,
        destination: &str,
    ) -> normalize::NormalizedEvent {
        let mut event = crate::test_helpers::make_sql_event_with_duration(
            trace_id,
            span_id,
            destination,
            "2025-07-10T14:32:01.123Z",
            600_000,
        );
        event.parent_span_id = Some(parent_span_id.to_string());
        event.service = Arc::from("orders-svc");
        event.event_type = EventType::Messaging;
        event.operation = "publish".to_string();
        event.source.endpoint = "unknown".to_string();
        normalize::normalize(event)
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
    /// The sticky slot is leaked per call: test-only, a few bytes each.
    /// Zero-capacity store shared by the `process_traces` tests: they
    /// assert on findings and metrics, retention has its own suite.
    fn noop_traces_store() -> &'static crate::daemon::traces_store::TracesStore {
        static STORE: std::sync::OnceLock<crate::daemon::traces_store::TracesStore> =
            std::sync::OnceLock::new();
        STORE.get_or_init(|| crate::daemon::traces_store::TracesStore::new(0, 0))
    }

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
            traces_store: noop_traces_store(),
            green_enabled,
            service_meter: Box::leak(Box::new(AnalysisServiceMeter::new(true, metrics))),

            carbon_ctx,
            metrics,
            confidence: Confidence::DaemonStaging,
            findings_store,
            hub_export: None,
            correlator: None,
            green_summary_cell,
            archive_tx: None,
            db_waste_sticky: Box::leak(Box::new(None)),
            msg_waste_sticky: Box::leak(Box::new(None)),
            waste_sticky_ttl_ms: 0,
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

    #[tokio::test]
    async fn context_only_batch_repairs_source_without_io_metric_inflation() {
        let metrics = MetricsState::new();
        let window = test_window();
        let mut event = make_normalized_for_service("trace-1", "orders-svc", "SELECT 1");
        event.event.source.endpoint = "unknown".to_string();
        event.event.parent_span_id = Some("root-1".to_string());
        window.lock().await.push(event, current_time_ms());
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events: Vec::new(),
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-1".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root-1".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/fault/slow-messaging".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert!(evicted.is_empty());
        let trace = window
            .lock()
            .await
            .peek_clone("trace-1")
            .expect("active trace remains");
        assert_eq!(trace[0].event.source.endpoint, "/api/fault/slow-messaging");
        assert_eq!(trace.len(), 1, "context update is not an event");
        assert!(metrics.events_processed_total.get().abs() < f64::EPSILON);
        assert!(
            metrics
                .service_io_ops_total
                .with_label_values(&["orders-svc"])
                .get()
                .abs()
                < f64::EPSILON
        );
    }

    #[tokio::test]
    async fn late_outer_server_replaces_known_nested_route_within_service() {
        let metrics = MetricsState::new();
        let window = test_window();
        let mut nested_sql = make_normalized_for_service("trace-nested", "laravel-svc", "SELECT 1");
        nested_sql.event.span_id = "sql".to_string();
        nested_sql.event.parent_span_id = Some("nested-server".to_string());
        nested_sql.event.source.endpoint = "/api/payments/history".to_string();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let first = super::super::IngestBatch {
            events: vec![nested_sql.event],
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                trace_id: "trace-nested".to_string(),
                service: Arc::from("laravel-svc"),
                span_id: "nested-server".to_string(),
                parent_span_id: Some("outer-server".to_string()),
                endpoint: Some("/api/payments/history".to_string()),
            }],
        };
        assert!(
            ingest_event_batch(first, 1.0, &window, &metrics, &mut service_meter,)
                .await
                .is_empty()
        );
        assert_eq!(
            window
                .lock()
                .await
                .peek_clone("trace-nested")
                .expect("trace retained")[0]
                .event
                .source
                .endpoint,
            "/api/payments/history"
        );

        let outer = super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                trace_id: "trace-nested".to_string(),
                service: Arc::from("laravel-svc"),
                span_id: "outer-server".to_string(),
                parent_span_id: None,
                endpoint: Some("/api/fault/pool-saturation".to_string()),
            }],
        };
        assert!(
            ingest_event_batch(outer, 1.0, &window, &metrics, &mut service_meter,)
                .await
                .is_empty()
        );

        let trace = window
            .lock()
            .await
            .peek_clone("trace-nested")
            .expect("trace retained");
        assert_eq!(trace[0].event.source.endpoint, "/api/fault/pool-saturation");
        let finished = window.lock().await.drain_all();
        assert_eq!(
            finished[0].1[0].event.source.endpoint,
            "/api/fault/pool-saturation"
        );
    }

    #[tokio::test]
    async fn late_outer_route_crosses_a_retained_internal_edge() {
        let metrics = MetricsState::new();
        let window = test_window();
        let mut nested_sql =
            make_normalized_for_service("trace-internal", "laravel-svc", "SELECT 1");
        nested_sql.event.span_id = "sql".to_string();
        nested_sql.event.parent_span_id = Some("inner-server".to_string());
        nested_sql.event.source.endpoint = "/api/payments/history".to_string();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let first = super::super::IngestBatch {
            events: vec![nested_sql.event],
            source_endpoint_updates: vec![
                super::super::SourceEndpointUpdate {
                    trace_id: "trace-internal".to_string(),
                    service: Arc::from("laravel-svc"),
                    span_id: "inner-server".to_string(),
                    parent_span_id: Some("internal".to_string()),
                    endpoint: Some("/api/payments/history".to_string()),
                },
                super::super::SourceEndpointUpdate {
                    trace_id: "trace-internal".to_string(),
                    service: Arc::from("laravel-svc"),
                    span_id: "internal".to_string(),
                    parent_span_id: Some("outer".to_string()),
                    endpoint: None,
                },
            ],
        };
        assert!(
            ingest_event_batch(first, 1.0, &window, &metrics, &mut service_meter)
                .await
                .is_empty()
        );
        assert!((metrics.events_processed_total.get() - 1.0).abs() < f64::EPSILON);

        let outer = super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                trace_id: "trace-internal".to_string(),
                service: Arc::from("laravel-svc"),
                span_id: "outer".to_string(),
                parent_span_id: None,
                endpoint: Some("/api/fault/pool-saturation".to_string()),
            }],
        };
        assert!(
            ingest_event_batch(outer, 1.0, &window, &metrics, &mut service_meter)
                .await
                .is_empty()
        );
        assert!((metrics.events_processed_total.get() - 1.0).abs() < f64::EPSILON);

        let trace = window
            .lock()
            .await
            .peek_clone("trace-internal")
            .expect("trace retained");
        assert_eq!(trace[0].event.source.endpoint, "/api/fault/pool-saturation");
        let finished = window.lock().await.drain_all();
        assert_eq!(
            finished[0].1[0].event.source.endpoint,
            "/api/fault/pool-saturation"
        );
    }

    #[tokio::test]
    async fn late_caller_root_does_not_cross_the_service_boundary() {
        let metrics = MetricsState::new();
        let window = test_window();
        let mut callee_sql = make_normalized_for_service("trace-cross", "payments-svc", "SELECT 1");
        callee_sql.event.span_id = "sql".to_string();
        callee_sql.event.parent_span_id = Some("callee-server".to_string());
        callee_sql.event.source.endpoint = "/api/payments/history".to_string();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        assert!(
            ingest_event_batch(
                super::super::IngestBatch {
                    events: vec![callee_sql.event],
                    source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                        trace_id: "trace-cross".to_string(),
                        service: Arc::from("payments-svc"),
                        span_id: "callee-server".to_string(),
                        parent_span_id: Some("caller-server".to_string()),
                        endpoint: Some("/api/payments/history".to_string()),
                    }],
                },
                1.0,
                &window,
                &metrics,
                &mut service_meter,
            )
            .await
            .is_empty()
        );
        assert!(
            ingest_event_batch(
                super::super::IngestBatch {
                    events: Vec::new(),
                    source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                        trace_id: "trace-cross".to_string(),
                        service: Arc::from("orders-svc"),
                        span_id: "caller-server".to_string(),
                        parent_span_id: None,
                        endpoint: Some("/api/orders".to_string()),
                    }],
                },
                1.0,
                &window,
                &metrics,
                &mut service_meter,
            )
            .await
            .is_empty()
        );

        let trace = window
            .lock()
            .await
            .peek_clone("trace-cross")
            .expect("trace retained");
        assert_eq!(trace[0].event.source.endpoint, "/api/payments/history");
        let finished = window.lock().await.drain_all();
        assert_eq!(
            finished[0].1[0].event.source.endpoint,
            "/api/payments/history"
        );
    }

    #[tokio::test]
    async fn zero_sampling_drops_root_context_without_evicting_a_kept_trace() {
        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_active_traces: std::num::NonZeroUsize::new(1).expect("nonzero"),
            ..WindowConfig::default()
        })));
        window.lock().await.push(
            make_normalized_messaging("kept", "span", "root", "orders"),
            current_time_ms(),
        );
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events: Vec::new(),
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "dropped".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/dropped".to_string()),
                }],
            },
            0.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        let guard = window.lock().await;
        assert!(evicted.is_empty());
        assert!(guard.peek_clone("kept").is_some());
        assert!(guard.peek_clone("dropped").is_none());
    }

    #[tokio::test]
    async fn partial_sampling_keeps_only_matching_root_context() {
        let rate = 0.5;
        let trace_id_for = |keep: bool| {
            (0..10_000)
                .map(|index| format!("sampling-root-{index}"))
                .find(|trace_id| {
                    let event = make_normalized_messaging(trace_id, "span", "root", "orders").event;
                    apply_sampling(vec![event], rate).is_empty() != keep
                })
                .expect("sampling decision of requested kind")
        };
        let kept_trace_id = trace_id_for(true);
        let dropped_trace_id = trace_id_for(false);
        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_active_traces: std::num::NonZeroUsize::new(1).expect("nonzero"),
            ..WindowConfig::default()
        })));
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);
        let root_batch = |trace_id: &str, endpoint: &str| super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                trace_id: trace_id.to_string(),
                service: Arc::from("orders-svc"),
                span_id: "root".to_string(),
                parent_span_id: None,
                endpoint: Some(endpoint.to_string()),
            }],
        };

        assert!(
            ingest_event_batch(
                root_batch(&kept_trace_id, "/api/kept"),
                rate,
                &window,
                &metrics,
                &mut service_meter,
            )
            .await
            .is_empty()
        );
        assert!(window.lock().await.peek_clone(&kept_trace_id).is_some());
        assert!(
            ingest_event_batch(
                root_batch(&dropped_trace_id, "/api/dropped"),
                rate,
                &window,
                &metrics,
                &mut service_meter,
            )
            .await
            .is_empty()
        );

        let guard = window.lock().await;
        assert!(guard.peek_clone(&kept_trace_id).is_some());
        assert!(guard.peek_clone(&dropped_trace_id).is_none());
    }

    #[tokio::test]
    async fn same_batch_root_reconciles_new_trace_before_detection() {
        let metrics = MetricsState::new();
        let window = test_window();
        let events = (0..3)
            .map(|index| {
                make_normalized_messaging(
                    "trace-new",
                    &format!("span-{index}"),
                    "root-new",
                    "orders",
                )
                .event
            })
            .collect();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events,
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-new".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root-new".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/orders".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert!(evicted.is_empty());
        let spans = window
            .lock()
            .await
            .peek_clone("trace-new")
            .expect("new trace remains active");
        let findings = detect::slow::detect_slow(
            &Trace {
                trace_id: "trace-new".to_string(),
                spans,
            },
            500,
            3,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_endpoint, "/api/orders");
        assert!((metrics.events_processed_total.get() - 3.0).abs() < f64::EPSILON);
        assert!((metrics.active_traces.get() - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn same_batch_root_reconciles_existing_and_new_events() {
        let metrics = MetricsState::new();
        let window = test_window();
        window.lock().await.push(
            make_normalized_messaging("trace-1", "old", "root-1", "orders"),
            current_time_ms(),
        );
        let event = make_normalized_messaging("trace-1", "new", "root-1", "orders").event;
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events: vec![event],
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-1".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root-1".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/orders".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert!(evicted.is_empty());
        let trace = window
            .lock()
            .await
            .peek_clone("trace-1")
            .expect("existing trace remains active");
        assert_eq!(trace.len(), 2);
        assert!(
            trace
                .iter()
                .all(|event| event.event.source.endpoint == "/api/orders")
        );
        assert_eq!(window.lock().await.reconciliation_passes(), 1);
    }

    #[tokio::test]
    async fn same_batch_root_reconciles_new_trace_evicted_during_ingest() {
        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_active_traces: std::num::NonZeroUsize::new(1).expect("nonzero"),
            ..WindowConfig::default()
        })));
        let events = vec![
            make_normalized_messaging("trace-a", "span-a", "root-a", "orders").event,
            make_normalized_messaging("trace-b", "span-b", "root-b", "orders").event,
        ];
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events,
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-a".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root-a".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/orders".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "trace-a");
        assert_eq!(evicted[0].1[0].event.source.endpoint, "/api/orders");
    }

    #[tokio::test]
    async fn same_batch_reconciliation_stays_within_the_bounded_work_budget() {
        const EVENT_COUNT: usize = 10_000;

        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_events_per_trace: EVENT_COUNT,
            ..WindowConfig::default()
        })));
        for index in 0..EVENT_COUNT / 2 {
            window.lock().await.push(
                make_normalized_messaging("trace-1", &format!("old-{index}"), "root-1", "orders"),
                current_time_ms(),
            );
        }
        let events = (0..EVENT_COUNT / 2)
            .map(|index| {
                make_normalized_messaging("trace-1", &format!("new-{index}"), "root-1", "orders")
                    .event
            })
            .collect();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let started = std::time::Instant::now();
        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events,
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-1".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root-1".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/orders".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert!(evicted.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "same-batch reconciliation exceeded the bounded-work budget"
        );
        assert!(
            window
                .lock()
                .await
                .peek_clone("trace-1")
                .expect("trace remains active")
                .iter()
                .all(|event| event.event.source.endpoint == "/api/orders")
        );
    }

    #[tokio::test]
    async fn oversized_root_group_is_reconciled_once_per_batch() {
        const CAP: usize = 100;

        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_events_per_trace: CAP,
            ..WindowConfig::default()
        })));
        window.lock().await.push(
            make_normalized_messaging("trace-1", "existing", "root-0", "orders"),
            current_time_ms(),
        );
        let events = (0..CAP)
            .map(|index| {
                make_normalized_messaging("trace-1", &format!("span-{index}"), "root-0", "orders")
                    .event
            })
            .collect();
        let source_endpoint_updates = (0..=CAP)
            .map(|index| super::super::SourceEndpointUpdate {
                trace_id: "trace-1".to_string(),
                service: Arc::from("orders-svc"),
                span_id: format!("root-{index}"),
                parent_span_id: None,
                endpoint: Some(format!("/api/{index}")),
            })
            .collect();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events,
                source_endpoint_updates,
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert!(evicted.is_empty());
        assert_eq!(window.lock().await.reconciliation_passes(), 1);
    }

    #[tokio::test]
    async fn unresolved_io_only_batches_stay_within_the_bounded_work_budget() {
        const BATCH_COUNT: usize = 500;
        const EVENTS_PER_BATCH: usize = 100;
        const EVENT_COUNT: usize = BATCH_COUNT * EVENTS_PER_BATCH;

        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_events_per_trace: EVENT_COUNT,
            ..WindowConfig::default()
        })));
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);
        ingest_event_batch(
            super::super::IngestBatch {
                events: Vec::new(),
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-1".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/orders".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        let started = std::time::Instant::now();
        for batch_index in 0..BATCH_COUNT {
            let events = (0..EVENTS_PER_BATCH)
                .map(|event_index| {
                    let index = batch_index * EVENTS_PER_BATCH + event_index;
                    make_normalized_messaging(
                        "trace-1",
                        &format!("span-{index}"),
                        &format!("missing-{index}"),
                        "orders",
                    )
                    .event
                })
                .collect();
            assert!(
                ingest_event_batch(
                    super::super::IngestBatch {
                        events,
                        source_endpoint_updates: Vec::new(),
                    },
                    1.0,
                    &window,
                    &metrics,
                    &mut service_meter,
                )
                .await
                .is_empty()
            );
        }
        let drained = window.lock().await.drain_all();

        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].1.len(), EVENT_COUNT);
        assert!(
            drained[0]
                .1
                .iter()
                .all(|event| event.event.source.endpoint == "unknown")
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "I/O-only batches repeatedly rescanned unresolved traces"
        );
    }

    #[tokio::test]
    async fn source_update_precedes_lru_eviction_in_the_same_batch() {
        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_active_traces: std::num::NonZeroUsize::new(1).expect("nonzero"),
            ..WindowConfig::default()
        })));
        let mut trace_a = make_normalized_for_service("trace-a", "orders-svc", "SELECT 1");
        trace_a.event.source.endpoint = "unknown".to_string();
        trace_a.event.parent_span_id = Some("root-a".to_string());
        window.lock().await.push(trace_a, current_time_ms());

        let mut trace_b = crate::test_helpers::make_sql_event_with_duration(
            "trace-b",
            "span-b",
            "SELECT 2",
            "2025-07-10T14:32:01.123Z",
            100,
        );
        trace_b.service = Arc::from("orders-svc");
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        let evicted = ingest_event_batch(
            super::super::IngestBatch {
                events: vec![trace_b],
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    trace_id: "trace-a".to_string(),
                    service: Arc::from("orders-svc"),
                    span_id: "root-a".to_string(),
                    parent_span_id: None,
                    endpoint: Some("/api/orders".to_string()),
                }],
            },
            1.0,
            &window,
            &metrics,
            &mut service_meter,
        )
        .await;

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "trace-a");
        assert_eq!(evicted[0].1[0].event.source.endpoint, "/api/orders");
        assert_eq!(window.lock().await.reconciliation_passes(), 1);
    }

    #[tokio::test]
    async fn daemon_otlp_batches_reconcile_two_late_roots_through_real_ingest() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        let (tx, mut rx) = mpsc::channel(2);
        let service =
            crate::ingest::otlp::OtlpGrpcService::new_daemon_with_grouping(tx, None, Vec::new());
        let mut children = Vec::new();
        for span_id in 10..13 {
            children.push(otlp_messaging_span(1, span_id, "orders-a"));
        }
        for span_id in 20..23 {
            children.push(otlp_messaging_span(2, span_id, "orders-b"));
        }
        service
            .export(tonic::Request::new(otlp_request("orders-svc", children)))
            .await
            .expect("children export accepted");
        service
            .export(tonic::Request::new(otlp_request(
                "orders-svc",
                vec![otlp_server_root(2, "/api/b"), otlp_server_root(1, "/api/a")],
            )))
            .await
            .expect("late roots export accepted");

        let metrics = MetricsState::new();
        let window = test_window();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);
        for _ in 0..2 {
            let batch = rx.recv().await.expect("daemon ingest batch sent");
            let evicted =
                ingest_event_batch(batch, 1.0, &window, &metrics, &mut service_meter).await;
            assert!(evicted.is_empty());
        }

        let (trace_id, spans) = window
            .lock()
            .await
            .drain_all()
            .pop()
            .expect("one active trace");
        let findings = detect::slow::detect_slow(&Trace { trace_id, spans }, 500, 3);
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|finding| {
            finding.finding_type == detect::FindingType::SlowMessaging
                && finding.pattern.occurrences == 3
        }));
        let mut endpoints: Vec<_> = findings
            .iter()
            .map(|finding| finding.source_endpoint.as_str())
            .collect();
        endpoints.sort_unstable();
        assert_eq!(endpoints, ["/api/a", "/api/b"]);
    }

    #[tokio::test]
    async fn daemon_otlp_root_first_batch_reconciles_later_io_through_real_ingest() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        let (tx, mut rx) = mpsc::channel(2);
        let service =
            crate::ingest::otlp::OtlpGrpcService::new_daemon_with_grouping(tx, None, Vec::new());
        service
            .export(tonic::Request::new(otlp_request(
                "orders-svc",
                vec![otlp_server_root(1, "/api/fastapi")],
            )))
            .await
            .expect("early root export accepted");

        let metrics = MetricsState::new();
        let window = test_window();
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);
        let root_batch = rx.recv().await.expect("early root batch sent");
        assert!(
            ingest_event_batch(root_batch, 1.0, &window, &metrics, &mut service_meter)
                .await
                .is_empty()
        );
        assert!(metrics.events_processed_total.get().abs() < f64::EPSILON);
        assert!(
            metrics
                .service_io_ops_total
                .with_label_values(&["orders-svc"])
                .get()
                .abs()
                < f64::EPSILON
        );

        let children = (10..13)
            .map(|span_id| otlp_messaging_span(1, span_id, "orders"))
            .collect();
        service
            .export(tonic::Request::new(otlp_request("orders-svc", children)))
            .await
            .expect("later I/O export accepted");
        let io_batch = rx.recv().await.expect("later I/O batch sent");
        assert!(
            ingest_event_batch(io_batch, 1.0, &window, &metrics, &mut service_meter)
                .await
                .is_empty()
        );

        let (trace_id, spans) = window
            .lock()
            .await
            .drain_all()
            .pop()
            .expect("one trace with I/O");
        let findings = detect::slow::detect_slow(&Trace { trace_id, spans }, 500, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_endpoint, "/api/fastapi");
    }

    #[tokio::test]
    async fn daemon_otlp_blank_service_does_not_link_separate_exports_at_cap_one() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        let (tx, mut rx) = mpsc::channel(2);
        let service =
            crate::ingest::otlp::OtlpGrpcService::new_daemon_with_grouping(tx, None, Vec::new());
        service
            .export(tonic::Request::new(otlp_request(
                " \t ",
                vec![otlp_server_root(1, "/api/anonymous")],
            )))
            .await
            .expect("anonymous root export accepted");
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        service
            .export(tonic::Request::new(otlp_request(
                " \t ",
                vec![otlp_messaging_span(1, 10, "orders")],
            )))
            .await
            .expect("anonymous I/O export accepted");
        let batch = rx.recv().await.expect("anonymous I/O batch sent");
        let metrics = MetricsState::new();
        let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            max_active_traces: std::num::NonZeroUsize::new(1).expect("nonzero"),
            ..WindowConfig::default()
        })));
        let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY);

        assert!(
            ingest_event_batch(batch, 1.0, &window, &metrics, &mut service_meter)
                .await
                .is_empty()
        );
        let (_, spans) = window
            .lock()
            .await
            .drain_all()
            .pop()
            .expect("one anonymous trace");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].event.service.as_ref(), "unknown");
        assert_eq!(spans[0].event.source.endpoint, "unknown");
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
            link_trace_id: None,
            service: Arc::from("test"),
            grouping: Vec::new(),
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
        let base = Arc::new(score::carbon::CarbonContext::default());
        let sources = no_scrapers(&base);
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        assert_matches!(ctx, std::borrow::Cow::Borrowed(_));
        assert!(ctx.energy_snapshot.is_none());
    }

    #[test]
    fn build_tick_ctx_scaphandre_only() {
        let base = Arc::new(score::carbon::CarbonContext::default());
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.scaphandre_state = Some(&scaph);
        sources.scaphandre_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
    }

    #[test]
    fn build_tick_ctx_cloud_only() {
        let base = Arc::new(score::carbon::CarbonContext::default());
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-b".into(), 2e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.cloud_state = Some(&cloud);
        sources.cloud_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-b"].model_tag, "cloud_specpower");
    }

    #[test]
    fn build_tick_ctx_kepler_only() {
        let base = Arc::new(score::carbon::CarbonContext::default());
        let kepler = KeplerState::new();
        kepler.insert_for_test("svc-k".into(), 4e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.kepler_state = Some(&kepler);
        sources.kepler_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-k"].model_tag, "kepler_ebpf");
    }

    #[test]
    fn build_tick_ctx_redfish_only() {
        let base = Arc::new(score::carbon::CarbonContext::default());
        let redfish = RedfishState::new();
        redfish.insert_for_test("svc-r".into(), 6e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.redfish_state = Some(&redfish);
        sources.redfish_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-r"].model_tag, "redfish_bmc");
    }

    #[test]
    fn build_tick_ctx_scaphandre_overrides_kepler_overrides_cloud_for_same_service() {
        let base = Arc::new(score::carbon::CarbonContext::default());
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let kepler = KeplerState::new();
        kepler.insert_for_test("svc-a".into(), 2e-7, 100);
        kepler.insert_for_test("svc-k".into(), 4e-7, 100);
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-a".into(), 5e-7, 100);
        cloud.insert_for_test("svc-b".into(), 3e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.scaphandre_state = Some(&scaph);
        sources.scaphandre_staleness_ms = 500;
        sources.kepler_state = Some(&kepler);
        sources.kepler_staleness_ms = 500;
        sources.cloud_state = Some(&cloud);
        sources.cloud_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
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
    fn build_tick_ctx_alumet_only() {
        let base = Arc::new(score::carbon::CarbonContext::default());
        let alumet = AlumetState::new();
        alumet.insert_for_test("svc-al".into(), 8e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.alumet_state = Some(&alumet);
        sources.alumet_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-al"].model_tag, "alumet_rapl");
    }

    fn waste_fixture(ratio: f64) -> DatabaseWaste {
        DatabaseWaste {
            energy_kwh: 0.01,
            waste_kwh: 0.01 * ratio,
            waste_gco2: None,
            energy_gco2: None,
            region: None,
            sql_waste_ratio: ratio,
            model: "alumet_rapl".to_string(),
        }
    }

    #[test]
    fn sticky_waste_figure_bridges_gaps_then_ages_out() {
        let mut sticky = None;
        let fresh = waste_fixture(0.4);
        // A fresh figure is stored and passed through.
        let out = sticky_waste_figure(Some(&fresh), &mut sticky, 1_000, 30_000);
        assert_eq!(out.as_ref(), Some(&fresh));
        // Gap between scrapes: the last figure bridges it.
        let out = sticky_waste_figure(None, &mut sticky, 10_000, 30_000);
        assert_eq!(out.as_ref(), Some(&fresh));
        // Scraper dead: the figure ages out instead of pinning forever.
        let out = sticky_waste_figure(None, &mut sticky, 40_000, 30_000);
        assert!(out.is_none());
        assert!(sticky.is_none(), "aged-out figure must be dropped");
    }

    #[test]
    fn sticky_waste_figure_disabled_at_zero_ttl() {
        let mut sticky = None;
        let fresh = waste_fixture(0.2);
        assert!(sticky_waste_figure(Some(&fresh), &mut sticky, 1_000, 0).is_some());
        assert!(sticky_waste_figure(None, &mut sticky, 1_001, 0).is_none());
    }

    #[test]
    fn build_tick_ctx_database_energy_forces_owned_then_consumes() {
        let base = Arc::new(score::carbon::CarbonContext {
            db_energy: Some(score::carbon::DbEnergyContext {
                window_kwh: 0.0,
                region: None,
                ..Default::default()
            }),
            ..score::carbon::CarbonContext::default()
        });
        let db = DbEnergyState::new();
        let now = score::scaphandre::monotonic_ms();
        db.add_window_kwh(2e-6, now);
        let mut sources = no_scrapers(&base);
        sources.alumet_db_state = Some(&db);
        sources.alumet_staleness_ms = 60_000;

        // Fresh DB energy alone must force the owned path and patch it in.
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        assert!(
            matches!(ctx, std::borrow::Cow::Owned(_)),
            "fresh db energy must not take the borrowed fast path"
        );
        let kwh = ctx.db_energy.as_ref().unwrap().window_kwh;
        assert!((kwh - 2e-6).abs() < 1e-18);

        // The take consumed it: the next build borrows the base again.
        let ctx2 = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        assert!(matches!(ctx2, std::borrow::Cow::Borrowed(_)));
        assert!((ctx2.db_energy.as_ref().unwrap().window_kwh - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn build_tick_ctx_broker_only_energy_forces_owned() {
        // Broker energy alone must leave the borrowed fast path, otherwise
        // a tick with no other fresh source would silently drop it.
        let base = Arc::new(score::carbon::CarbonContext {
            broker_energy: Some(score::carbon::DbEnergyContext {
                window_kwh: 0.0,
                region: None,
                ..Default::default()
            }),
            ..score::carbon::CarbonContext::default()
        });
        let broker = DbEnergyState::new();
        broker.add_window_kwh(3e-6, 10_000);
        let mut sources = no_scrapers(&base);
        sources.alumet_broker_state = Some(&broker);
        sources.alumet_staleness_ms = 60_000;

        let ctx = build_tick_ctx(&sources, 10_000);
        assert!(
            matches!(ctx, std::borrow::Cow::Owned(_)),
            "fresh broker energy must not take the borrowed fast path"
        );
        let kwh = ctx.broker_energy.as_ref().unwrap().window_kwh;
        assert!((kwh - 3e-6).abs() < 1e-18);

        let ctx2 = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        assert!(matches!(ctx2, std::borrow::Cow::Borrowed(_)));
    }

    fn declared_cfg(nodes: u32) -> score::broker_static::StaticBrokerConfig {
        score::broker_static::StaticBrokerConfig {
            nodes,
            instance_type: "m5.2xlarge".to_string(),
            provider: "aws".to_string(),
            region: Some("eu-west-3".to_string()),
        }
    }

    #[test]
    fn a_measured_broker_outranks_the_declared_cluster() {
        let measured = DbEnergyState::new();
        measured.add_window_kwh(5e-6, 10_000);
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);

        let (m, d) = take_broker_energy(Some(&measured), Some(&state), 10_000, 60_000);
        assert_eq!(m, Some(5e-6));
        assert!(
            d.is_none(),
            "a declaration must not be billed beside a measurement"
        );
    }

    #[test]
    fn a_gap_between_alumet_deltas_is_not_billed_by_the_declaration() {
        // Alumet delivers retroactively: the next delta will cover this
        // interval too, so billing it now publishes the same wall clock
        // twice, once per model tag.
        let measured = DbEnergyState::new();
        measured.add_window_kwh(5e-6, 10_000);
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);
        take_broker_energy(Some(&measured), Some(&state), 10_000, 60_000);

        // Tick 2: no fresh scrape landed, but the scraper is still live.
        let (m, d) = take_broker_energy(Some(&measured), Some(&state), 20_000, 60_000);
        assert!(m.is_none(), "no delta accumulated");
        assert!(
            d.is_none(),
            "the declaration would re-bill what the next Alumet delta covers"
        );
    }

    #[test]
    fn a_stale_alumet_hands_the_window_over_to_the_declaration() {
        let measured = DbEnergyState::new();
        measured.add_window_kwh(5e-6, 1_000);
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);

        // Far past the staleness window: the measurement no longer owns
        // the timeline, so the declaration takes over.
        let (m, d) = take_broker_energy(Some(&measured), Some(&state), 100_000, 10_000);
        assert!(m.is_none());
        assert!(d.is_some_and(|k| k > 0.0));
    }

    #[test]
    fn recovery_after_a_fallback_stretch_drops_the_banked_energy() {
        let measured = DbEnergyState::new();
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);
        measured.add_window_kwh(5e-6, 1_000);

        // The scraper goes stale and the declaration covers the outage.
        let (_, d) = take_broker_energy(Some(&measured), Some(&state), 100_000, 10_000);
        assert!(d.is_some(), "the declaration covers the outage");

        // It recovers. That first delta reaches back over the outage the
        // declaration already billed, so it is dropped rather than added.
        measured.add_window_kwh(2e-6, 101_000);
        let (m, d2) = take_broker_energy(Some(&measured), Some(&state), 101_000, 10_000);
        assert!(m.is_none(), "the recovery delta covers billed wall clock");
        assert!(d2.is_none(), "the measurement owns the timeline again");

        // The next delta is genuinely new and is delivered in full.
        measured.add_window_kwh(3e-6, 102_000);
        let (m2, _) = take_broker_energy(Some(&measured), Some(&state), 102_000, 10_000);
        let delivered = m2.expect("the measurement resumes");
        assert!(
            (delivered - 3e-6).abs() < 1e-18,
            "only the joules after the handover may be billed, got {delivered}"
        );
    }

    #[test]
    fn a_bank_landing_after_a_billed_outage_is_dropped_not_delivered() {
        // The endpoint keeps answering while the label vanishes, so the
        // declaration bills the outage. When the label returns, its delta
        // reaches back over that stretch and must not be paid twice.
        let measured = DbEnergyState::new();
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);
        measured.add_window_kwh(1e-6, 1_000);
        take_broker_energy(Some(&measured), Some(&state), 1_000, 10_000);

        // Label gone, endpoint alive: the declaration covers the outage.
        measured.mark_alive(100_000);
        let (_, d) = take_broker_energy(Some(&measured), Some(&state), 100_000, 10_000);
        assert!(d.is_some(), "the declaration bills the outage");

        // One late sample banks a delta spanning the whole outage, but the
        // label is stale again by the next window.
        measured.add_window_kwh(5e-6, 105_000);
        measured.mark_alive(200_000);
        let (m, _) = take_broker_energy(Some(&measured), Some(&state), 200_000, 10_000);
        assert!(
            m.is_none(),
            "the banked delta covers wall clock the declaration already billed"
        );
    }

    #[test]
    fn sub_second_stale_ticks_do_not_erase_the_outage_marker() {
        // Regression: the marker states a fact about the timeline. A stale
        // tick spaced under MIN_BILLABLE_MS bills nothing, so consuming it
        // there lost the fact and the recovery delta was billed twice. With
        // trace_ttl_ms = 1000 the eviction sweep lands every 500 ms, so this
        // cadence is the default under continuous traffic, not an edge case.
        let measured = DbEnergyState::new();
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);
        measured.add_window_kwh(1e-6, 1_000);
        take_broker_energy(Some(&measured), Some(&state), 1_000, 10_000);

        // Label gone, endpoint alive: one billing tick sets the marker.
        measured.mark_alive(100_000);
        let (_, d) = take_broker_energy(Some(&measured), Some(&state), 100_000, 10_000);
        assert!(d.is_some(), "the declaration bills the outage");

        // Then several sub-second stale ticks, none of which bills.
        for t in [100_300_u64, 100_600, 100_900] {
            measured.mark_alive(t);
            let (_, billed) = take_broker_energy(Some(&measured), Some(&state), t, 10_000);
            assert!(billed.is_none(), "a sub-second tick bills nothing at t={t}");
        }

        // The catch-up sample still covers wall clock already billed.
        measured.add_window_kwh(5e-6, 101_000);
        measured.mark_alive(200_000);
        let (m, _) = take_broker_energy(Some(&measured), Some(&state), 200_000, 10_000);
        assert!(
            m.is_none(),
            "the marker must survive ticks that bill nothing"
        );
    }

    #[test]
    fn the_declared_marker_advances_while_alumet_owns_the_timeline() {
        // Otherwise the first outage bills the whole measured stretch on
        // top of the measurement that already covered it.
        let measured = DbEnergyState::new();
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);
        for t in [10_000_u64, 20_000, 30_000] {
            measured.add_window_kwh(1e-6, t);
            take_broker_energy(Some(&measured), Some(&state), t, 60_000);
        }

        // Alumet stale at t=100_000: only the 70 s outage may be billed.
        let (_, d) = take_broker_energy(Some(&measured), Some(&state), 100_000, 10_000);
        let billed = d.expect("the fallback covers the outage");
        let outage_kwh = cfg.cluster_watts() * 70_000.0 / 3_600_000.0 / 1000.0;
        assert!(
            (billed - outage_kwh).abs() < 1e-12,
            "billed {billed} kWh, expected the 70 s outage alone"
        );
    }

    #[test]
    fn a_fallback_window_carries_the_declared_tag_and_region() {
        // Base built as if [green.alumet.broker] were configured: alumet
        // tag and alumet-declared region.
        let mut broker = score::carbon::DbEnergyContext {
            window_kwh: 0.0,
            region: Some("eu-west-1".to_string()),
            model: score::carbon::CO2_MODEL_ALUMET,
        };
        let cfg = declared_cfg(1);

        patch_broker_energy(&mut broker, None, Some((4.2e-6, &cfg)));
        assert_eq!(
            broker.model,
            crate::report::BROKER_WASTE_MODEL_SPECPOWER,
            "a fallback window must not be published as a measurement"
        );
        assert_eq!(broker.region.as_deref(), Some("eu-west-3"));
        assert!((broker.window_kwh - 4.2e-6).abs() < 1e-18);
    }

    #[test]
    fn a_measured_window_keeps_its_tag_when_both_sources_deliver() {
        let mut broker = score::carbon::DbEnergyContext {
            window_kwh: 0.0,
            region: Some("eu-west-1".to_string()),
            model: crate::report::BROKER_WASTE_MODEL_SPECPOWER,
        };
        let cfg = declared_cfg(1);

        patch_broker_energy(&mut broker, Some(5e-6), Some((9e-6, &cfg)));
        assert_eq!(broker.model, score::carbon::CO2_MODEL_ALUMET);
        assert_eq!(broker.region.as_deref(), Some("eu-west-1"));
        assert!((broker.window_kwh - 5e-6).abs() < 1e-18);
    }

    #[test]
    fn build_tick_ctx_falls_back_to_the_declared_cluster() {
        // Covers the wiring, not the arbitration: a declared cluster alone
        // must leave the borrowed fast path and reach patch_broker_energy.
        let base = Arc::new(score::carbon::CarbonContext {
            broker_energy: Some(score::carbon::DbEnergyContext {
                window_kwh: 0.0,
                region: None,
                ..Default::default()
            }),
            ..score::carbon::CarbonContext::default()
        });
        let declared = declared_cfg(3);
        let declared_state = score::broker_static::StaticBrokerState::new(0, &declared);
        let mut sources = no_scrapers(&base);
        sources.static_broker = Some((&declared, &declared_state));

        let ctx = build_tick_ctx(&sources, 60_000);
        assert!(
            matches!(ctx, std::borrow::Cow::Owned(_)),
            "a declared cluster alone must leave the borrowed fast path"
        );
        let broker = ctx.broker_energy.as_ref().expect("broker context");
        assert!(broker.window_kwh > 0.0);
        assert_eq!(broker.model, crate::report::BROKER_WASTE_MODEL_SPECPOWER);
    }

    #[test]
    fn build_tick_ctx_keeps_the_fast_path_on_a_sub_second_tick() {
        // MIN_BILLABLE_MS accrues rather than bills, which is what keeps a
        // busy daemon off the CarbonContext clone.
        let base = Arc::new(score::carbon::CarbonContext {
            broker_energy: Some(score::carbon::DbEnergyContext::default()),
            ..score::carbon::CarbonContext::default()
        });
        let declared = declared_cfg(3);
        let declared_state = score::broker_static::StaticBrokerState::new(0, &declared);
        let mut sources = no_scrapers(&base);
        sources.static_broker = Some((&declared, &declared_state));

        let ctx = build_tick_ctx(&sources, 200);
        assert!(matches!(ctx, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn a_scrape_without_the_broker_label_hands_over_to_the_declaration() {
        // mark_alive fires on every successful scrape, label or not. A
        // mistyped label_value must not suppress the fallback forever.
        let measured = DbEnergyState::new();
        measured.mark_alive(10_000);
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);

        let (m, d) = take_broker_energy(Some(&measured), Some(&state), 10_000, 60_000);
        assert!(m.is_none(), "no sample ever carried the label");
        assert!(
            d.is_some_and(|k| k > 0.0),
            "the declaration must cover a workload nothing measured"
        );
    }

    #[test]
    fn a_vanished_label_still_delivers_what_it_measured() {
        // The cgroup is renamed away, so the series stops. Whatever it
        // banked before that is real and must not be stranded.
        let measured = DbEnergyState::new();
        measured.add_window_kwh(4e-6, 10_000);
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);
        // The scraper still answers, so liveness stays fresh; only the
        // labelled sample is gone.
        measured.mark_alive(100_000);

        let (m, d) = take_broker_energy(Some(&measured), Some(&state), 100_000, 10_000);
        assert_eq!(m, Some(4e-6), "banked measured energy must be delivered");
        assert!(d.is_none(), "the declaration does not bill the same window");

        // Nothing left to deliver, so the next window is the fallback's.
        let (m2, d2) = take_broker_energy(Some(&measured), Some(&state), 110_000, 10_000);
        assert!(m2.is_none());
        assert!(d2.is_some_and(|k| k > 0.0));
    }

    #[test]
    fn an_unscraped_state_does_not_own_the_timeline_at_boot() {
        // last_sample_ms and monotonic_ms() both start at 0, so an elapsed
        // check alone reads fresh for the first staleness window.
        let measured = DbEnergyState::new();
        let cfg = declared_cfg(3);
        let state = score::broker_static::StaticBrokerState::new(0, &cfg);

        let (m, d) = take_broker_energy(Some(&measured), Some(&state), 5_000, 60_000);
        assert!(m.is_none());
        assert!(
            d.is_some_and(|k| k > 0.0),
            "a state that never saw a scrape must not suppress the fallback"
        );
    }

    #[test]
    fn build_tick_ctx_alumet_overrides_scaphandre_for_same_service() {
        // The one genuinely new precedence edge: Alumet sits above
        // Scaphandre, so a service measured by both must carry Alumet's
        // coefficient and tag. Guards the insertion order in
        // `build_tick_ctx` (reverse precedence, Alumet inserted last).
        let base = Arc::new(score::carbon::CarbonContext::default());
        let alumet = AlumetState::new();
        alumet.insert_for_test("svc-a".into(), 1e-7, 100);
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 9e-7, 100);
        scaph.insert_for_test("svc-s".into(), 3e-7, 100);
        let mut sources = no_scrapers(&base);
        sources.alumet_state = Some(&alumet);
        sources.alumet_staleness_ms = 500;
        sources.scaphandre_state = Some(&scaph);
        sources.scaphandre_staleness_ms = 500;
        let ctx = build_tick_ctx(&sources, score::scaphandre::monotonic_ms());
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 2);
        // svc-a: Alumet wins over Scaphandre.
        assert_eq!(snap["svc-a"].model_tag, "alumet_rapl");
        assert!((snap["svc-a"].energy_per_op_kwh - 1e-7).abs() < 1e-15);
        // svc-s: Scaphandre-only entry survives.
        assert_eq!(snap["svc-s"].model_tag, "scaphandre_rapl");
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
            alumet_state: None,
            alumet_db_state: None,
            alumet_broker_state: None,
            static_broker: None,
            alumet_staleness_ms: 0,
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
            traces_store: Arc::new(crate::daemon::traces_store::TracesStore::new(0, 0)),
            green_enabled: true,
            per_service_labels: true,

            confidence: Confidence::DaemonStaging,
            metrics: metrics.clone(),
            findings_store: findings_store.clone(),
            hub_export: None,
            correlator: None,
            green_summary_cell: green_summary_cell.clone(),
            archive_tx: None,
            waste_sticky_ttl_ms: 0,
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

    #[tokio::test]
    async fn correlator_pair_evictions_recorded_in_metrics() {
        let metrics = MetricsState::new();
        let carbon = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        let cell = fresh_green_cell();
        // Cap of 1 pair: three same-batch N+1 findings from three
        // services create three cross-service pairs, forcing evictions.
        let correlator = Mutex::new(detect::correlate_cross::CrossTraceCorrelator::new(
            detect::correlate_cross::CorrelationConfig {
                enabled: true,
                max_tracked_pairs: 1,
                lag_threshold_ms: 100_000,
                min_co_occurrences: 1,
                min_confidence: 0.0,
                ..Default::default()
            },
        ));
        let mut ctx = test_ctx(&detect_config, &carbon, &metrics, &store, true, &cell);
        ctx.correlator = Some(&correlator);

        let traces: Vec<_> = ["svc-a", "svc-b", "svc-c"]
            .iter()
            .enumerate()
            .map(|(i, svc)| {
                let trace_id = format!("t{i}");
                let events: Vec<_> = (1..=6)
                    .map(|p| {
                        make_normalized_for_service(
                            &trace_id,
                            svc,
                            &format!("SELECT * FROM order_item WHERE order_id = {p}"),
                        )
                    })
                    .collect();
                (trace_id, events)
            })
            .collect();

        process_traces(traces, ctx).await;

        assert!(
            metrics.correlator_pairs_evicted_total.get() > 0,
            "pair cap evictions must reach the metric"
        );
    }

    #[test]
    fn service_meter_overflow_counts_unattributed_ops() {
        let metrics = MetricsState::new();
        let mut meter = ServiceMeter::new(2);

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
        assert!(meter.capped.warned);
    }

    #[test]
    fn ingest_service_meter_normalizes_anonymous_and_reserved_names() {
        let metrics = MetricsState::new();
        let mut meter = ServiceMeter::new(2);

        // Anonymous spans count under the OTLP default, not `service=""`.
        meter.record("", &metrics);
        let unknown = metrics
            .service_io_ops_total
            .with_label_values(&["unknown"])
            .get();
        assert!((unknown - 1.0).abs() < f64::EPSILON);
        let empty = metrics.service_io_ops_total.with_label_values(&[""]).get();
        assert!(empty.abs() < f64::EPSILON);

        // `_other` is reserved: counted, but never taking a cap slot.
        meter.record(SERVICE_OVERFLOW_LABEL, &metrics);
        assert_eq!(meter.capped.admitted.len(), 1);
        let other = metrics
            .service_io_ops_total
            .with_label_values(&[SERVICE_OVERFLOW_LABEL])
            .get();
        assert!((other - 1.0).abs() < f64::EPSILON);
        assert_eq!(metrics.service_io_ops_overflow_total.get(), 0);
    }

    #[tokio::test]
    async fn service_analyzed_io_ops_sums_to_global_counter() {
        let trace_for = |trace_id: &str, service: &str, n: usize| {
            let events: Vec<_> = (1..=n)
                .map(|i| {
                    make_normalized_for_service(
                        trace_id,
                        service,
                        &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    )
                })
                .collect();
            (trace_id.to_string(), events)
        };
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        let cell = fresh_green_cell();
        process_traces(
            vec![trace_for("t1", "svc-a", 6), trace_for("t2", "svc-b", 2)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
        )
        .await;

        let per_service = |s: &str| {
            metrics
                .service_analyzed_io_ops_total
                .with_label_values(&[s])
                .get()
        };
        assert!((per_service("svc-a") - 6.0).abs() < f64::EPSILON);
        assert!((per_service("svc-b") - 2.0).abs() < f64::EPSILON);
        let summed =
            per_service("svc-a") + per_service("svc-b") + per_service(SERVICE_OVERFLOW_LABEL);
        assert!((metrics.total_io_ops.get() - summed).abs() < f64::EPSILON);
    }

    #[test]
    fn analysis_service_meter_folds_past_cap_into_other() {
        let metrics = MetricsState::new();
        let mut meter = AnalysisServiceMeter::new(true, &metrics);
        meter.names.cap = 2;

        assert_eq!(meter.service_label("svc-a", &metrics), "svc-a");
        assert_eq!(meter.service_label("svc-b", &metrics), "svc-b");
        assert_eq!(
            meter.service_label("svc-c", &metrics),
            SERVICE_OVERFLOW_LABEL
        );
        // Already-admitted services keep their own label past the cap.
        assert_eq!(meter.service_label("svc-a", &metrics), "svc-a");
        assert_eq!(metrics.analysis_service_overflow_total.get(), 1);
        assert!(meter.names.warned);
    }

    #[test]
    fn analysis_service_meter_reserves_sentinel_names() {
        let metrics = MetricsState::new();
        let mut meter = AnalysisServiceMeter::new(true, &metrics);

        // A real service named like the fold bucket merges into it
        // without taking a cap slot or counting as overflow.
        assert_eq!(
            meter.service_label(SERVICE_OVERFLOW_LABEL, &metrics),
            SERVICE_OVERFLOW_LABEL
        );
        assert!(meter.names.admitted.is_empty());
        assert_eq!(metrics.analysis_service_overflow_total.get(), 0);

        // Anonymous spans never mint the knob-off sentinel `service=""`.
        assert_eq!(meter.service_label("", &metrics), "unknown");
        assert_eq!(meter.finding_label("", &metrics), "unknown");
        meter.hist_children("", &metrics)[0].observe(1.0);
        let empty_label_samples = metrics
            .slow_duration_seconds
            .with_label_values(&["sql", ""])
            .get_sample_count();
        assert_eq!(empty_label_samples, 0);
        let unknown_samples = metrics
            .slow_duration_seconds
            .with_label_values(&["sql", "unknown"])
            .get_sample_count();
        assert_eq!(unknown_samples, 1);
    }

    #[test]
    fn histogram_meter_folds_past_cap_into_other() {
        let metrics = MetricsState::new();
        let mut meter = AnalysisServiceMeter::new(true, &metrics);
        meter.hist_names.cap = 1;

        meter.hist_children("svc-a", &metrics)[0].observe(1.0);
        meter.hist_children("svc-b", &metrics)[0].observe(1.0);

        assert_eq!(metrics.slow_duration_service_overflow_total.get(), 1);
        let sample_count = |service: &str| {
            metrics
                .slow_duration_seconds
                .with_label_values(&["sql", service])
                .get_sample_count()
        };
        assert_eq!(sample_count("svc-a"), 1);
        assert_eq!(sample_count(SERVICE_OVERFLOW_LABEL), 1);
    }

    #[test]
    fn per_service_labels_off_uses_empty_label() {
        let metrics = MetricsState::new();
        let mut meter = AnalysisServiceMeter::new(false, &metrics);

        // Findings and histogram series carry the empty value...
        assert_eq!(meter.finding_label("svc-a", &metrics), "");
        meter.hist_children("svc-a", &metrics)[0].observe(1.0);
        let unlabeled = metrics
            .slow_duration_seconds
            .with_label_values(&["sql", ""])
            .get_sample_count();
        assert_eq!(unlabeled, 1);
        // ...while the avoidable counter's labels ignore the knob.
        assert_eq!(meter.service_label("svc-a", &metrics), "svc-a");
        assert_eq!(metrics.analysis_service_overflow_total.get(), 0);
    }

    #[tokio::test]
    async fn service_avoidable_io_ops_sums_to_global_counter() {
        let trace_for = |trace_id: &str, service: &str| {
            let events: Vec<_> = (1..=6)
                .map(|i| {
                    make_normalized_for_service(
                        trace_id,
                        service,
                        &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    )
                })
                .collect();
            (trace_id.to_string(), events)
        };
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        let cell = fresh_green_cell();
        process_traces(
            vec![trace_for("t1", "svc-a"), trace_for("t2", "svc-b")],
            test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
        )
        .await;

        let global = metrics.avoidable_io_ops.get();
        assert!(global > 0.0, "fixture should produce avoidable I/O");
        let summed: f64 = ["svc-a", "svc-b", SERVICE_OVERFLOW_LABEL]
            .iter()
            .map(|s| {
                metrics
                    .service_avoidable_io_ops_total
                    .with_label_values(&[s])
                    .get()
            })
            .sum();
        assert!((global - summed).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn shed_traces_are_excluded_from_analysis_outputs() {
        let metrics = Arc::new(MetricsState::new());
        let store = Arc::new(findings_store::FindingsStore::new(100));
        let cell = fresh_green_cell();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);

        // Capacity-1 queue with the worker not yet started: the first
        // batch queues, the second is shed before analysis ever runs.
        let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(1);
        let n_plus_one_events = |trace_id: &str| -> Vec<normalize::NormalizedEvent> {
            (1..=6)
                .map(|i| {
                    make_normalized(
                        trace_id,
                        &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    )
                })
                .collect()
        };
        enqueue_for_analysis(
            vec![("kept".to_string(), n_plus_one_events("kept"))],
            &sources,
            &work_tx,
            &metrics,
        );
        enqueue_for_analysis(
            vec![("shed".to_string(), n_plus_one_events("shed"))],
            &sources,
            &work_tx,
            &metrics,
        );
        assert_eq!(metrics.analysis_shed_batches_total.get(), 1);
        assert_eq!(metrics.analysis_shed_traces_total.get(), 1);

        let worker = tokio::spawn(run_analysis_worker(
            work_rx,
            test_worker_ctx(&metrics, &store, &cell),
        ));
        drop(work_tx);
        worker.await.expect("worker should drain and exit");

        // Only the kept trace was analyzed, and only it reached the
        // findings store. The shed trace left no output anywhere.
        assert!((metrics.traces_analyzed_total.get() - 1.0).abs() < f64::EPSILON);
        assert!(
            !store.by_trace_id("kept").await.is_empty(),
            "kept trace must reach the findings store"
        );
        assert!(
            store.by_trace_id("shed").await.is_empty(),
            "shed trace must never reach analysis outputs"
        );
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

        drain_to_worker_and_join(&window, Vec::new(), &sources, work_tx, worker, &metrics).await;

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
                alumet: None,
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
            per_service_labels: true,
            waste_sticky_ttl_ms: 0,
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
        let (_tx, mut rx) = mpsc::channel::<super::super::IngestBatch>(16);
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

        let (_tx, mut rx) = mpsc::channel::<super::super::IngestBatch>(16);
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

    #[tokio::test]
    async fn graceful_shutdown_ingests_queued_root_context_before_final_drain() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

        let metrics = Arc::new(MetricsState::new());
        let store = Arc::new(findings_store::FindingsStore::new(100));
        let cell = fresh_green_cell();
        let base = Arc::new(empty_carbon_ctx());
        let sources = no_scrapers(&base);
        let window = test_window();
        let trace_id = "09".repeat(16);
        let root_span_id = "01".repeat(8);
        {
            let mut w = window.lock().await;
            for span_id in ["child-1", "child-2", "child-3"] {
                w.push(
                    make_normalized_messaging(&trace_id, span_id, &root_span_id, "orders"),
                    current_time_ms(),
                );
            }
        }

        let (tx, mut rx) = mpsc::channel(1);
        let service =
            crate::ingest::otlp::OtlpGrpcService::new_daemon_with_grouping(tx, None, Vec::new());
        let shutdown_fut = async move {
            service
                .export(tonic::Request::new(otlp_request(
                    "orders-svc",
                    vec![otlp_server_root(1, "/api/shutdown")],
                )))
                .await
                .expect("root context acknowledged before shutdown");
        };
        let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(4);
        let worker = tokio::spawn(run_analysis_worker(
            work_rx,
            test_worker_ctx(&metrics, &store, &cell),
        ));
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
            shutdown_fut,
        )
        .await;

        assert!(result.is_ok());
        let findings = store.by_trace_id(&trace_id).await;
        assert_eq!(findings.len(), 1);
        let finding = &findings[0].finding;
        assert_eq!(finding.finding_type, detect::FindingType::SlowMessaging);
        assert_eq!(finding.pattern.occurrences, 3);
        assert_eq!(finding.source_endpoint, "/api/shutdown");
        assert!(metrics.events_processed_total.get().abs() < f64::EPSILON);
        assert!((metrics.traces_analyzed_total.get() - 1.0).abs() < f64::EPSILON);
    }
}
