//! Prometheus metrics export for daemon mode.
//!
//! Exposes a `/metrics` endpoint on the same axum HTTP server (port 4318)
//! with counters and gauges for monitoring perf-sentinel in real time.
//! Supports `OpenMetrics` exemplars for click-through from Grafana to traces.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use prometheus::{
    Counter, CounterVec, Encoder, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    Opts, Registry, TextEncoder,
};

#[cfg(feature = "daemon")]
use crate::daemon::ack::AckAction;
use crate::report::Report;

/// Reason an OTLP request was rejected by the daemon.
///
/// Used as the `reason` label of `perf_sentinel_otlp_rejected_total`.
/// Variants are pre-warmed to 0 at startup so dashboards can plot
/// zero-values before any rejection occurs.
///
/// `payload_too_large` is intentionally absent: tower-http's
/// `RequestBodyLimitLayer` (HTTP) and tonic's `max_decoding_message_size`
/// (gRPC) reject oversized payloads before the application handler
/// runs. Operators concerned with payload size should monitor the
/// upstream proxy or wire a tower-http rejection counter in their
/// own stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtlpRejectReason {
    /// HTTP only: Content-Type is not `application/x-protobuf`.
    UnsupportedMediaType,
    /// HTTP only: protobuf decode failed.
    ParseError,
    /// HTTP and gRPC: the event channel is saturated or closed.
    ChannelFull,
}

impl OtlpRejectReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::ParseError => "parse_error",
            Self::ChannelFull => "channel_full",
        }
    }
}

/// Reason a daemon ack or unack operation failed.
///
/// Used as the `reason` label of
/// `perf_sentinel_ack_operations_failed_total`. Documented combinations
/// with `AckAction` are pre-warmed to 0 at startup so dashboards can
/// plot zero-values before the first failure occurs.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AckFailureReason {
    /// HTTP 409, action=ack only: signature is already acked, either by
    /// the daemon JSONL or by an active CI TOML baseline.
    AlreadyAcked,
    /// HTTP 404, action=unack only: signature has no active daemon ack.
    NotAcked,
    /// HTTP 401: `[daemon.ack] api_key` is set, request missing or
    /// has an invalid `X-API-Key` header.
    Unauthorized,
    /// HTTP 503: ack store disabled (`enabled = false`, or default
    /// storage path could not be resolved at startup).
    NoStore,
    /// HTTP 400: `{signature}` path segment fails canonical format
    /// validation.
    InvalidSignature,
    /// HTTP 507, action=ack only: `MAX_ACTIVE_ACKS` reached.
    LimitReached,
    /// HTTP 507, action=ack only: append would push the JSONL above
    /// `MAX_ACKS_FILE_BYTES` (per-daemon saturation, indicates
    /// compaction is needed at next restart or the cap should be
    /// raised).
    FileTooLarge,
    /// HTTP 507, action=ack only: a single record exceeds
    /// `MAX_ACK_ENTRY_BYTES` after serialization, typically because
    /// the caller-supplied `by` or `reason` field is oversized
    /// (per-request misuse, indicates client-side validation should
    /// be tightened).
    EntryTooLarge,
    /// HTTP 500: IO failure, serialization error, symlink refused,
    /// insecure permissions, or no default storage location at write
    /// time. Also absorbs `AckError::FileTooLarge` and
    /// `AckError::EntryTooLarge` on the unack path: the unack flow
    /// surfaces those two cases under `internal_error` with HTTP 500
    /// rather than HTTP 507, since the ack endpoints do not
    /// differentiate the cap on the unack write today.
    InternalError,
}

#[cfg(feature = "daemon")]
impl AckFailureReason {
    /// Stable Prometheus label string for this variant.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyAcked => "already_acked",
            Self::NotAcked => "not_acked",
            Self::Unauthorized => "unauthorized",
            Self::NoStore => "no_store",
            Self::InvalidSignature => "invalid_signature",
            Self::LimitReached => "limit_reached",
            Self::FileTooLarge => "file_too_large",
            Self::EntryTooLarge => "entry_too_large",
            Self::InternalError => "internal_error",
        }
    }
}

/// Build an `IntCounterVec` and register it on the given registry,
/// failing the daemon at startup on metric creation or registration
/// failure. Both branches are infallible in practice (label set is
/// static and registered once), so `expect` is the documented choice
/// across `MetricsState::new`.
fn register_int_counter_vec(
    registry: &Registry,
    name: &str,
    help: &str,
    labels: &[&str],
) -> IntCounterVec {
    let counter =
        IntCounterVec::new(Opts::new(name, help), labels).expect("metric creation should not fail");
    registry
        .register(Box::new(counter.clone()))
        .expect("registration should not fail");
    counter
}

/// Data attached to a metric as an `OpenMetrics` exemplar.
#[derive(Debug, Clone)]
struct ExemplarData {
    trace_id: String,
}

/// Sanitize a value for use in an `OpenMetrics` exemplar label.
///
/// Keeps only alphanumeric characters, `-`, and `_`. Truncates to 64 characters.
/// Prevents injection into the Prometheus text exposition format.
fn sanitize_exemplar_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

