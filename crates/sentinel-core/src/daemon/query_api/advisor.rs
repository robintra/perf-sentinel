//! `Report.warning_details` builders for `/api/export/report`: snapshot scope and the tuning advisor.

use crate::report::metrics::MetricsState;

/// State what the snapshot covers.
///
/// Two facts a consumer cannot recover from the payload: the findings
/// are capped at `[daemon] max_export_findings`, and the green figures
/// are the event loop's latest per-batch
/// [`GreenSummary`](crate::report::GreenSummary), not an
/// aggregate over the findings listed beside them. On a busy daemon both
/// describe a fraction of the store, so the carbon totals otherwise read
/// as the daemon's lifetime. The cap also feeds `quality_gate`, whose
/// count rules only ever see the exported slice. The truncation line
/// says so, to keep a reader from treating a green gate as the daemon's
/// verdict. Batch output carries neither warning: there every number
/// comes from the same pass.
pub(super) fn snapshot_scope_warnings(
    exported: usize,
    retained: usize,
) -> Vec<crate::report::Warning> {
    use crate::report::warnings::SNAPSHOT_SCOPE;

    let mut details = Vec::new();
    if retained > exported {
        details.push(crate::report::Warning::new(
            SNAPSHOT_SCOPE,
            format!(
                "Findings capped at {exported} of {retained} retained: the most \
                 recent ones, not the whole store, and the quality gate below \
                 counts only these"
            ),
        ));
    }
    details.push(crate::report::Warning::new(
        SNAPSHOT_SCOPE,
        format!(
            "Green figures (io_waste_ratio, CO2, energy) describe the latest \
             analyzed batch, not the {exported} findings listed"
        ),
    ));
    details
}

/// Trace-window occupancy ratio above which the tuning advisor flags
/// `max_active_traces` as undersized.
const TUNING_ACTIVE_TRACES_RATIO: f64 = 0.9;

/// Minimum received-span count before zero retention is meaningful.
pub(super) const TUNING_ZERO_RETENTION_MIN_RECEIVED: u64 = 1_000;

/// Sum of filtered OTLP spans that signal an instrumentation gap.
/// Excludes `NonSqlDatastore` (Redis/Mongo drops are not an
/// instrumentation gap: a cache-only fleet must not trip the
/// zero-retention warning) and `MergedDbSpan` (the query those spans
/// belong to was analyzed).
fn instrumentation_gap_filtered(metrics: &MetricsState) -> u64 {
    use crate::report::metrics::OtlpSpanFilterReason;
    OtlpSpanFilterReason::ALL
        .iter()
        .filter(|r| {
            !matches!(
                r,
                OtlpSpanFilterReason::NonSqlDatastore | OtlpSpanFilterReason::MergedDbSpan
            )
        })
        .map(|r| {
            metrics
                .otlp_spans_filtered_total
                .with_label_values(&[r.as_str()])
                .get()
        })
        .sum()
}

/// Config-only advisor rule on `[daemon] sampling_rate`. Ratios are left
/// out: uniform per-trace sampling hits numerator and denominator alike,
/// so the waste ratio stays readable.
///
/// Called from `collect_warning_details` and from the cold-start branch too:
/// at rate 0.0 the daemon never leaves cold start.
pub(super) fn sampling_rate_warning(
    daemon: &crate::config::DaemonConfig,
) -> Option<crate::report::Warning> {
    use crate::report::warnings::TUNING;
    let rate = daemon.sampling_rate;
    if rate <= 0.0 {
        Some(crate::report::Warning::new(
            TUNING,
            "`[daemon] sampling_rate` is 0: no trace is analyzed, so this \
             report can only ever be empty. Raise it above 0 to get findings",
        ))
    } else if rate < 1.0 {
        Some(crate::report::Warning::new(
            TUNING,
            format!(
                "`[daemon] sampling_rate` is {rate}: only that fraction of \
                 traces is analyzed, so absolute counts (findings, \
                 occurrences, the `perf_sentinel_*` totals) describe a sample \
                 and a rare pattern can be missed entirely. Set it to 1.0 \
                 for whole-traffic counts"
            ),
        ))
    } else {
        None
    }
}

