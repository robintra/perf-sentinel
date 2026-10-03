//! Daemon main event loop: ingest batches, evict expired traces, and route
//! the resulting traces through detect + score + metrics + findings store.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::{Duration, interval};

use crate::correlate::Trace;
use crate::correlate::window::TraceWindow;
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
    /// Cross-batch slow window, `0` disables. From
    /// `[detection] slow_query_window_minutes`.
    pub(super) slow_window_ms: u64,
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
    /// Whether the same series and the per-service I/O counters carry a
    /// `grouping` label. From `[daemon] per_grouping_labels`.
    pub(super) per_grouping_labels: bool,
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
/// scraper readings are sampled then, not when the worker runs.
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
    per_grouping_labels: bool,
    confidence: Confidence,
    metrics: Arc<MetricsState>,
    findings_store: Arc<findings_store::FindingsStore>,
    hub_export: Option<Arc<HubExportBuffer>>,
    traces_store: Arc<super::traces_store::TracesStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    green_summary_cell: Arc<RwLock<GreenSummary>>,
    archive_tx: Option<mpsc::Sender<super::archive::OwnedArchive>>,
    waste_sticky_ttl_ms: u64,
    slow_window_ms: u64,
    /// Slow spans within this of an episode's first span count as one episode.
    slow_episode_ms: u64,
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
    // long analysis pass cannot stall ingestion (rx) or TTL eviction
    // (ticker). One channel, one worker, FIFO: the stateful
    // cross-trace correlator still sees a deterministic batch sequence.
    let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(loop_cfg.analysis_queue_capacity);
    let worker = tokio::spawn(run_analysis_worker(
        work_rx,
        AnalysisWorkerCtx {
            detect_config: detect_config.clone(),
            green_enabled: loop_cfg.green_enabled,
            per_service_labels: loop_cfg.per_service_labels,
            per_grouping_labels: loop_cfg.per_grouping_labels,
            confidence: loop_cfg.confidence,
            metrics: metrics.clone(),
            findings_store,
            hub_export,
            traces_store,
            correlator,
            green_summary_cell,
            archive_tx,
            waste_sticky_ttl_ms: loop_cfg.waste_sticky_ttl_ms,
            slow_window_ms: loop_cfg.slow_window_ms,
            // Covers the TTL tick plus LRU eviction spread of one slow burst.
            slow_episode_ms: loop_cfg.evict_ms.saturating_mul(3).max(60_000),
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
/// `shutdown_fut` fires. Otherwise drains queued ingest and the window into
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
    let mut service_meter =
        ServiceMeter::new(MAX_SERVICE_CARDINALITY, loop_cfg.per_grouping_labels);

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
                // the process.
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
    let mut service_meter = AnalysisServiceMeter::new(
        wctx.per_service_labels,
        wctx.per_grouping_labels,
        &wctx.metrics,
    );
    let mut slow_window = (wctx.slow_window_ms > 0).then(|| {
        super::slow_window::SlowWindowTracker::new(
            wctx.slow_window_ms,
            wctx.slow_episode_ms,
            wctx.detect_config.slow_threshold_ms,
            wctx.detect_config.slow_min_occurrences,
        )
    });
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
                slow_window: slow_window.as_mut(),
            },
        )
        .await;
    }
}

/// Cardinality cap on `perf_sentinel_service_io_ops_total`. Shared with
/// the tuning advisor in `query_api` so its hint names the real cap.
pub(crate) const MAX_SERVICE_CARDINALITY: usize = 1024;

/// Cardinality cap on the analysis-side `service` labels
/// (`findings_total`, `service_avoidable_io_ops_total`,
/// `service_analyzed_io_ops_total`). Lower than
/// [`MAX_SERVICE_CARDINALITY`]: these series multiply by type/severity.
pub(crate) const MAX_ANALYSIS_SERVICE_CARDINALITY: usize = 128;