/// Shared metrics state for the daemon.
#[derive(Clone)]
pub struct MetricsState {
    registry: Registry,
    /// Findings detected, labeled by type and severity.
    pub findings_total: CounterVec,
    /// Cumulative I/O waste ratio since daemon start.
    /// Use Prometheus `rate()` on `total_io_ops` and `avoidable_io_ops` for windowed ratios.
    pub io_waste_ratio: Gauge,
    /// Total traces analyzed since daemon start.
    pub traces_analyzed_total: Counter,
    /// Total events processed since daemon start.
    pub events_processed_total: Counter,
    /// Currently active traces in the sliding window.
    pub active_traces: Gauge,
    /// Cumulative total I/O ops (for computing rolling waste ratio).
    pub total_io_ops: Counter,
    /// Cumulative avoidable I/O ops (for computing rolling waste ratio).
    pub avoidable_io_ops: Counter,
    /// cumulative I/O ops per service. Labeled with the
    /// `service` attribute from span `service.name`. Exposed so
    /// Grafana dashboards can show per-service throughput, and used
    /// by the Scaphandre scraper to compute per-service op deltas
    /// without running a parallel counter (see
    /// [`crate::score::scaphandre::OpsSnapshotDiff`]).
    pub service_io_ops_total: CounterVec,
    /// age in seconds since the last successful Scaphandre
    /// scrape. Reset to 0 on each successful scrape and incremented
    /// every scrape interval by the scraper task. Useful for
    /// Grafana alerts that detect a hung scraper. Stays at 0 when
    /// Scaphandre is not configured.
    pub scaphandre_last_scrape_age_seconds: Gauge,
    /// Age in seconds since the last successful cloud energy scrape.
    /// Same pattern as [`Self::scaphandre_last_scrape_age_seconds`].
    /// Stays at 0 when cloud energy is not configured.
    pub cloud_energy_last_scrape_age_seconds: Gauge,
    /// Duration histogram for spans exceeding the slow threshold, labeled
    /// by event type (`sql` or `http_out`). Enables accurate global
    /// percentile computation via `histogram_quantile()` across sharded
    /// daemon instances where cross-trace percentiles would otherwise be
    /// computed per-instance on a subset of traces.
    pub slow_duration_seconds: HistogramVec,
    /// Total requests to `GET /api/export/report` since daemon start.
    /// Bumped by the handler so operators can dashboard or alert on
    /// the frequency of Report snapshots being pulled by clients.
    /// Counts every request, including cold-start responses (200 with
    /// an empty envelope), consistent with HTTP access-log conventions.
    pub export_report_requests_total: Counter,
    /// OTLP requests rejected by the daemon, labeled by `reason`.
    /// Pre-warmed to 0 for the 3 reasons (`unsupported_media_type`,
    /// `parse_error`, `channel_full`) at startup so dashboards plot
    /// zero-values before the first rejection. The 3
    /// `otlp_rejected_*` `IntCounter` fields below cache the labeled
    /// children so the hot path (`record_otlp_reject`) avoids a label
    /// hashmap lookup per rejection. Lookup the vec directly only for
    /// scrape rendering and tests, not in the hot path.
    pub otlp_rejected_total: IntCounterVec,
    /// Cached child for `otlp_rejected_total{reason="unsupported_media_type"}`.
    pub otlp_rejected_unsupported_media_type: IntCounter,
    /// Cached child for `otlp_rejected_total{reason="parse_error"}`.
    pub otlp_rejected_parse_error: IntCounter,
    /// Cached child for `otlp_rejected_total{reason="channel_full"}`.
    /// Read directly by `daemon::query_api::collect_warning_details` to
    /// surface an `ingestion_drops` warning in the report payload when
    /// the counter is positive.
    pub otlp_rejected_channel_full: IntCounter,
    /// Successful ack and unack operations on the daemon HTTP API,
    /// labeled by `action` (`ack` or `unack`). Pre-warmed to 0 for
    /// both actions at startup so dashboards plot zero-values before
    /// the first operation. The 2 `ack_operations_*_success`
    /// `IntCounter` fields below cache the labeled children for the
    /// hot path.
    #[cfg(feature = "daemon")]
    pub ack_operations_total: IntCounterVec,
    /// Failed ack and unack operations on the daemon HTTP API, labeled
    /// by `action` and `reason`. Pre-warmed to 0 for the 13 documented
    /// reachable combinations (8 reasons on action=ack, 5 reasons on
    /// action=unack). Failures are by definition rare so no hot-path
    /// child caching, `with_label_values` is called per call site.
    #[cfg(feature = "daemon")]
    pub ack_operations_failed_total: IntCounterVec,
    /// Cached child for `ack_operations_total{action="ack"}`.
    #[cfg(feature = "daemon")]
    pub ack_operations_ack_success: IntCounter,
    /// Cached child for `ack_operations_total{action="unack"}`.
    #[cfg(feature = "daemon")]
    pub ack_operations_unack_success: IntCounter,
    /// Worst-case `trace_id` per (`finding_type`, severity) for exemplars.
    worst_finding_trace: Arc<RwLock<HashMap<(&'static str, &'static str), ExemplarData>>>,
    /// Worst-case `trace_id` for io waste ratio.
    worst_waste_trace: Arc<RwLock<Option<ExemplarData>>>,
}