/// Grouping-cap fold rule, one message for the three axes: all three
/// fold rather than drop, and the remedy is the same for each. No knob
/// branch: with `per_grouping_labels` off no path reaches a grouping cap,
/// so the counters cannot move.
fn grouping_fold_warning(metrics: &MetricsState) -> Option<crate::report::Warning> {
    use crate::report::warnings::TUNING;
    let folded = metrics.service_io_ops_grouping_overflow_total.get()
        + metrics.analysis_grouping_overflow_total.get()
        + metrics.slow_duration_grouping_overflow_total.get();
    if folded == 0 {
        return None;
    }
    let ingest_cap = crate::daemon::event_loop::MAX_GROUPING_PAIRS;
    let analysis_cap = crate::daemon::event_loop::MAX_ANALYSIS_GROUPING_PAIRS;
    let histogram_cap = crate::daemon::event_loop::MAX_HISTOGRAM_GROUPING_PAIRS;
    Some(crate::report::Warning::new(
        TUNING,
        format!(
            "{folded} attributions landed in `grouping=\"_other\"` past the \
             per-run (service, grouping) pair caps ({ingest_cap} on the \
             ingest I/O counter, {analysis_cap} on findings and the \
             analysis-side I/O counters, {histogram_cap} on the slow-span \
             histogram): per-service totals stay exact, the per-grouping \
             split does not, trim `[detection] grouping_attributes` or set \
             `per_grouping_labels = false`"
        ),
    ))
}