/// Cardinality cap on the slow-duration histogram's `service` label.
/// Lower still: a histogram costs 14 series per (type, service) pair
/// (11 buckets plus `+Inf`, `_sum` and `_count`).
pub(crate) const MAX_HISTOGRAM_SERVICE_CARDINALITY: usize = 64;

/// Cap on admitted (service, grouping) pairs of
/// `perf_sentinel_service_io_ops_total`, the second gate after
/// [`MAX_SERVICE_CARDINALITY`]. A pair past it keeps its service and
/// folds its grouping into `_other`, so a service's sum over its
/// groupings stays its pre-0.19 value. Capping pairs rather than
/// grouping values keeps the bound proportional to the series that
/// exist, and the binding term is services x groupings: 11 services in
/// 20 namespaces is 220 pairs, 100 services in 10 namespaces is 1000.
/// Worst case 4096 pairs plus `_other` and the empty value per
/// admitted service, about 6k series.
pub(crate) const MAX_GROUPING_PAIRS: usize = 4096;

/// Cap on admitted (service, grouping) pairs on the analysis side
/// (`findings_total`, `service_avoidable_io_ops_total`,
/// `service_analyzed_io_ops_total`). Findings multiply by 12 types x 3
/// severities, so 512 pairs plus two extra groupings per admitted
/// service bound the family near 28k series in the adversarial case.
/// The cap counts pairs: 11 services in 20 namespaces (220 pairs) never
/// folds, 100 services in 10 namespaces (1000 pairs) folds every pair
/// past the 512th.
pub(crate) const MAX_ANALYSIS_GROUPING_PAIRS: usize = 512;

/// Cap on admitted (service, grouping) pairs of the slow-duration
/// histogram, the lowest of the three: 14 series per (type, service,
/// grouping), 3 x 14 x (256 + 2 x 65) near 16k in the adversarial case.
pub(crate) const MAX_HISTOGRAM_GROUPING_PAIRS: usize = 256;

/// Label value that series of services or groupings past a cap fold
/// into, so the global sums stay exact while cardinality stays bounded.
const SERVICE_OVERFLOW_LABEL: &str = "_other";

/// Admission policy shared by the label meters: a bounded value set, an
/// overflow counter, a one-shot warning. [`SERVICE_OVERFLOW_LABEL`] is
/// reserved and passes through without taking a slot, and so does the
/// empty value, the service blanked by `per_service_labels = false` on
/// the histogram path.
struct CappedServices {
    admitted: HashSet<String>,
    cap: usize,
    warned: bool,
    /// Metric family named in the one-shot cap warning.
    what: &'static str,
    /// What happens past the cap, so the one-shot warning says whether
    /// the ops are dropped (ingest) or folded into `_other` (analysis).
    consequence: &'static str,
}

impl CappedServices {
    fn new(cap: usize, what: &'static str, consequence: &'static str) -> Self {
        Self {
            admitted: HashSet::new(),
            cap,
            warned: false,
            what,
            consequence,
        }
    }

    /// `Some(value)` while it has (or gets) a slot, `None` past the
    /// cap (counts the overflow, warns once).
    fn admit<'a>(&mut self, value: &'a str, overflow: &prometheus::IntCounter) -> Option<&'a str> {
        if value.is_empty() || value == SERVICE_OVERFLOW_LABEL || self.admitted.contains(value) {
            return Some(value);
        }
        if self.admitted.len() < self.cap {
            self.admitted.insert(value.to_string());
            return Some(value);
        }
        overflow.inc();
        if !self.warned {
            tracing::warn!(
                cap = self.cap,
                what = self.what,
                "label cardinality cap reached, {}",
                self.consequence
            );
            self.warned = true;
        }
        None
    }
}