impl MetricsState {
    /// Create a new metrics state with all metrics registered.
    ///
    /// # Panics
    ///
    /// Panics if prometheus metric creation or registration fails (should not happen).
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Self {
        let registry = Registry::new();

        let findings_total = CounterVec::new(
            Opts::new(
                "perf_sentinel_findings_total",
                "Total findings detected by type and severity",
            ),
            &["type", "severity"],
        )
        .expect("metric creation should not fail");

        let io_waste_ratio = Gauge::new(
            "perf_sentinel_io_waste_ratio",
            "Cumulative I/O waste ratio since daemon start",
        )
        .expect("metric creation should not fail");

        let traces_analyzed_total = Counter::new(
            "perf_sentinel_traces_analyzed_total",
            "Total traces analyzed since start",
        )
        .expect("metric creation should not fail");

        let events_processed_total = Counter::new(
            "perf_sentinel_events_processed_total",
            "Total events processed since start",
        )
        .expect("metric creation should not fail");

        let active_traces = Gauge::new(
            "perf_sentinel_active_traces",
            "Currently active traces in the sliding window",
        )
        .expect("metric creation should not fail");

        let total_io_ops = Counter::new(
            "perf_sentinel_total_io_ops",
            "Cumulative total I/O ops processed",
        )
        .expect("metric creation should not fail");

        let avoidable_io_ops = Counter::new(
            "perf_sentinel_avoidable_io_ops",
            "Cumulative avoidable I/O ops detected",
        )
        .expect("metric creation should not fail");

        // per-service I/O op counter. Single source of
        // truth for per-service op counts, the Scaphandre scraper
        // reads this via snapshot-diff instead of maintaining a
        // parallel counter that would drift under concurrent writes.
        let service_io_ops_total = CounterVec::new(
            Opts::new(
                "perf_sentinel_service_io_ops_total",
                "Cumulative I/O ops attributed to each service",
            ),
            &["service"],
        )
        .expect("metric creation should not fail");

        // Scaphandre scrape freshness gauge. 0 when a
        // successful scrape just completed; grows with wall-clock
        // time until the next success. Always 0 when Scaphandre is
        // not configured (the scraper task is the only writer).
        let scaphandre_last_scrape_age_seconds = Gauge::new(
            "perf_sentinel_scaphandre_last_scrape_age_seconds",
            "Age in seconds since the last successful Scaphandre scrape",
        )
        .expect("metric creation should not fail");

        registry
            .register(Box::new(findings_total.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(io_waste_ratio.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(traces_analyzed_total.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(events_processed_total.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(active_traces.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(total_io_ops.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(avoidable_io_ops.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(service_io_ops_total.clone()))
            .expect("registration should not fail");
        registry
            .register(Box::new(scaphandre_last_scrape_age_seconds.clone()))
            .expect("registration should not fail");

        // Histogram for slow span durations (seconds). Buckets cover the
        // typical range from 100ms to 30s. Prometheus aggregates these
        // across instances via histogram_quantile(), solving the
        // cross-trace percentile degradation in sharded deployments.
        let slow_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "perf_sentinel_slow_duration_seconds",
                "Duration of spans exceeding the slow threshold",
            )
            .buckets(vec![
                0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0, 30.0,
            ]),
            &["type"],
        )
        .expect("metric creation should not fail");
        registry
            .register(Box::new(slow_duration_seconds.clone()))
            .expect("registration should not fail");

        let cloud_energy_last_scrape_age_seconds = Gauge::new(
            "perf_sentinel_cloud_energy_last_scrape_age_seconds",
            "Age in seconds since the last successful cloud energy scrape",
        )
        .expect("metric creation should not fail");
        registry
            .register(Box::new(cloud_energy_last_scrape_age_seconds.clone()))
            .expect("registration should not fail");

        let export_report_requests_total = Counter::new(
            "perf_sentinel_export_report_requests_total",
            "Total requests to GET /api/export/report since daemon start",
        )
        .expect("metric creation should not fail");
        registry
            .register(Box::new(export_report_requests_total.clone()))
            .expect("registration should not fail");

        let otlp_rejected_total = register_int_counter_vec(
            &registry,
            "perf_sentinel_otlp_rejected_total",
            "Total OTLP requests rejected by the daemon, by reason",
            &["reason"],
        );
        // Cache the 3 labeled children. `with_label_values` materializes
        // the child in the parent vec on first call, so the children
        // both serve as the hot-path increment handles (cf
        // `record_otlp_reject`) and pre-warm the metric so `gather()`
        // emits a 0 line for each reason before the first rejection.
        let otlp_rejected_unsupported_media_type = otlp_rejected_total
            .with_label_values(&[OtlpRejectReason::UnsupportedMediaType.as_str()]);
        let otlp_rejected_parse_error =
            otlp_rejected_total.with_label_values(&[OtlpRejectReason::ParseError.as_str()]);
        let otlp_rejected_channel_full =
            otlp_rejected_total.with_label_values(&[OtlpRejectReason::ChannelFull.as_str()]);

        #[cfg(feature = "daemon")]
        let ack_operations_total = register_int_counter_vec(
            &registry,
            "perf_sentinel_ack_operations_total",
            "Successful ack and unack operations on the daemon HTTP API, by action",
            &["action"],
        );
        #[cfg(feature = "daemon")]
        let ack_operations_failed_total = register_int_counter_vec(
            &registry,
            "perf_sentinel_ack_operations_failed_total",
            "Failed ack and unack operations on the daemon HTTP API, by action and reason",
            &["action", "reason"],
        );
        // Cache success children, plus pre-warm documented failure
        // combinations so dashboards build with `rate()` queries
        // without `absent()` guards. We skip impossible combinations
        // (action=ack with reason=not_acked, action=unack with reason
        // already_acked / limit_reached / file_too_large /
        // entry_too_large) to avoid misleading series. The `let _ =`
        // on the failure pre-warm makes the materialization side
        // effect explicit, the returned child is intentionally
        // dropped, the parent vec retains it.
        #[cfg(feature = "daemon")]
        let ack_operations_ack_success =
            ack_operations_total.with_label_values(&[AckAction::Ack.as_str()]);
        #[cfg(feature = "daemon")]
        let ack_operations_unack_success =
            ack_operations_total.with_label_values(&[AckAction::Unack.as_str()]);
        #[cfg(feature = "daemon")]
        {
            let prewarm_failure: &[(AckAction, &[AckFailureReason])] = &[
                (
                    AckAction::Ack,
                    &[
                        AckFailureReason::AlreadyAcked,
                        AckFailureReason::Unauthorized,
                        AckFailureReason::NoStore,
                        AckFailureReason::InvalidSignature,
                        AckFailureReason::LimitReached,
                        AckFailureReason::FileTooLarge,
                        AckFailureReason::EntryTooLarge,
                        AckFailureReason::InternalError,
                    ],
                ),
                (
                    AckAction::Unack,
                    &[
                        AckFailureReason::NotAcked,
                        AckFailureReason::Unauthorized,
                        AckFailureReason::NoStore,
                        AckFailureReason::InvalidSignature,
                        AckFailureReason::InternalError,
                    ],
                ),
            ];
            for (action, reasons) in prewarm_failure {
                for reason in *reasons {
                    let _ = ack_operations_failed_total
                        .with_label_values(&[action.as_str(), reason.as_str()]);
                }
            }
        }

        // Process metrics (RSS, FDs, start_time, CPU). procfs-backed,
        // Linux-only. On macOS/Windows we skip registration so each
        // scrape does not pay for failed reads under the hood.
        #[cfg(target_os = "linux")]
        {
            use prometheus::process_collector::ProcessCollector;
            registry
                .register(Box::new(ProcessCollector::for_self()))
                .expect("process collector registration should not fail");
        }

        Self {
            registry,
            findings_total,
            io_waste_ratio,
            traces_analyzed_total,
            events_processed_total,
            active_traces,
            total_io_ops,
            avoidable_io_ops,
            service_io_ops_total,
            scaphandre_last_scrape_age_seconds,
            cloud_energy_last_scrape_age_seconds,
            slow_duration_seconds,
            export_report_requests_total,
            otlp_rejected_total,
            otlp_rejected_unsupported_media_type,
            otlp_rejected_parse_error,
            otlp_rejected_channel_full,
            #[cfg(feature = "daemon")]
            ack_operations_total,
            #[cfg(feature = "daemon")]
            ack_operations_failed_total,
            #[cfg(feature = "daemon")]
            ack_operations_ack_success,
            #[cfg(feature = "daemon")]
            ack_operations_unack_success,
            worst_finding_trace: Arc::new(RwLock::new(HashMap::new())),
            worst_waste_trace: Arc::new(RwLock::new(None)),
        }
    }

    /// Increment `perf_sentinel_otlp_rejected_total` for the given
    /// reason. Called by the OTLP HTTP and gRPC handlers at every
    /// rejection site. Branchless `match` over the cached children, no
    /// per-call label hashmap lookup, so a backpressure storm does not
    /// amplify daemon slowdown via metric overhead.
    #[inline]
    pub fn record_otlp_reject(&self, reason: OtlpRejectReason) {
        match reason {
            OtlpRejectReason::UnsupportedMediaType => {
                self.otlp_rejected_unsupported_media_type.inc();
            }
            OtlpRejectReason::ParseError => self.otlp_rejected_parse_error.inc(),
            OtlpRejectReason::ChannelFull => self.otlp_rejected_channel_full.inc(),
        }
    }

    /// Increment `perf_sentinel_ack_operations_total` for the given
    /// action. Called by the daemon ack endpoints on every successful
    /// ack or unack write. Branchless `match` over the cached children,
    /// no per-call label hashmap lookup.
    #[cfg(feature = "daemon")]
    #[inline]
    pub fn record_ack_success(&self, action: AckAction) {
        match action {
            AckAction::Ack => self.ack_operations_ack_success.inc(),
            AckAction::Unack => self.ack_operations_unack_success.inc(),
        }
    }

    /// Increment `perf_sentinel_ack_operations_failed_total` for the
    /// given action and reason. Called at every error exit path of the
    /// daemon ack endpoints. Failures are by definition rare, so we
    /// pay the label hashmap lookup per call instead of caching all
    /// 13 documented children.
    #[cfg(feature = "daemon")]
    #[inline]
    pub fn record_ack_failure(&self, action: AckAction, reason: AckFailureReason) {
        self.ack_operations_failed_total
            .with_label_values(&[action.as_str(), reason.as_str()])
            .inc();
    }

    /// snapshot the per-service I/O op counter.
    ///
    /// Returns a `HashMap<service_name, cumulative_count>` built by
    /// iterating the Prometheus `CounterVec` metric families via the
    /// registry's `gather()` method. The Scaphandre scraper calls
    /// this once per tick and feeds it into
    /// [`crate::score::scaphandre::OpsSnapshotDiff`] to compute the
    /// per-service op delta for the current window.
    ///
    /// Returns an empty map when no services have been observed yet.
    #[must_use]
    pub fn snapshot_service_io_ops(&self) -> HashMap<String, u64> {
        use prometheus::core::Collector;
        let mut out = HashMap::new();
        for family in Collector::collect(&self.service_io_ops_total) {
            for metric in family.get_metric() {
                // `metric.get_counter()` returns `MessageField<Counter>`
                // (protobuf wrapper). Dereference to the inner Counter
                // and call `.value()` which is the current accessor in
                // prometheus 0.14.
                let counter_value = metric.get_counter().value();
                // Cumulative counts should always be representable as
                // u64, saturate to u64::MAX on overflow so the delta
                // math still produces sane values.
                // Saturate to u64 safely: clamp the float to the
                // representable range first, then cast. Counter
                // values should never be negative or overflow, but
                // clippy's cast_sign_loss / cast_possible_truncation
                // lints want the bounds to be explicit.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let count = if counter_value <= 0.0 {
                    0u64
                } else if counter_value >= u64::MAX as f64 {
                    u64::MAX
                } else {
                    counter_value as u64
                };
                for label in metric.get_label() {
                    if label.name() == "service" {
                        out.insert(label.value().to_string(), count);
                        break;
                    }
                }
            }
        }
        out
    }

    /// Record analysis results from a batch report.
    ///
    /// Updates all counters/gauges and tracks worst-case `trace_id` values
    /// for exemplar annotations on Prometheus metrics.
    ///
    /// Recovers gracefully if an internal lock is poisoned.
    pub fn record_batch(&self, report: &Report) {
        self.traces_analyzed_total
            .inc_by(report.analysis.traces_analyzed as f64);
        self.events_processed_total
            .inc_by(report.analysis.events_processed as f64);
        self.total_io_ops
            .inc_by(report.green_summary.total_io_ops as f64);
        self.avoidable_io_ops
            .inc_by(report.green_summary.avoidable_io_ops as f64);

        let cumulative_total = self.total_io_ops.get();
        if cumulative_total > 0.0 {
            self.io_waste_ratio
                .set(self.avoidable_io_ops.get() / cumulative_total);
        }

        for finding in &report.findings {
            self.findings_total
                .with_label_values(&[finding.finding_type.as_str(), finding.severity.as_str()])
                .inc();
        }

        self.record_exemplars(&report.findings, &report.green_summary);
    }

    /// Update exemplar tracking from findings and green summary.
    ///
    /// Called by both `record_batch` (batch mode) and the daemon's `process_traces`.
    /// Builds exemplar data in a local map, then takes the write lock only for the swap.
    ///
    /// Recovers gracefully if an internal lock is poisoned.
    pub fn record_exemplars(
        &self,
        findings: &[crate::detect::Finding],
        green_summary: &crate::report::GreenSummary,
    ) {
        // Build exemplar updates locally to minimize lock hold time.
        let mut new_exemplars: HashMap<(&'static str, &'static str), ExemplarData> = HashMap::new();
        for finding in findings {
            new_exemplars.insert(
                (finding.finding_type.as_str(), finding.severity.as_str()),
                ExemplarData {
                    trace_id: finding.trace_id.clone(),
                },
            );
        }

        if !new_exemplars.is_empty() {
            let mut worst_map = self
                .worst_finding_trace
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            worst_map.extend(new_exemplars);
        }

        // Track worst-case trace for waste ratio
        if let Some(worst_finding) = (green_summary.io_waste_ratio > 0.0)
            .then(|| {
                findings
                    .iter()
                    .filter(|f| f.finding_type.is_avoidable_io())
                    .max_by_key(|f| f.pattern.occurrences)
            })
            .flatten()
        {
            let mut waste_lock = self
                .worst_waste_trace
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *waste_lock = Some(ExemplarData {
                trace_id: worst_finding.trace_id.clone(),
            });
        }
    }

    /// Whether any exemplar data is available.
    ///
    /// Recovers gracefully if an internal lock is poisoned.
    #[must_use]
    pub fn has_exemplars(&self) -> bool {
        let finding_lock = self
            .worst_finding_trace
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !finding_lock.is_empty() {
            return true;
        }
        let waste_lock = self
            .worst_waste_trace
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        waste_lock.is_some()
    }

    /// Render with HTTP content negotiation. Returns `(body, content_type)`.
    ///
    /// Three-mode dispatch keyed on the client's `Accept` header:
    /// 1. Header contains `application/openmetrics-text` (Prometheus default,
    ///    explicit `OpenMetrics` request): emit `OpenMetrics` 1.0 always,
    ///    with the `# EOF` terminator and exemplar annotations when
    ///    exemplars exist.
    /// 2. Header absent, `*/*`, or `*/*` mixed in (vmagent-style
    ///    `text/plain;*/*;q=0.1`): legacy 0.5.15 behavior, `OpenMetrics`
    ///    when `has_exemplars()`, plain `Prometheus` otherwise. Preserves
    ///    vmagent and curl by default.
    /// 3. Header refuses the wildcard and accepts only `text/plain`: plain
    ///    `Prometheus` 0.0.4, no `# EOF`, no exemplar injection.
    ///
    /// On any internal encoder failure returns
    /// `"# error encoding metrics\n"` with `text/plain` rather than panicking.
    #[must_use]
    pub fn negotiate(&self, accept: Option<&str>) -> (String, &'static str) {
        let format = select_format(accept);

        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        if encoder.encode(&metric_families, &mut buffer).is_err() {
            return (
                "# error encoding metrics\n".to_string(),
                "text/plain; version=0.0.4; charset=utf-8",
            );
        }
        let Ok(base_output) = String::from_utf8(buffer) else {
            return (
                "# error encoding metrics\n".to_string(),
                "text/plain; version=0.0.4; charset=utf-8",
            );
        };

        match format {
            NegotiatedFormat::OpenMetricsForced => {
                let mut output = if self.has_exemplars() {
                    self.inject_exemplars(base_output)
                } else {
                    base_output
                };
                if !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("# EOF\n");
                (
                    output,
                    "application/openmetrics-text; version=1.0.0; charset=utf-8",
                )
            }
            NegotiatedFormat::Legacy => {
                if self.has_exemplars() {
                    let mut output = self.inject_exemplars(base_output);
                    output.push_str("# EOF\n");
                    (
                        output,
                        "application/openmetrics-text; version=1.0.0; charset=utf-8",
                    )
                } else {
                    (base_output, "text/plain; version=0.0.4; charset=utf-8")
                }
            }
            NegotiatedFormat::PlainStrict => {
                (base_output, "text/plain; version=0.0.4; charset=utf-8")
            }
        }
    }

    /// Render in the format that [`Self::negotiate`]`(None)` would choose.
    /// Used by tests and non-HTTP callers that lack an Accept header.
    #[must_use]
    pub fn render(&self) -> String {
        self.negotiate(None).0
    }

    /// Post-process rendered metrics text to inject exemplar annotations.
    ///
    /// Note: This relies on the prometheus crate 0.14.0 output format for line-prefix
    /// matching. If the crate changes its label ordering or spacing, the matching
    /// will silently stop injecting exemplars. The exemplar format follows the
    /// `OpenMetrics` 1.0.0 specification (section 5.1.10):
    /// `metric{labels} value # {trace_id="..."} 1.0`. The trailing `1.0` is the
    /// mandatory exemplar value, set to a constant dummy because Grafana and
    /// other exemplar-aware tools read only the `trace_id` label for click-
    /// through navigation.
    fn inject_exemplars(&self, base: String) -> String {
        use std::fmt::Write;

        let finding_map = self
            .worst_finding_trace
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let waste_exemplar = self
            .worst_waste_trace
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if finding_map.is_empty() && waste_exemplar.is_none() {
            return base;
        }

        let mut output = String::with_capacity(base.len() + 256);

        for line in base.lines() {
            output.push_str(line);

            // Inject exemplar on findings_total lines
            if let Some(exemplar) = line
                .starts_with("perf_sentinel_findings_total{")
                .then(|| extract_finding_exemplar(line, &finding_map))
                .flatten()
            {
                let sanitized = sanitize_exemplar_value(&exemplar.trace_id);
                let _ = write!(output, " # {{trace_id=\"{sanitized}\"}} 1.0");
            }

            // Inject exemplar on io_waste_ratio line
            if let Some(exemplar) = waste_exemplar
                .as_ref()
                .filter(|_| line.starts_with("perf_sentinel_io_waste_ratio "))
            {
                let sanitized = sanitize_exemplar_value(&exemplar.trace_id);
                let _ = write!(output, " # {{trace_id=\"{sanitized}\"}} 1.0");
            }

            output.push('\n');
        }

        output
    }

    /// Returns the `Content-Type` that [`Self::negotiate`]`(None)` would
    /// produce. Used by tests and non-HTTP callers that lack an Accept header.
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        self.negotiate(None).1
    }
}

/// Output format selected from the client's HTTP `Accept` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NegotiatedFormat {
    /// Client explicitly requested `application/openmetrics-text`.
    /// Emit `OpenMetrics` 1.0 always, with `# EOF` and exemplars when present.
    OpenMetricsForced,
    /// No Accept, `*/*`, or `*/*` mixed with other media types
    /// (vmagent-style `text/plain;*/*;q=0.1`). Preserves the 0.5.15
    /// behavior: `OpenMetrics` when `has_exemplars()`, plain otherwise.
    Legacy,
    /// Client explicitly accepts `text/plain` and refuses the wildcard.
    /// Emit plain `Prometheus` 0.0.4, no `# EOF`, no exemplar injection.
    PlainStrict,
}

/// Negotiation discriminator. `None` falls back to legacy behavior so
/// non-HTTP callers (CLI batch path, tests calling `render()` without a
/// header) keep the 0.5.15 contract.
///
/// `OpenMetrics` detection is token-aware. The header is split on `,` per
/// RFC 7231 §5.3.2, each token's media-type is compared case-insensitively
/// to the literal `application/openmetrics-text`. A hostile or unrelated
/// token such as `application/openmetrics-text-evil` therefore does NOT
/// trigger the `OpenMetrics` path. Tokens explicitly refused via `q=0`
/// (or `q=0.0`) are skipped per RFC 7231 §5.3.1.
///
/// The `*/*` wildcard detection uses a substring match on purpose, the
/// wildcard is unique enough that no other media type contains it, and
/// some real scrapers (vmagent in particular) emit a non-RFC variant
/// where `*/*` appears as a parameter rather than a comma-separated
/// token (e.g. `text/plain;*/*;q=0.1`). The substring match handles
/// both the RFC form and the vmagent form.
fn select_format(accept: Option<&str>) -> NegotiatedFormat {
    let Some(header) = accept else {
        return NegotiatedFormat::Legacy;
    };

    let mut accepts_openmetrics = false;

    for token in header.split(',') {
        let token = token.trim();
        let mut parts = token.split(';');
        let media = parts.next().unwrap_or("").trim();
        let refused = parts.any(|p| {
            let p = p.trim();
            p.eq_ignore_ascii_case("q=0") || p.eq_ignore_ascii_case("q=0.0")
        });
        if refused {
            continue;
        }
        if media.eq_ignore_ascii_case("application/openmetrics-text") {
            accepts_openmetrics = true;
            break;
        }
    }

    if accepts_openmetrics {
        NegotiatedFormat::OpenMetricsForced
    } else if header.contains("*/*") {
        NegotiatedFormat::Legacy
    } else {
        NegotiatedFormat::PlainStrict
    }
}

/// Extract the finding exemplar for a given `findings_total` metric line.
///
/// Parses the `type` and `severity` labels from the line and looks them up
/// in the exemplar map. Since the map keys are `&'static str` (from `FindingType::as_str()`
/// and `Severity::as_str()`), the lookup iterates the map to compare against
/// the parsed label values without allocating.
fn extract_finding_exemplar<'a>(
    line: &str,
    map: &'a HashMap<(&'static str, &'static str), ExemplarData>,
) -> Option<&'a ExemplarData> {
    // Line format: perf_sentinel_findings_total{severity="warning",type="n_plus_one_sql"} 1
    let labels_start = line.find('{')?;
    let labels_end = line.find('}')?;
    let labels_str = &line[labels_start + 1..labels_end];