/// Surface aggregated soft conditions in `Report.warning_details`, on
/// top of the /metrics counters. Operators reading `/api/export/report`
/// do not always scrape Prometheus, so a count of dropped requests
/// visible here gives a fast "is the daemon backpressured?" signal.
///
/// The `tuning` entries are the daemon's settings advisor. Most rules
/// compare a metric against the daemon config frozen at startup. The
/// metrics are lifetime counters, plus the point-in-time `active_traces`
/// gauge for the trace-window rule, which therefore appears and
/// disappears with the load. When a knob looks undersized for the
/// observed load, the rule emits a hint naming the knob, its current
/// value and the suggested adjustment. All inputs are trusted
/// (Prometheus counters and parsed config), so `Warning::new` applies.
///
/// The cold-start branch in `handle_export_report` returns before
/// reaching this helper, so the metric-driven kinds never appear next to
/// `cold_start`. Only [`sampling_rate_warning`], which reads no metric,
/// is emitted on both paths.
// Linear warning collector: one independent rule per tuning/ingestion
// signal. Splitting scatters the rules without clarity gain.
#[allow(clippy::too_many_lines)]
pub(super) fn collect_warning_details(
    metrics: &MetricsState,
    daemon: &crate::config::DaemonConfig,
) -> Vec<crate::report::Warning> {
    use crate::report::warnings::{INGESTION_DROPS, TUNING};

    let mut details = Vec::new();

    // Config-only rule, first so it frames the counts below.
    details.extend(sampling_rate_warning(daemon));

    let dropped = metrics.otlp_rejected_channel_full.get();
    if dropped > 0 {
        details.push(crate::report::Warning::new(
            INGESTION_DROPS,
            format!(
                "{dropped} OTLP requests rejected since daemon start \
                 (channel saturation, see `perf_sentinel_otlp_rejected_total`)"
            ),
        ));
        let cap = daemon.ingest_queue_capacity;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{dropped} OTLP requests hit a full ingest queue: raise \
                 `[daemon] ingest_queue_capacity` (currently {cap}) or \
                 spread ingestion across more daemons"
            ),
        ));
    }

    let mem_rejected = metrics.otlp_rejected_memory_pressure.get();
    if mem_rejected > 0 {
        let pct = daemon.memory_high_water_pct;
        details.push(crate::report::Warning::new(
            INGESTION_DROPS,
            format!(
                "{mem_rejected} OTLP requests rejected since daemon start \
                 (memory high-water, RSS bounded to protect against OOM)"
            ),
        ));
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{mem_rejected} OTLP requests hit the memory guard \
                 (`[daemon] memory_high_water_pct` = {pct}): raise the \
                 container memory limit or spread ingestion across more daemons"
            ),
        ));
    }

    let shed = metrics.analysis_shed_batches_total.get();
    if shed > 0 {
        let cap = daemon.analysis_queue_capacity;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "analysis worker shed {shed} batches since daemon start: \
                 raise `[daemon] analysis_queue_capacity` (currently {cap}) \
                 or give the daemon more CPU so detection keeps up"
            ),
        ));
    }

    #[allow(clippy::cast_precision_loss)]
    let active_cap = daemon.max_active_traces as f64;
    let active = metrics.active_traces.get();
    if active >= active_cap * TUNING_ACTIVE_TRACES_RATIO {
        let cap = daemon.max_active_traces;
        let ttl = daemon.trace_ttl_ms;
        // Derive the displayed percentage from the const so the message
        // cannot drift from the actual threshold.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (TUNING_ACTIVE_TRACES_RATIO * 100.0).round() as u32;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "active traces ({active:.0}) are within {pct}% of `[daemon] \
                 max_active_traces` ({cap}): raise the cap or lower \
                 `trace_ttl_ms` (currently {ttl} ms) so LRU eviction does \
                 not split live traces"
            ),
        ));
    }

    let overflow = metrics.service_io_ops_overflow_total.get();
    if overflow > 0 {
        let cap = crate::daemon::event_loop::MAX_SERVICE_CARDINALITY;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{overflow} I/O operations landed beyond the {cap}-service \
                 metering cap: per-service GreenOps attribution is \
                 undercounting, aggregate or reduce service names upstream"
            ),
        ));
    }

    let folded = metrics.analysis_service_overflow_total.get()
        + metrics.slow_duration_service_overflow_total.get();
    if folded > 0 {
        // Read from the consts so the hint cannot drift from the real
        // caps, same reason as the ingest-side message above.
        let analysis_cap = crate::daemon::event_loop::MAX_ANALYSIS_SERVICE_CARDINALITY;
        let histogram_cap = crate::daemon::event_loop::MAX_HISTOGRAM_SERVICE_CARDINALITY;
        // With the knob off findings and the histogram carry no service
        // label, so only the I/O counters can fold.
        let caps = if daemon.per_service_labels {
            format!(
                "{analysis_cap} on findings and the per-service I/O \
                 counters, {histogram_cap} on the slow-span histogram"
            )
        } else {
            format!(
                "{analysis_cap} on the per-service I/O counters, findings \
                 and the slow-span histogram are unlabeled"
            )
        };
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{folded} analysis-side attributions landed in \
                 `service=\"_other\"` past the per-run service caps \
                 ({caps}): totals stay exact, the per-service split does \
                 not, aggregate or reduce service names upstream"
            ),
        ));
    }

    details.extend(grouping_fold_warning(metrics));

    let evicted = metrics.correlator_pairs_evicted_total.get();
    if daemon.correlation.enabled && evicted > 0 {
        let cap = daemon.correlation.max_tracked_pairs;
        details.push(crate::report::Warning::new(
            TUNING,
            format!(
                "{evicted} service pairs dropped at the correlation cap: \
                 raise [daemon.correlation] max_tracked_pairs (currently \
                 {cap}) or disable correlation on wide topologies"
            ),
        ));
    }

    // A high not_io share is healthy on a well-instrumented fleet
    // exporting all its spans. The actionable signal is ZERO retention:
    // spans keep arriving and not one is analyzable.
    let received = metrics.otlp_spans_received_total.get();
    if received >= TUNING_ZERO_RETENTION_MIN_RECEIVED {
        let filtered = instrumentation_gap_filtered(metrics);
        if filtered >= received {
            details.push(crate::report::Warning::new(
                TUNING,
                format!(
                    "all {received} received OTLP spans were filtered as \
                     non-analyzable (no db.statement, no client-side \
                     http.url): the daemon will never produce findings, \
                     check the instrumentation exports I/O attributes on \
                     CLIENT spans, since a SERVER span carrying a URL is \
                     inbound work, or point instrumented services at this \
                     endpoint"
                ),
            ));
        }
    }

    details
}