/// Admission policy for the `grouping` axis: a bounded set of admitted
/// (service, grouping) pairs keyed on the effective service label, an
/// overflow counter and a one-shot warning. The service axis is capped
/// first by [`CappedServices`]. This pair cap is the second gate. A pair
/// past the cap keeps its service and folds its grouping, so per-service
/// sums stay exact. [`SERVICE_OVERFLOW_LABEL`] and the empty value pass
/// through without a slot, as they do for services. Where a meter also
/// keeps a child cache keyed on the same pairs, the admitted set
/// duplicates the cache's keys and only `len` gates. The set keeps
/// `len` exact where a folded service re-hits an admitted pair (the
/// histogram) and where no cache exists (findings).
struct CappedPairs {
    admitted: HashMap<String, HashSet<String>>,
    len: usize,
    cap: usize,
    warned: bool,
    /// Metric family named in the one-shot cap warning.
    what: &'static str,
}

impl CappedPairs {
    fn new(cap: usize, what: &'static str) -> Self {
        Self {
            admitted: HashMap::new(),
            len: 0,
            cap,
            warned: false,
            what,
        }
    }

    /// `Some(grouping)` while the pair has (or gets) a slot, `None` past
    /// the cap (counts the overflow, warns once): the caller then folds
    /// the grouping half and keeps the service.
    fn admit<'a>(
        &mut self,
        service: &str,
        grouping: &'a str,
        overflow: &prometheus::IntCounter,
    ) -> Option<&'a str> {
        if grouping.is_empty()
            || grouping == SERVICE_OVERFLOW_LABEL
            || self
                .admitted
                .get(service)
                .is_some_and(|groupings| groupings.contains(grouping))
        {
            return Some(grouping);
        }
        if self.len < self.cap {
            self.admitted
                .entry(service.to_string())
                .or_default()
                .insert(grouping.to_string());
            self.len += 1;
            return Some(grouping);
        }
        overflow.inc();
        if !self.warned {
            tracing::warn!(
                cap = self.cap,
                what = self.what,
                "grouping pair cap reached, new (service, grouping) pairs fold into grouping=\"_other\""
            );
            self.warned = true;
        }
        None
    }
}

/// Per-service I/O op counter cache over [`CappedServices`]. Caps
/// cardinality against hostile `service.name` floods and caches the
/// labeled children so the hit path is two `HashMap` lookups plus an
/// atomic add. An event whose grouping folded past its cap misses that
/// path and pays the three admission probes plus the overflow increment
/// on every occurrence. Ingest drops past the service cap: the overflow
/// counter moves on every unattributed op. The grouping axis is capped
/// on (service, grouping) pairs and folds into [`SERVICE_OVERFLOW_LABEL`]
/// instead, so a service's sum over its groupings stays exact for the
/// energy scrapers.
struct ServiceMeter {
    per_grouping_labels: bool,
    /// Children per admitted service. Two levels rather than a
    /// `(String, String)` key: std's `HashMap` cannot probe a tuple of
    /// `String`s with borrowed `&str`s, and the per-event hit path must
    /// not allocate.
    children: HashMap<String, ServiceChildren>,
    capped: CappedServices,
    pairs: CappedPairs,
}

/// What one admitted service owns: when it was last heard from, and one
/// I/O op counter per effective grouping. The gauge sits here rather
/// than in a second map so one probe of `children` serves both.
struct ServiceChildren {
    last_span: prometheus::Gauge,
    per_grouping: HashMap<String, prometheus::Counter>,
}

impl ServiceMeter {
    fn new(cap: usize, per_grouping_labels: bool) -> Self {
        Self {
            per_grouping_labels,
            children: HashMap::new(),
            capped: CappedServices::new(
                cap,
                "ingest I/O ops",
                "new services get no per-service I/O op counter",
            ),
            pairs: CappedPairs::new(MAX_GROUPING_PAIRS, "ingest I/O ops grouping pairs"),
        }
    }