    let mut finding_type = None;
    let mut severity = None;

    for part in labels_str.split(',') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("type=\"") {
            finding_type = val.strip_suffix('"');
        } else if let Some(val) = part.strip_prefix("severity=\"") {
            severity = val.strip_suffix('"');
        }
    }

    let ft = finding_type?;
    let sev = severity?;
    // Iterate the map to find a matching key without allocating Strings
    map.iter()
        .find(|((k_ft, k_sev), _)| *k_ft == ft && *k_sev == sev)
        .map(|(_, v)| v)
}

impl Default for MetricsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build an axum router with a `GET /metrics` endpoint.
pub fn metrics_route(state: Arc<MetricsState>) -> Router {
    async fn handle_metrics(
        State(metrics): State<Arc<MetricsState>>,
        headers: axum::http::HeaderMap,
    ) -> ([(axum::http::header::HeaderName, &'static str); 1], String) {
        let accept = headers
            .get(axum::http::header::ACCEPT)
            .and_then(|v| v.to_str().ok());
        let (body, content_type) = metrics.negotiate(accept);
        ([(axum::http::header::CONTENT_TYPE, content_type)], body)
    }

    Router::new()
        .route("/metrics", get(handle_metrics))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Confidence, Finding, FindingType, GreenImpact, Pattern, Severity};
    use crate::report::{Analysis, GreenSummary, QualityGate, Report};

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn make_test_report(findings: Vec<Finding>, waste_ratio: f64) -> Report {
        let total = 10;
        let avoidable = (total as f64 * waste_ratio) as usize;
        Report {
            analysis: Analysis {
                duration_ms: 1,
                events_processed: 100,
                traces_analyzed: 2,
            },
            findings,
            green_summary: GreenSummary {
                total_io_ops: total,
                avoidable_io_ops: avoidable,
                io_waste_ratio: waste_ratio,
                io_waste_ratio_band: crate::report::interpret::InterpretationLevel::for_waste_ratio(
                    waste_ratio,
                ),
                top_offenders: vec![],
                co2: None,
                regions: vec![],
                transport_gco2: None,
                scoring_config: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
        }
    }

    fn make_finding(
        finding_type: FindingType,
        severity: Severity,
        trace_id: &str,
        occurrences: usize,
    ) -> Finding {
        Finding {
            finding_type,
            severity,
            trace_id: trace_id.to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences,
                window_ms: 200,
                distinct_params: occurrences,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: Some(GreenImpact {
                estimated_extra_io_ops: occurrences.saturating_sub(1),
                io_intensity_score: 6.0,
                io_intensity_band: crate::report::interpret::InterpretationLevel::for_iis(6.0),
            }),
            confidence: Confidence::default(),
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: String::new(),
        }
    }

    #[test]
    fn default_creates_same_as_new() {
        let state = MetricsState::default();
        // Should work identically to new()
        state.events_processed_total.inc();
        let output = state.render();
        assert!(output.contains("perf_sentinel_events_processed_total"));
    }

    #[tokio::test]
    async fn metrics_route_returns_prometheus_output() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let state = Arc::new(MetricsState::new());
        state.traces_analyzed_total.inc_by(42.0);
        state.io_waste_ratio.set(0.25);
        state
            .findings_total
            .with_label_values(&["n_plus_one_sql", "warning"])
            .inc();

        let router = metrics_route(state);

        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify Prometheus-compliant Content-Type
        let content_type = response
            .headers()
            .get("content-type")
            .expect("should have content-type header")
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/plain"),
            "Content-Type should be text/plain, got: {content_type}"
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        assert!(
            body_str.contains("perf_sentinel_traces_analyzed_total 42"),
            "should contain traces count, got: {body_str}"
        );
        assert!(
            body_str.contains("perf_sentinel_io_waste_ratio 0.25"),
            "should contain waste ratio"
        );
        assert!(
            body_str.contains("n_plus_one_sql"),
            "should contain finding type label"
        );
    }

    #[test]
    fn metrics_state_creates_successfully() {
        let state = MetricsState::new();
        // Initialize the CounterVec with a label pair so it appears in output
        state
            .findings_total
            .with_label_values(&["test", "test"])
            .inc_by(0.0);
        let output = state.render();
        assert!(
            output.contains("perf_sentinel_findings_total"),
            "output: {output}"
        );
        assert!(output.contains("perf_sentinel_io_waste_ratio"));
        assert!(output.contains("perf_sentinel_traces_analyzed_total"));
        assert!(output.contains("perf_sentinel_events_processed_total"));
        assert!(output.contains("perf_sentinel_active_traces"));
    }

    #[test]
    fn increment_findings_counter() {
        let state = MetricsState::new();
        state
            .findings_total
            .with_label_values(&["n_plus_one_sql", "critical"])
            .inc();
        state
            .findings_total
            .with_label_values(&["n_plus_one_sql", "critical"])
            .inc();

        let output = state.render();
        assert!(output.contains(r#"type="n_plus_one_sql""#));
        assert!(output.contains(r#"severity="critical""#));
    }

    #[test]
    fn set_gauge_values() {
        let state = MetricsState::new();
        state.io_waste_ratio.set(0.42);
        state.active_traces.set(5.0);

        let output = state.render();
        assert!(output.contains("0.42"));
    }

    #[test]
    fn increment_counters() {
        let state = MetricsState::new();
        state.traces_analyzed_total.inc_by(10.0);
        state.events_processed_total.inc_by(100.0);

        let output = state.render();
        assert!(output.contains("100"));
    }

    // -- Exemplar tests --

    #[test]
    fn record_batch_tracks_worst_finding_trace() {
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Critical,
                "trace-abc",
                10,
            )],
            0.5,
        );
        state.record_batch(&report);

        let map = state.worst_finding_trace.read().unwrap();
        assert_eq!(
            map.get(&("n_plus_one_sql", "critical")).unwrap().trace_id,
            "trace-abc"
        );
    }

    #[test]
    fn record_batch_tracks_worst_waste_trace() {
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-waste",
                8,
            )],
            0.4,
        );
        state.record_batch(&report);

        let waste = state.worst_waste_trace.read().unwrap();
        assert_eq!(waste.as_ref().unwrap().trace_id, "trace-waste");
    }

    #[test]
    fn render_includes_exemplar_annotation() {
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-exemplar",
                6,
            )],
            0.3,
        );
        state.record_batch(&report);

        let output = state.render();
        assert!(
            output.contains(r#"# {trace_id="trace-exemplar"}"#),
            "should contain exemplar annotation, got: {output}"
        );
    }

    #[test]
    fn render_no_exemplar_when_no_data() {
        let state = MetricsState::new();
        // Manually set some metrics without using record_batch
        state.traces_analyzed_total.inc();
        state
            .findings_total
            .with_label_values(&["n_plus_one_sql", "warning"])
            .inc();

        let output = state.render();
        assert!(
            !output.contains("# {trace_id="),
            "should not contain exemplar when no record_batch called"
        );
    }

    #[test]
    fn exemplar_on_io_waste_ratio() {
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::RedundantSql,
                Severity::Warning,
                "trace-waste-ratio",
                4,
            )],
            0.5,
        );
        state.record_batch(&report);

        let output = state.render();
        // The io_waste_ratio line should have an exemplar
        for line in output.lines() {
            if line.starts_with("perf_sentinel_io_waste_ratio ") {
                assert!(
                    line.contains(r#"# {trace_id="trace-waste-ratio"}"#),
                    "waste ratio line should have exemplar: {line}"
                );
            }
        }
    }

    #[test]
    fn content_type_is_openmetrics_with_exemplars() {
        let state = MetricsState::new();
        assert_eq!(
            state.content_type(),
            "text/plain; version=0.0.4; charset=utf-8"
        );

        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-1",
                5,
            )],
            0.0,
        );
        state.record_batch(&report);
        assert_eq!(
            state.content_type(),
            "application/openmetrics-text; version=1.0.0; charset=utf-8"
        );
    }

    #[test]
    fn multiple_batches_update_exemplars() {
        let state = MetricsState::new();

        let report1 = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-old",
                5,
            )],
            0.3,
        );
        state.record_batch(&report1);

        let report2 = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-new",
                10,
            )],
            0.5,
        );
        state.record_batch(&report2);

        let map = state.worst_finding_trace.read().unwrap();
        assert_eq!(
            map.get(&("n_plus_one_sql", "warning")).unwrap().trace_id,
            "trace-new",
            "should update to latest batch's worst finding"
        );
    }

    #[tokio::test]
    async fn metrics_route_returns_openmetrics_with_exemplars() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let state = Arc::new(MetricsState::new());
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-route-test",
                5,
            )],
            0.0,
        );
        state.record_batch(&report);

        let router = metrics_route(state);
        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("openmetrics"),
            "should use OpenMetrics content type: {content_type}"
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body_str.contains(r#"trace_id="trace-route-test""#),
            "should contain exemplar trace_id"
        );
    }

    // Three-mode Accept negotiation tests. Strict OM-forced when the client
    // requests OpenMetrics, legacy 0.5.15 behavior when no preference, plain
    // strict when the client refuses the wildcard.

    #[test]
    fn negotiate_returns_openmetrics_when_accept_header_explicitly_requests_it() {
        // No exemplars but explicit OM request → OM 1.0 forced with `# EOF`.
        let state = MetricsState::new();
        state.traces_analyzed_total.inc();

        let (body, content_type) =
            state.negotiate(Some("application/openmetrics-text;version=1.0.0"));
        assert_eq!(
            content_type,
            "application/openmetrics-text; version=1.0.0; charset=utf-8"
        );
        assert!(
            body.ends_with("# EOF\n"),
            "OM-forced body must terminate with `# EOF\\n`"
        );
    }

    #[test]
    fn negotiate_returns_openmetrics_with_exemplars_when_accept_explicit_and_exemplars_present() {
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-om-explicit",
                5,
            )],
            0.4,
        );
        state.record_batch(&report);

        let (body, content_type) = state.negotiate(Some("application/openmetrics-text"));
        assert_eq!(
            content_type,
            "application/openmetrics-text; version=1.0.0; charset=utf-8"
        );
        assert!(body.ends_with("# EOF\n"));
        assert!(
            body.contains(r#"# {trace_id="trace-om-explicit"} 1.0"#),
            "OM-forced body must include exemplar annotation: {body}"
        );
    }

    #[test]
    fn negotiate_falls_back_to_legacy_when_accept_absent() {
        // No Accept header → legacy 0.5.15 behavior, OM-when-exemplars.
        let state = MetricsState::new();
        state.traces_analyzed_total.inc();

        // No exemplars, legacy → plain Prometheus, no `# EOF`.
        let (body_no_ex, ct_no_ex) = state.negotiate(None);
        assert_eq!(ct_no_ex, "text/plain; version=0.0.4; charset=utf-8");
        assert!(!body_no_ex.contains("# EOF"));

        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-legacy",
                5,
            )],
            0.0,
        );
        state.record_batch(&report);

        // With exemplars, legacy → OpenMetrics with `# EOF`.
        let (body_with_ex, ct_with_ex) = state.negotiate(None);
        assert_eq!(
            ct_with_ex,
            "application/openmetrics-text; version=1.0.0; charset=utf-8"
        );
        assert!(body_with_ex.ends_with("# EOF\n"));
    }

    #[test]
    fn negotiate_falls_back_to_legacy_when_accept_contains_wildcard() {
        // vmagent-style Accept includes `*/*` as a low-q fallback. Treated
        // as "no preference", legacy behavior preserved so vmagent keeps
        // its exemplars on the Grafana click-through path.
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-vmagent",
                5,
            )],
            0.0,
        );
        state.record_batch(&report);

        let (body, content_type) = state.negotiate(Some("text/plain;version=0.0.4;*/*;q=0.1"));
        assert_eq!(
            content_type,
            "application/openmetrics-text; version=1.0.0; charset=utf-8"
        );
        assert!(body.ends_with("# EOF\n"));
        assert!(body.contains(r#"# {trace_id="trace-vmagent"} 1.0"#));
    }

    #[test]
    fn negotiate_returns_plain_strict_when_accept_text_plain_only() {
        // Strict refusal of OM and `*/*` → plain Prometheus, no exemplars
        // even when present, no `# EOF`. Defends pre-OpenMetrics scrapers.
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-strict",
                5,
            )],
            0.4,
        );
        state.record_batch(&report);

        let (body, content_type) = state.negotiate(Some("text/plain;version=0.0.4"));
        assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
        assert!(!body.contains("# EOF"));
        assert!(
            !body.contains("# {trace_id="),
            "plain-strict body must not contain exemplar annotations: {body}"
        );
    }

    #[test]
    fn select_format_dispatches_correctly() {
        // None → Legacy.
        assert_eq!(select_format(None), NegotiatedFormat::Legacy);
        // Plain strict.
        assert_eq!(
            select_format(Some("text/plain")),
            NegotiatedFormat::PlainStrict
        );
        assert_eq!(
            select_format(Some("text/plain;version=0.0.4")),
            NegotiatedFormat::PlainStrict
        );
        // Legacy via wildcard. The vmagent-style header uses semicolons
        // instead of commas, the substring `*/*` check still fires.
        assert_eq!(select_format(Some("*/*")), NegotiatedFormat::Legacy);
        assert_eq!(
            select_format(Some("text/plain;*/*;q=0.1")),
            NegotiatedFormat::Legacy
        );
        assert_eq!(
            select_format(Some("text/plain;version=0.0.4,*/*;q=0.1")),
            NegotiatedFormat::Legacy
        );
        // Forced via explicit OM.
        assert_eq!(
            select_format(Some("application/openmetrics-text;version=1.0.0")),
            NegotiatedFormat::OpenMetricsForced
        );
        assert_eq!(
            select_format(Some("application/openmetrics-text;q=0.9,text/plain;q=0.5")),
            NegotiatedFormat::OpenMetricsForced
        );
        // Case-insensitive media-type match.
        assert_eq!(
            select_format(Some("Application/OpenMetrics-Text")),
            NegotiatedFormat::OpenMetricsForced
        );
        // Substring attack: a hostile media type that contains the OM
        // literal as a substring must NOT route to OM.
        assert_eq!(
            select_format(Some("application/openmetrics-text-foo")),
            NegotiatedFormat::PlainStrict
        );
        assert_eq!(
            select_format(Some("xapplication/openmetrics-text")),
            NegotiatedFormat::PlainStrict
        );
        // Explicit refusal via `q=0` per RFC 7231 §5.3.1: the client
        // rejects OM even though the literal is present.
        assert_eq!(
            select_format(Some("application/openmetrics-text;q=0,*/*;q=1")),
            NegotiatedFormat::Legacy
        );
        assert_eq!(
            select_format(Some("application/openmetrics-text;q=0.0,text/plain")),
            NegotiatedFormat::PlainStrict
        );
    }

    #[tokio::test]
    async fn metrics_route_honors_explicit_openmetrics_accept_header() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let state = Arc::new(MetricsState::new());
        state.traces_analyzed_total.inc();

        let router = metrics_route(state);
        let request = Request::builder()
            .uri("/metrics")
            .header("accept", "application/openmetrics-text;version=1.0.0")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("application/openmetrics-text"),
            "expected OM CT, got: {ct}"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            String::from_utf8(body.to_vec())
                .unwrap()
                .ends_with("# EOF\n")
        );
    }

    #[tokio::test]
    async fn metrics_route_preserves_legacy_behavior_for_vmagent_style_accept() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let state = Arc::new(MetricsState::new());
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-vmagent-route",
                5,
            )],
            0.0,
        );
        state.record_batch(&report);

        let router = metrics_route(state);
        let request = Request::builder()
            .uri("/metrics")
            .header("accept", "text/plain;version=0.0.4;*/*;q=0.1")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("application/openmetrics-text"),
            "vmagent-style Accept (`*/*`) must keep legacy OM-with-exemplars: {ct}"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.ends_with("# EOF\n"));
        assert!(body_str.contains(r#"trace_id="trace-vmagent-route""#));
    }

    #[test]
    fn sanitize_exemplar_value_strips_dangerous_chars() {
        assert_eq!(sanitize_exemplar_value("abc-123_def"), "abc-123_def");
        assert_eq!(
            sanitize_exemplar_value("evil\"} 999\nfake_metric"),
            "evil999fake_metric"
        );
        assert_eq!(sanitize_exemplar_value(""), "");
        // Truncation to 64 chars
        let long = "a".repeat(100);
        assert_eq!(sanitize_exemplar_value(&long).len(), 64);
    }

    #[test]
    fn exemplar_with_malicious_trace_id_is_sanitized() {
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "evil\"} 999\nmy_fake_metric",
                5,
            )],
            0.0,
        );
        state.record_batch(&report);

        let output = state.render();
        // Should NOT contain the raw malicious string
        assert!(
            !output.contains("evil\""),
            "malicious trace_id should be sanitized"
        );
        // Should contain the sanitized version
        assert!(output.contains("evil999my_fake_metric"));
    }

    #[test]
    fn render_appends_eof_marker_with_exemplars() {
        // OpenMetrics 1.0.0 mandates `# EOF` as the end-of-exposition marker.
        // Pre-0.5.15 the daemon advertised the OpenMetrics content type but
        // omitted the marker, causing strict scrapers (Prometheus in
        // openmetrics-text negotiation) to refuse the payload.
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-eof",
                5,
            )],
            0.3,
        );
        state.record_batch(&report);
        let output = state.render();

        assert!(
            output.ends_with("# EOF\n"),
            "OpenMetrics output must terminate with `# EOF\\n`, got tail: {:?}",
            &output[output.len().saturating_sub(64)..]
        );
        // Catch a regression where someone appends content after the EOF
        // marker. The spec requires EOF to be the last logical record.
        assert!(
            !output.contains("# EOF\n#") && !output.contains("# EOF\nperf_sentinel"),
            "no content may follow the `# EOF` marker"
        );
    }

    #[test]
    fn render_omits_eof_marker_without_exemplars() {
        // Plain Prometheus text format (text/plain; version=0.0.4) must NOT
        // contain `# EOF`, which is illegal in pre-OpenMetrics scrapers.
        let state = MetricsState::new();
        state.traces_analyzed_total.inc();

        let output = state.render();
        assert!(
            !output.contains("# EOF"),
            "Prometheus text/plain output must not contain `# EOF`, got: {output}"
        );
    }

    #[test]
    fn exemplar_annotation_includes_numeric_value() {
        // OpenMetrics 1.0.0 section 5.1.10 requires a numeric value after the
        // exemplar labels block. Pre-0.5.15 the helper emitted only the labels.
        let state = MetricsState::new();
        let report = make_test_report(
            vec![make_finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "trace-numeric",
                5,
            )],
            0.5,
        );
        state.record_batch(&report);
        let output = state.render();

        let exemplar_line = output
            .lines()
            .find(|l| l.starts_with("perf_sentinel_findings_total{") && l.contains("trace_id="))
            .expect("expected at least one findings exemplar line");
        assert!(
            exemplar_line.ends_with(r#" # {trace_id="trace-numeric"} 1.0"#),
            "findings exemplar must follow OpenMetrics 1.0 format: {exemplar_line}"
        );

        let waste_line = output
            .lines()
            .find(|l| l.starts_with("perf_sentinel_io_waste_ratio ") && l.contains("trace_id="))
            .expect("expected the io_waste_ratio exemplar line");
        assert!(
            waste_line.ends_with(r#" # {trace_id="trace-numeric"} 1.0"#),
            "io_waste_ratio exemplar must follow OpenMetrics 1.0 format: {waste_line}"
        );
    }

    #[test]
    fn prometheus_output_format_matches_expected_prefixes() {
        // Regression test: validates that the prometheus crate (0.14.0) output
        // format matches the line prefixes used by inject_exemplars().
        // If this test fails after a prometheus crate upgrade, update inject_exemplars.
        let state = MetricsState::new();
        state
            .findings_total
            .with_label_values(&["n_plus_one_sql", "warning"])
            .inc();
        state.io_waste_ratio.set(0.5);

        let output = state.render();

        // Verify the line prefix format that inject_exemplars relies on
        let has_findings_prefix = output
            .lines()
            .any(|l| l.starts_with("perf_sentinel_findings_total{"));
        assert!(
            has_findings_prefix,
            "prometheus output must contain lines starting with 'perf_sentinel_findings_total{{': {output}"
        );

        let has_waste_prefix = output
            .lines()
            .any(|l| l.starts_with("perf_sentinel_io_waste_ratio "));
        assert!(
            has_waste_prefix,
            "prometheus output must contain lines starting with 'perf_sentinel_io_waste_ratio ': {output}"
        );
    }

    #[test]
    fn registry_contains_otlp_rejected() {
        let state = MetricsState::new();
        let output = state.render();
        assert!(
            output.contains("perf_sentinel_otlp_rejected_total"),
            "registry should expose perf_sentinel_otlp_rejected_total, got: {output}"
        );
    }

    #[test]
    fn otlp_rejected_starts_at_zero_for_all_three_reasons() {
        let state = MetricsState::new();
        for reason in [
            OtlpRejectReason::UnsupportedMediaType,
            OtlpRejectReason::ParseError,
            OtlpRejectReason::ChannelFull,
        ] {
            let count = state
                .otlp_rejected_total
                .with_label_values(&[reason.as_str()])
                .get();
            assert_eq!(count, 0, "reason {} should start at 0", reason.as_str());
        }
        let output = state.render();
        for reason in ["unsupported_media_type", "parse_error", "channel_full"] {
            assert!(
                output.contains(&format!(
                    "perf_sentinel_otlp_rejected_total{{reason=\"{reason}\"}} 0"
                )),
                "pre-warmed line for reason {reason} should appear in /metrics, got: {output}"
            );
        }
    }

    #[test]
    fn record_otlp_reject_increments_correct_label() {
        let state = MetricsState::new();
        state.record_otlp_reject(OtlpRejectReason::ChannelFull);
        state.record_otlp_reject(OtlpRejectReason::ChannelFull);
        state.record_otlp_reject(OtlpRejectReason::ChannelFull);
        assert_eq!(
            state
                .otlp_rejected_total
                .with_label_values(&["channel_full"])
                .get(),
            3
        );
        assert_eq!(
            state
                .otlp_rejected_total
                .with_label_values(&["parse_error"])
                .get(),
            0
        );
        assert_eq!(
            state
                .otlp_rejected_total
                .with_label_values(&["unsupported_media_type"])
                .get(),
            0
        );
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn ack_failure_reason_as_str_round_trips_all_variants() {
        for (variant, label) in [
            (AckFailureReason::AlreadyAcked, "already_acked"),
            (AckFailureReason::NotAcked, "not_acked"),
            (AckFailureReason::Unauthorized, "unauthorized"),
            (AckFailureReason::NoStore, "no_store"),
            (AckFailureReason::InvalidSignature, "invalid_signature"),
            (AckFailureReason::LimitReached, "limit_reached"),
            (AckFailureReason::FileTooLarge, "file_too_large"),
            (AckFailureReason::EntryTooLarge, "entry_too_large"),
            (AckFailureReason::InternalError, "internal_error"),
        ] {
            assert_eq!(variant.as_str(), label);
        }
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn record_ack_success_increments_correct_label() {
        let state = MetricsState::new();
        state.record_ack_success(AckAction::Ack);
        state.record_ack_success(AckAction::Ack);
        state.record_ack_success(AckAction::Unack);
        assert_eq!(state.ack_operations_ack_success.get(), 2);
        assert_eq!(state.ack_operations_unack_success.get(), 1);
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn record_ack_failure_increments_correct_combination() {
        let state = MetricsState::new();
        state.record_ack_failure(AckAction::Ack, AckFailureReason::Unauthorized);
        state.record_ack_failure(AckAction::Unack, AckFailureReason::NotAcked);
        assert_eq!(
            state
                .ack_operations_failed_total
                .with_label_values(&["ack", "unauthorized"])
                .get(),
            1
        );
        assert_eq!(
            state
                .ack_operations_failed_total
                .with_label_values(&["unack", "not_acked"])
                .get(),
            1
        );
        assert_eq!(
            state
                .ack_operations_failed_total
                .with_label_values(&["ack", "not_acked"])
                .get(),
            0
        );
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn ack_operations_total_starts_at_zero_for_both_actions() {
        let state = MetricsState::new();
        assert_eq!(state.ack_operations_ack_success.get(), 0);
        assert_eq!(state.ack_operations_unack_success.get(), 0);
        let output = state.render();
        for action in ["ack", "unack"] {
            assert!(
                output.contains(&format!(
                    "perf_sentinel_ack_operations_total{{action=\"{action}\"}} 0"
                )),
                "pre-warmed line for action={action} should appear, got: {output}"
            );
        }
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn ack_operations_failed_total_starts_at_zero_for_documented_combinations() {
        let state = MetricsState::new();
        let output = state.render();
        let documented: &[(&str, &[&str])] = &[
            (
                "ack",
                &[
                    "already_acked",
                    "unauthorized",
                    "no_store",
                    "invalid_signature",
                    "limit_reached",
                    "file_too_large",
                    "entry_too_large",
                    "internal_error",
                ],
            ),
            (
                "unack",
                &[
                    "not_acked",
                    "unauthorized",
                    "no_store",
                    "invalid_signature",
                    "internal_error",
                ],
            ),
        ];
        for (action, reasons) in documented {
            for reason in *reasons {
                let line = format!(
                    "perf_sentinel_ack_operations_failed_total{{action=\"{action}\",reason=\"{reason}\"}} 0"
                );
                assert!(
                    output.contains(&line),
                    "pre-warmed line {line} should appear in /metrics"
                );
            }
        }
        // Impossible combinations must not be pre-warmed: scraping for
        // them would mislead operators into building queries on series
        // that can never grow.
        for forbidden in [
            "perf_sentinel_ack_operations_failed_total{action=\"ack\",reason=\"not_acked\"}",
            "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"already_acked\"}",
            "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"limit_reached\"}",
            "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"file_too_large\"}",
            "perf_sentinel_ack_operations_failed_total{action=\"unack\",reason=\"entry_too_large\"}",
        ] {
            assert!(
                !output.contains(forbidden),
                "forbidden combination {forbidden} should not be pre-warmed"
            );
        }
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn ack_operations_appear_in_render() {
        let state = MetricsState::new();
        let output = state.render();
        assert!(
            output.contains("perf_sentinel_ack_operations_total"),
            "registry should expose perf_sentinel_ack_operations_total"
        );
        assert!(
            output.contains("perf_sentinel_ack_operations_failed_total"),
            "registry should expose perf_sentinel_ack_operations_failed_total"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_collector_registered_on_linux() {
        let state = MetricsState::new();
        let output = state.render();
        assert!(
            output
                .lines()
                .any(|l| l.starts_with("process_resident_memory_bytes")),
            "process_resident_memory_bytes should be exposed on Linux, got: {output}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn process_collector_not_registered_on_non_linux() {
        let state = MetricsState::new();
        let output = state.render();
        assert!(
            !output.contains("process_resident_memory_bytes"),
            "process_resident_memory_bytes must not be exposed off Linux, got: {output}"
        );
    }
}