    /// `now_secs` is the batch's own stamp, taken once by the caller: the
    /// gauge answers to the millisecond either way, and a per-event clock
    /// read would be paid on the hot path for nothing.
    fn record(&mut self, service: &str, grouping: &str, metrics: &MetricsState, now_secs: f64) {
        let grouping = if self.per_grouping_labels {
            grouping
        } else {
            ""
        };
        if let Some(entry) = self.children.get(service) {
            entry.last_span.set(now_secs);
            if let Some(child) = entry.per_grouping.get(grouping) {
                child.inc();
                return;
            }
        }
        let Some(service) = self
            .capped
            .admit(service, &metrics.service_io_ops_overflow_total)
        else {
            return;
        };
        let grouping = self
            .pairs
            .admit(
                service,
                grouping,
                &metrics.service_io_ops_grouping_overflow_total,
            )
            .unwrap_or(SERVICE_OVERFLOW_LABEL);
        // Past the pair cap the label is `_other`, minted on the first
        // fold: no `String` per folded event.
        if let Some(entry) = self.children.get(service)
            && let Some(child) = entry.per_grouping.get(grouping)
        {
            child.inc();
            return;
        }
        let entry = self
            .children
            .entry(service.to_string())
            .or_insert_with(|| ServiceChildren {
                last_span: metrics
                    .service_last_span_timestamp_seconds
                    .with_label_values(&[service]),
                per_grouping: HashMap::new(),
            });
        entry.last_span.set(now_secs);
        entry
            .per_grouping
            .entry(grouping.to_string())
            .or_insert_with(|| {
                metrics
                    .service_io_ops_total
                    .with_label_values(&[service, grouping])
            })
            .inc();
    }
}

/// The three slow-duration histogram children of one (service, grouping)
/// label pair.
/// Named rather than a positional array: an index-to-`EventType` map
/// is one reorder away from filing every duration under the wrong
/// `type`.
struct SlowHists {
    sql: prometheus::Histogram,
    http_out: prometheus::Histogram,
    messaging: prometheus::Histogram,
}

impl SlowHists {
    fn mint(service: &str, grouping: &str, metrics: &MetricsState) -> Self {
        let child = |kind: &str| {
            metrics
                .slow_duration_seconds
                .with_label_values(&[kind, service, grouping])
        };
        Self {
            sql: child("sql"),
            http_out: child("http_out"),
            messaging: child("messaging"),
        }
    }

    fn for_type(&self, event_type: &crate::event::EventType) -> &prometheus::Histogram {
        match event_type {
            crate::event::EventType::Sql => &self.sql,
            crate::event::EventType::HttpOut => &self.http_out,
            crate::event::EventType::Messaging => &self.messaging,
        }
    }
}

/// Caps the `service` and `grouping` labels on the analysis-side
/// metrics. Single-owner state of the analysis worker task, like
/// [`ServiceMeter`]: no lock. Past a cap, series fold into
/// [`SERVICE_OVERFLOW_LABEL`] on that axis so sums stay exact: the
/// service by value, then the grouping by admitted (service, grouping)
/// pair keyed on the effective service, so a fleet under the pair cap
/// never folds a grouping. With `[daemon] per_service_labels = false`,
/// findings and histogram series carry an empty `service` (the
/// per-service I/O counters ignore that knob). With
/// `per_grouping_labels = false` every family carries an empty
/// `grouping`.
struct AnalysisServiceMeter {
    per_service_labels: bool,
    per_grouping_labels: bool,
    names: CappedServices,
    pairs: CappedPairs,
    hist_names: CappedServices,
    hist_pairs: CappedPairs,
    /// Histogram children per effective (service, grouping) pair, so the
    /// hit path is two `HashMap` lookups and no allocation.
    hist_children: HashMap<String, HashMap<String, SlowHists>>,
}

impl AnalysisServiceMeter {
    /// With both knobs off there is a single histogram label pair, so it
    /// is materialized up front and "series absent" keeps meaning "worker
    /// not running" rather than "no slow span yet". With either knob on
    /// nothing is pre-warmed: the only values known before traffic
    /// arrives are [`SERVICE_OVERFLOW_LABEL`] on the labeled axes.
    /// Minting them would publish a permanent `_other` series on a daemon
    /// that never hit a cap, which reads as overflow and shows up in the
    /// dashboard's pickers.
    fn new(per_service_labels: bool, per_grouping_labels: bool, metrics: &MetricsState) -> Self {
        let mut meter = Self {
            per_service_labels,
            per_grouping_labels,
            names: CappedServices::new(
                MAX_ANALYSIS_SERVICE_CARDINALITY,
                "analysis service labels",
                "new services fold into service=\"_other\"",
            ),
            pairs: CappedPairs::new(MAX_ANALYSIS_GROUPING_PAIRS, "analysis grouping pairs"),
            hist_names: CappedServices::new(
                MAX_HISTOGRAM_SERVICE_CARDINALITY,
                "slow-duration histogram",
                "new services fold into service=\"_other\"",
            ),
            hist_pairs: CappedPairs::new(
                MAX_HISTOGRAM_GROUPING_PAIRS,
                "slow-duration histogram grouping pairs",
            ),
            hist_children: HashMap::new(),
        };
        if !per_service_labels && !per_grouping_labels {
            meter
                .hist_children
                .entry(String::new())
                .or_default()
                .insert(String::new(), SlowHists::mint("", "", metrics));
        }
        meter
    }

    /// Effective label under the shared analysis cap: the service, or
    /// [`SERVICE_OVERFLOW_LABEL`] past it.
    fn service_label<'a>(&mut self, service: &'a str, metrics: &MetricsState) -> &'a str {
        self.names
            .admit(service, &metrics.analysis_service_overflow_total)
            .unwrap_or(SERVICE_OVERFLOW_LABEL)
    }

    /// Effective `grouping` label for an already-resolved `service`,
    /// under the analysis pair cap: empty with the knob off, the value,
    /// or [`SERVICE_OVERFLOW_LABEL`] once the pair cap is full.
    fn grouping_half<'a>(
        &mut self,
        service: &str,
        grouping: &'a str,
        metrics: &MetricsState,
    ) -> &'a str {
        if !self.per_grouping_labels {
            return "";
        }
        self.pairs
            .admit(service, grouping, &metrics.analysis_grouping_overflow_total)
            .unwrap_or(SERVICE_OVERFLOW_LABEL)
    }

    /// Label pair for the per-service I/O counters: the service under
    /// its own cap, then the grouping under the pair cap keyed on that
    /// effective service.
    fn pair_labels<'a>(
        &mut self,
        service: &'a str,
        grouping: &'a str,
        metrics: &MetricsState,
    ) -> (&'a str, &'a str) {
        let service = self.service_label(service, metrics);
        (service, self.grouping_half(service, grouping, metrics))
    }

    /// Label pair for `findings_total`. The pair is keyed on the effective
    /// service whatever the knob says, so a finding and its I/O counter
    /// rows share one slot. Only the emitted service half is blanked with
    /// `per_service_labels` off.
    fn finding_labels<'a>(
        &mut self,
        service: &'a str,
        grouping: &'a str,
        metrics: &MetricsState,
    ) -> (&'a str, &'a str) {
        let service = self.service_label(service, metrics);
        let grouping = self.grouping_half(service, grouping, metrics);
        (if self.per_service_labels { service } else { "" }, grouping)
    }

    /// Observe one slow span's duration on its (service, grouping)
    /// histogram, under the histogram's own caps. Observing here rather
    /// than handing back a `&SlowHists` keeps the hit path at two
    /// `HashMap` lookups: returning a reference out of a `&mut self`
    /// method forces a `contains_key` check followed by an index lookup.
    fn observe_slow(
        &mut self,
        service: &str,
        grouping: &str,
        event_type: &crate::event::EventType,
        seconds: f64,
        metrics: &MetricsState,
    ) {
        let service = if self.per_service_labels { service } else { "" };
        let grouping = if self.per_grouping_labels {
            grouping
        } else {
            ""
        };
        if let Some(hists) = self
            .hist_children
            .get(service)
            .and_then(|m| m.get(grouping))
        {
            hists.for_type(event_type).observe(seconds);
            return;
        }

        let service = self
            .hist_names
            .admit(service, &metrics.slow_duration_service_overflow_total)
            .unwrap_or(SERVICE_OVERFLOW_LABEL);
        let grouping = self
            .hist_pairs
            .admit(
                service,
                grouping,
                &metrics.slow_duration_grouping_overflow_total,
            )
            .unwrap_or(SERVICE_OVERFLOW_LABEL);
        // Past a cap the label is `_other`, minted on the first fold:
        // no `String` per folded span.
        if let Some(hists) = self
            .hist_children
            .get(service)
            .and_then(|m| m.get(grouping))
        {
            hists.for_type(event_type).observe(seconds);
            return;
        }
        self.hist_children
            .entry(service.to_string())
            .or_default()
            .entry(grouping.to_string())
            .or_insert_with(|| SlowHists::mint(service, grouping, metrics))
            .for_type(event_type)
            .observe(seconds);
    }
}

/// A batch's grouped endpoint context: route roots, parent links and consumer
/// destinations, each keyed by trace id. Every update writes a parent link,
/// so `parents` holds every trace id the other two do.
struct BatchSourceContext {
    roots: TraceSourceEndpointGroups<String>,
    parents: TraceSourceEndpointGroups<Option<String>>,
    consumers: TraceSourceEndpointGroups<String>,
}

/// Merge one trace's sampled endpoint context and collect any LRU eviction.
fn retain_source_endpoint_context(
    window: &mut TraceWindow,
    trace_id: &str,
    context: &BatchSourceContext,
    now_ms: u64,
    lru_evicted: &mut Vec<(String, Vec<normalize::NormalizedEvent>)>,
    source_endpoint_generations: &mut HashMap<String, u64>,
) {
    let empty = HashMap::new();
    if let Some(evicted) = window.retain_source_endpoint_context_groups(
        trace_id,
        context.roots.get(trace_id).unwrap_or(&empty),
        &context.parents[trace_id],
        context.consumers.get(trace_id).unwrap_or(&empty),
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
) -> BatchSourceContext {
    let mut roots: TraceSourceEndpointGroups<String> = HashMap::new();
    let mut parents: TraceSourceEndpointGroups<Option<String>> = HashMap::new();
    let mut consumers: TraceSourceEndpointGroups<String> = HashMap::new();
    for update in updates
        .into_iter()
        .filter(|update| should_sample(&update.trace_id, sampling_rate))
    {
        if let Some(consumer_endpoint) = update.consumer_endpoint {
            consumers
                .entry(update.trace_id.clone())
                .or_default()
                .entry(Arc::clone(&update.service))
                .or_default()
                .insert(update.span_id.clone(), consumer_endpoint);
        }
        if let Some(endpoint) = update.endpoint {
            roots
                .entry(update.trace_id.clone())
                .or_default()
                .entry(Arc::clone(&update.service))
                .or_default()
                .insert(update.span_id.clone(), endpoint);
        }
        // Unconditional, and last, so the common child span moves its
        // strings here instead of cloning them.
        parents
            .entry(update.trace_id)
            .or_default()
            .entry(update.service)
            .or_default()
            .insert(update.span_id, update.parent_span_id);
    }
    BatchSourceContext {
        roots,
        parents,
        consumers,
    }
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
    let now_ms = current_time_ms();
    #[allow(clippy::cast_precision_loss)] // epoch millis, exact in f64 until year 287396
    let now_secs = now_ms as f64 / 1000.0;
    for event in &normalized {
        service_meter.record(
            event.event.service.as_ref(),
            event.event.grouping_value().unwrap_or(""),
            metrics,
            now_secs,
        );
    }
    let source_context = group_source_endpoint_updates(source_endpoint_updates, sampling_rate);
    let mut lru_evicted = Vec::new();
    let mut source_endpoint_generations = HashMap::new();
    {
        // Each push performs at most the fixed ancestor-depth bound of lookups.
        // Payload and queue caps bound work held behind this lock.
        let mut w = window.lock().await;
        // Repair existing traces before a new context-only trace can evict them.
        // The second pass retains context that preceded the first I/O event.
        let existing_update_ids: Vec<_> = source_context
            .parents
            .keys()
            .filter(|trace_id| w.contains_trace(trace_id))
            .cloned()
            .collect();
        for trace_id in &existing_update_ids {
            retain_source_endpoint_context(
                &mut w,
                trace_id,
                &source_context,
                now_ms,
                &mut lru_evicted,
                &mut source_endpoint_generations,
            );
        }
        let missing_update_ids: Vec<_> = source_context
            .parents
            .keys()
            .filter(|trace_id| !w.contains_trace(trace_id))
            .cloned()
            .collect();
        for trace_id in &missing_update_ids {
            retain_source_endpoint_context(
                &mut w,
                trace_id,
                &source_context,
                now_ms,
                &mut lru_evicted,
                &mut source_endpoint_generations,
            );
        }
        for event in normalized {
            let trace_id = event.event.trace_id.as_str();
            if source_context.parents.contains_key(trace_id) {
                let expected_generation = source_endpoint_generations.get(trace_id).copied();
                if expected_generation.is_none()
                    || w.source_endpoint_generation(trace_id) != expected_generation
                {
                    retain_source_endpoint_context(
                        &mut w,
                        trace_id,
                        &source_context,
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
/// owned so it can travel to the worker. The energy sources are sampled
/// here, on the loop side at eviction time.
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
/// Synchronous and `try_reserve`-based: the select! loop never
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
// positional arguments: six of those would be mutually type-compatible
// `u64` staleness windows, so a mis-paired argument would compile
// silently and gate one backend's readings by another's staleness.
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

    // Fast path: with nothing fresh this tick, borrow base instead of cloning.
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
/// They are not obvious, so change this against that section, not
/// against intuition.
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

/// The tag and region follow the source that filled the window, so a
/// fallback tick is never published as a measurement.
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
/// (service, grouping) pair, so the per-span hit path stays two `HashMap`
/// lookups instead of the `MetricVec` label-hash + lock of
/// `with_label_values`.
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
                meter.observe_slow(
                    span.event.service.as_ref(),
                    span.event.grouping_value().unwrap_or(""),
                    &span.event.event_type,
                    span.event.duration_us as f64 / 1_000_000.0,
                    metrics,
                );
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
    meter: &mut AnalysisServiceMeter,
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
    // Window-scoped energy/carbon scalars for the Grafana Trends panels.
    // Per-service/region breakdown stays off /metrics (cardinality). The
    // totals are bounded and safe to expose as gauges.
    metrics.energy_kwh.set(green_summary.energy_kwh);
    metrics
        .carbon_gco2
        .set(green_summary.regions.iter().map(|r| r.co2_gco2).sum());

    // Per-(service, grouping) avoidable and analysed I/O ops: the two
    // series a per-service waste ratio divides, from the same scoring
    // pass and under the same caps. Both empty when green is off, like
    // the global avoidable counter. Both maps arrive ordered by
    // (service, grouping) from `score_green`, so cap admission is
    // deterministic and the overflow counters move once per row.
    for ((service, grouping), ops) in &green_summary.avoidable_per_service {
        let (service, grouping) = meter.pair_labels(service, grouping, metrics);
        metrics
            .service_avoidable_io_ops_total
            .with_label_values(&[service, grouping])
            .inc_by(*ops as f64);
    }
    for ((service, grouping), ops) in &green_summary.analyzed_per_service {
        let (service, grouping) = meter.pair_labels(service, grouping, metrics);
        metrics
            .service_analyzed_io_ops_total
            .with_label_values(&[service, grouping])
            .inc_by(*ops as f64);
    }

    // Resolve effective labels once: the counter and its exemplars must
    // land on the same series.
    let labeled: Vec<(&detect::Finding, &str, &str)> = findings
        .iter()
        .map(|f| {
            let (service, grouping) =
                meter.finding_labels(&f.service, f.grouping_value().unwrap_or(""), metrics);
            (f, service, grouping)
        })
        .collect();
    metrics.record_exemplars_labeled(&labeled, green_summary);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for (finding, service_label, grouping_label) in &labeled {
        metrics
            .findings_total
            .with_label_values(&[
                finding.finding_type.as_str(),
                finding.severity.as_str(),
                service_label,
                grouping_label,
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

/// Count slow spans refused by the slow window key cap, warning once per
/// process like [`record_correlator_evictions`].
fn record_slow_window_refusals(refused: usize, metrics: &MetricsState) {
    static CAP_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if refused == 0 {
        return;
    }
    metrics
        .slow_window_keys_refused_total
        .inc_by(refused as u64);
    if !CAP_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            refused,
            "[detection] slow_query_window_minutes key cap reached, new slow \
             templates are not counted (see \
             perf_sentinel_slow_window_keys_refused_total)"
        );
    }
}

/// Add the cross-batch slow findings of this batch, when the window is on.
fn observe_slow_window(
    traces: &[Trace],
    findings: &mut Vec<detect::Finding>,
    now_ms: u64,
    ctx: &mut ProcessTracesCtx<'_>,
) {
    let Some(tracker) = ctx.slow_window.as_deref_mut() else {
        return;
    };
    let (emitted, refused) = tracker.observe(traces, findings, now_ms);
    findings.extend(emitted);
    record_slow_window_refusals(refused, ctx.metrics);
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
) {
    if ctx.green_enabled {
        score::score_green(traces, findings, Some(ctx.carbon_ctx))
    } else {
        let total_io_ops = traces.iter().map(|t| t.spans.len()).sum();
        (findings, GreenSummary::disabled(total_io_ops), Vec::new())
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
    /// cycle and is filled only on batches where a scrape landed.
    msg_waste_sticky: &'a mut Option<(MessagingWaste, u64)>,
    waste_sticky_ttl_ms: u64,
    /// Worker-owned cross-batch slow window, `None` when disabled.
    slow_window: Option<&'a mut super::slow_window::SlowWindowTracker>,
}

/// Copy the batch summary onto the shared cell, with both waste figures
/// bridged over their scrape gaps. The per-window archive keeps the
/// batch-scoped truth. Only the live cell gets the TTL-bounded figures.
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

/// Stamps `confidence` on every finding after detection. The
/// value is derived from `config.daemon.environment` in `run()` and passed
/// here unchanged. `analyze` batch mode does not call this function. It
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

    let mut findings = detect::run_full_detection(&trace_structs, ctx.detect_config);
    let now_ms = current_time_ms();
    observe_slow_window(&trace_structs, &mut findings, now_ms, &mut ctx);

    record_slow_durations(
        &trace_structs,
        ctx.detect_config,
        ctx.metrics,
        ctx.service_meter,
    );

    // Keep `per_endpoint_io_ops` for the periodic-disclosure archive
    // (design doc 08), computed by `score_green`'s single pass along
    // with the per-service avoidable split on the summary.
    let (mut findings, green_summary, per_endpoint_io_ops) =
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
        ctx.metrics,
        ctx.service_meter,
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
            // Move owned data into the archive. The aggregator consumes
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
/// the timestamp as a monotonic-ish sort key. A single zero tick produces
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
mod tests;
