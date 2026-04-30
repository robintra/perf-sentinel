//! HTTP query API for the daemon's internal state.
//!
//! Exposes findings, trace explanations, correlations, and status
//! alongside the existing `/v1/traces` and `/metrics` endpoints.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::findings_store::{FindingsFilter, FindingsStore, StoredFinding};
use crate::correlate::Trace;
use crate::correlate::window::TraceWindow;
use crate::detect::correlate_cross::{CrossTraceCorrelation, CrossTraceCorrelator};
#[cfg(test)]
use crate::detect::sanitizer_aware::SanitizerAwareMode;
use crate::detect::{self, DetectConfig};
use crate::explain;
use crate::report::metrics::MetricsState;
use crate::report::{Analysis, GreenSummary, QualityGate, Report};

/// Upper bound for `?limit=` on `/api/findings` to protect the daemon
/// from expensive large-response requests.
const MAX_FINDINGS_LIMIT: usize = 1000;

/// Upper bound for `/api/correlations` response size. Same rationale as
/// [`MAX_FINDINGS_LIMIT`]: cap response size under an unauthenticated
/// loopback API. In practice `max_tracked_pairs` (config default `10_000`)
/// already bounds the correlator's memory, but serializing all pairs
/// per poll is still an expensive operation we want to limit.
const MAX_CORRELATIONS_LIMIT: usize = 1000;

/// Shared state for query API route handlers.
pub struct QueryApiState {
    pub findings_store: Arc<FindingsStore>,
    pub window: Arc<tokio::sync::Mutex<TraceWindow>>,
    pub detect_config: DetectConfig,
    pub start_time: std::time::Instant,
    /// Optional cross-trace correlator. `None` when
    /// `[daemon.correlation] enabled = false`.
    pub correlator: Option<Arc<tokio::sync::Mutex<CrossTraceCorrelator>>>,
    /// Shared metrics registry. The `/api/export/report` handler reads
    /// lifetime counters (`events_processed_total`, `traces_analyzed_total`)
    /// to populate the `Report.analysis` fields, and bumps
    /// `export_report_requests_total` per call.
    pub metrics: Arc<MetricsState>,
    /// Active Electricity Maps scoring configuration, copied from the
    /// loaded `Config` at daemon startup. Surfaces on
    /// `/api/export/report` so the audit chip stays visible whenever
    /// Electricity Maps is configured. `None` otherwise.
    pub scoring_config: Option<crate::score::carbon::ScoringConfig>,
    /// Live `GreenSummary` refreshed by the event loop after each
    /// batch. `/api/export/report` clones this cell so the snapshot
    /// reflects the latest CO2 picture (regions, top offenders,
    /// avoidable I/O ratio) instead of `GreenSummary::disabled(0)`.
    /// Initialized to `disabled(0)` at daemon startup. The cold-start
    /// guard (`events_processed == 0 -> 503`) ensures clients never
    /// observe the initial value.
    pub green_summary: Arc<tokio::sync::RwLock<GreenSummary>>,
}

/// Build the query API router.
pub fn query_api_router(state: Arc<QueryApiState>) -> Router {
    Router::new()
        .route("/api/findings", get(handle_findings))
        .route("/api/findings/{trace_id}", get(handle_findings_by_trace))
        .route("/api/explain/{trace_id}", get(handle_explain))
        .route("/api/correlations", get(handle_correlations))
        .route("/api/status", get(handle_status))
        .route("/api/export/report", get(handle_export_report))
        .with_state(state)
}

// ── Query parameters ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct FindingsParams {
    service: Option<String>,
    #[serde(rename = "type")]
    finding_type: Option<String>,
    severity: Option<String>,
    limit: Option<usize>,
}

// ── Response types ────────────────────────────────────────────────

#[derive(Serialize)]
struct StatusResponse {
    version: &'static str,
    uptime_seconds: u64,
    active_traces: usize,
    stored_findings: usize,
}

// ── Handlers ──────────────────────────────────────────────────────

async fn handle_findings(
    State(state): State<Arc<QueryApiState>>,
    Query(params): Query<FindingsParams>,
) -> Json<Vec<StoredFinding>> {
    // Cap the limit to protect the daemon from expensive responses
    // (large JSON serialization under an unauthenticated loopback API).
    let filter = FindingsFilter {
        service: params.service,
        finding_type: params.finding_type,
        severity: params.severity,
        limit: params.limit.unwrap_or(100).min(MAX_FINDINGS_LIMIT),
    };
    Json(state.findings_store.query(&filter).await)
}

async fn handle_findings_by_trace(
    State(state): State<Arc<QueryApiState>>,
    Path(trace_id): Path<String>,
) -> Json<Vec<StoredFinding>> {
    // Cap for defense-in-depth, consistent with `/api/findings`. In normal
    // traffic a trace has a handful of findings, but a pathological trace
    // with hundreds of N+1 clusters is possible; the cap prevents a large
    // serialization under an unauthenticated loopback API.
    let mut results = state.findings_store.by_trace_id(&trace_id).await;
    results.truncate(MAX_FINDINGS_LIMIT);
    Json(results)
}

async fn handle_explain(
    State(state): State<Arc<QueryApiState>>,
    Path(trace_id): Path<String>,
) -> Json<serde_json::Value> {
    // Look up the trace in the window (if still in memory). The clone
    // happens inside the window lock, but is bounded by
    // `max_events_per_trace` (config default 1000) so the critical
    // section stays short. A pathological trace with many spans could
    // briefly block `process_traces`; the `{}` scope releases the lock
    // as soon as the clone completes.
    let maybe_spans = {
        let window = state.window.lock().await;
        window.peek_clone(&trace_id)
    };

    let value = match maybe_spans {
        Some(spans) => {
            let trace = Trace {
                trace_id: trace_id.clone(),
                spans,
            };
            let findings = detect::detect(std::slice::from_ref(&trace), &state.detect_config);
            let tree = explain::build_tree(&trace, &findings);
            // Serialize directly to Value (one allocation) instead of
            // to_string + from_str (three allocations).
            serde_json::to_value(&tree)
                .unwrap_or_else(|_| serde_json::json!({"error": "failed to format explain tree"}))
        }
        None => serde_json::json!({"error": "trace not found in daemon memory"}),
    };
    Json(value)
}

async fn handle_correlations(
    State(state): State<Arc<QueryApiState>>,
) -> Json<Vec<CrossTraceCorrelation>> {
    match &state.correlator {
        Some(correlator) => {
            let mut correlations = correlator.lock().await.active_correlations();
            // Cap response size. Sort by confidence descending so the
            // most-significant correlations survive the truncation.
            // `f64::total_cmp` provides a total order and handles NaN
            // deterministically (NaN sorts last), so we do not need
            // `partial_cmp(...).unwrap_or(Equal)` to guard invariants.
            correlations.sort_by(|a, b| {
                b.confidence
                    .total_cmp(&a.confidence)
                    .then_with(|| b.co_occurrence_count.cmp(&a.co_occurrence_count))
            });
            correlations.truncate(MAX_CORRELATIONS_LIMIT);
            Json(correlations)
        }
        None => Json(vec![]),
    }
}

async fn handle_status(State(state): State<Arc<QueryApiState>>) -> Json<StatusResponse> {
    let uptime = state.start_time.elapsed().as_secs();
    let active_traces = state.window.lock().await.active_traces();
    let stored_findings = state.findings_store.len().await;
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: uptime,
        active_traces,
        stored_findings,
    })
}

/// Snapshot the daemon's in-memory state as a [`Report`].
///
/// Returns the same JSON shape that `analyze --format json` produces,
/// which allows piping the response directly into `perf-sentinel
/// report --input -` to materialize an HTML dashboard from a live
/// daemon:
///
/// ```text
/// curl -s http://daemon.internal:4318/api/export/report \
///     | perf-sentinel report --input - --output report.html
/// ```
///
/// `green_summary` is refreshed by the event loop after each completed
/// batch (regions, top offenders, avoidable I/O ratio, CO2 numbers).
/// **Per-batch view:** every numeric field under `green_summary`
/// (`total_io_ops`, `avoidable_io_ops`, `io_waste_ratio`, `co2.*`,
/// `regions`, `top_offenders`, `transport_gco2`) reflects the most
/// recent batch only, not a daemon-lifetime aggregate. The
/// `analysis.events_processed` and `analysis.traces_analyzed` fields
/// are lifetime counters for context. Operators wanting cumulative
/// `GreenOps` numbers should scrape `/metrics` (Prometheus counters
/// `total_io_ops`, `avoidable_io_ops`, `io_waste_ratio`).
///
/// The HTML dashboard's `GreenOps` tab renders only when
/// `green_summary.co2` is non-null, daemons configured with Electricity
/// Maps surface the chip banner naturally once at least one batch has
/// been processed.
///
/// `analysis.duration_ms` is `0`, not daemon uptime. The batch-pipeline
/// value is the cost of a single analysis run, a daemon snapshot has
/// no such single run to time.
///
/// Cold start returns `200 OK` with an empty `Report` envelope. Pre-0.5.16
/// this path returned `503 Service Unavailable`, which tripped Kubernetes
/// probes and confused CI scripts that treated 5xx as a daemon health
/// problem. The empty envelope (`findings: []`,
/// `green_summary: GreenSummary::disabled(0)`,
/// `warnings: ["daemon has not yet processed any events"]`) lets clients
/// detect cold-start without a status code mismatch, while preserving the
/// double-counter check (`events_processed_total > 0` AND
/// `traces_analyzed_total > 0`) so the snapshot is meaningful: events can
/// be ingested seconds before the first eviction tick fires
/// (`trace_ttl_ms / 2`, default 15s), gating only on `events_processed > 0`
/// would expose a window where the cell is still `disabled(0)`. The
/// `export_report_requests_total` counter is bumped before the cold-start
/// check, so cold-start responses are counted too (consistent with HTTP
/// access-log conventions).
///
/// Response size is bounded by `MAX_FINDINGS_LIMIT` + `MAX_CORRELATIONS_LIMIT`
/// (1000 + 1000 entries) plus a bounded `green_summary` (`top_offenders`
/// capped, `regions` limited by cloud-region cardinality), worst-case
/// body ~3.5 MB. Acceptable on a loopback bind (the documented
/// posture), review the cap if the daemon is ever bound to a
/// non-loopback interface.
///
/// TODO: the `Report` assembly below duplicates the one in
/// `pipeline::analyze`. When a third call site lands, factor into
/// `report::build_report(...)` and call it from both.
async fn handle_export_report(State(state): State<Arc<QueryApiState>>) -> Json<Report> {
    state.metrics.export_report_requests_total.inc();

    // Prometheus counters are f64 internally. Daemon-lifetime counts
    // easily fit in u64 and we never decrement, so a saturating cast
    // via `as` is safe. The two reads are not atomic as a pair, a
    // concurrent `inc_by` in the event loop could race between them,
    // the values are monotonic and informational so the worst case is
    // a report where `events_processed > 0` and `traces_analyzed = 0`
    // for a few microseconds around the first batch.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let events_processed = state.metrics.events_processed_total.get() as u64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let traces_analyzed = state.metrics.traces_analyzed_total.get() as u64;

    // Cold-start path: gating on both counters keeps the snapshot
    // self-consistent (the cell holds at least one real batch result by
    // the time `traces_analyzed > 0`). Return an empty envelope with a
    // warning string so consumers can distinguish "no events yet" from
    // "events exist, zero findings" without a 5xx HTTP status.
    if events_processed == 0 || traces_analyzed == 0 {
        let mut green_summary = GreenSummary::disabled(0);
        green_summary
            .scoring_config
            .clone_from(&state.scoring_config);
        return Json(Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: 0,
                traces_analyzed: 0,
            },
            findings: Vec::new(),
            green_summary,
            quality_gate: QualityGate {
                passed: true,
                rules: Vec::new(),
            },
            per_endpoint_io_ops: Vec::new(),
            correlations: Vec::new(),
            warnings: vec!["daemon has not yet processed any events".to_string()],
        });
    }

    // Snapshot findings. Cap at MAX_FINDINGS_LIMIT to mirror
    // `/api/findings`, a huge ring buffer should not serialize into
    // an unbounded response body.
    let stored = state
        .findings_store
        .query(&FindingsFilter {
            service: None,
            finding_type: None,
            severity: None,
            limit: MAX_FINDINGS_LIMIT,
        })
        .await;
    let findings: Vec<_> = stored.into_iter().map(|s| s.finding).collect();

    // Snapshot correlations, sorted + capped identically to
    // `/api/correlations` so both endpoints stay consistent.
    let correlations = if let Some(correlator) = &state.correlator {
        let mut list = correlator.lock().await.active_correlations();
        list.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| b.co_occurrence_count.cmp(&a.co_occurrence_count))
        });
        list.truncate(MAX_CORRELATIONS_LIMIT);
        list
    } else {
        vec![]
    };

    // Read the live `GreenSummary` populated by the event loop after
    // each batch. `scoring_config` is re-applied on top: the event
    // loop emits the per-batch summary without the audit-trail
    // metadata, the handler stitches it back from the daemon's
    // startup config (same pattern as 0.5.12 for the chip banner).
    let mut green_summary = state.green_summary.read().await.clone();
    green_summary
        .scoring_config
        .clone_from(&state.scoring_config);
    let quality_gate = QualityGate {
        passed: true,
        rules: vec![],
    };

    // usize::try_from guards 32-bit targets where a 5-billion-event
    // counter would overflow a usize. On 64-bit the fallback branch is
    // unreachable in practice (2^63 events at 1 M/s = 290 000 years).
    // When we do saturate on 32-bit, surface a warning: a usize::MAX
    // counter in the dashboard is far more misleading than silent
    // truncation to an observably large number would be, so the log
    // record is the user's only signal that the field is saturated.
    let events_usize = usize::try_from(events_processed).unwrap_or_else(|_| {
        tracing::warn!(
            counter = events_processed,
            "events_processed overflowed usize on this target, saturating in export"
        );
        usize::MAX
    });
    let traces_usize = usize::try_from(traces_analyzed).unwrap_or_else(|_| {
        tracing::warn!(
            counter = traces_analyzed,
            "traces_analyzed overflowed usize on this target, saturating in export"
        );
        usize::MAX
    });

    let report = Report {
        analysis: Analysis {
            // Explicitly zero rather than the daemon uptime, see the
            // doc comment above for the rationale.
            duration_ms: 0,
            events_processed: events_usize,
            traces_analyzed: traces_usize,
        },
        findings,
        green_summary,
        quality_gate,
        per_endpoint_io_ops: vec![],
        correlations,
        warnings: vec![],
    };

    Json(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Build a `QueryApiState` for tests, wiring an optional correlator.
    /// The three concrete test-site constructions only differed by the
    /// correlator slot (None, Some(A), Some(B)); every other field used
    /// the same test defaults.
    fn make_state_with_correlator(
        correlator: Option<Arc<tokio::sync::Mutex<CrossTraceCorrelator>>>,
    ) -> Arc<QueryApiState> {
        use crate::correlate::window::WindowConfig;

        Arc::new(QueryApiState {
            findings_store: Arc::new(FindingsStore::new(100)),
            window: Arc::new(tokio::sync::Mutex::new(TraceWindow::new(
                WindowConfig::default(),
            ))),
            detect_config: DetectConfig {
                n_plus_one_threshold: 5,
                window_ms: 500,
                slow_threshold_ms: 500,
                slow_min_occurrences: 3,
                max_fanout: 20,
                chatty_service_min_calls: 15,
                pool_saturation_concurrent_threshold: 10,
                serialized_min_sequential: 3,
                sanitizer_aware_classification: SanitizerAwareMode::default(),
            },
            start_time: std::time::Instant::now(),
            correlator,
            metrics: Arc::new(MetricsState::new()),
            scoring_config: None,
            green_summary: Arc::new(tokio::sync::RwLock::new(GreenSummary::disabled(0))),
        })
    }

    fn make_state() -> Arc<QueryApiState> {
        make_state_with_correlator(None)
    }

    /// Seed a correlator with three rounds of paired events (an
    /// `NPlusOneSql` on `order-svc` immediately followed by
    /// `follow_up_kind` on `payment-svc`, 1 ms apart, 10 s between
    /// rounds). The shape is tuned to the default `min_co_occurrences
    /// = 2` + `min_confidence = 0.5` config used by the two tests
    /// that need at least one active correlation in the result.
    fn seed_correlator_with_pair(
        correlator: &mut CrossTraceCorrelator,
        follow_up_kind: &detect::FindingType,
    ) {
        for i in 0..3 {
            let t = 1_000_000 + i * 10_000;
            let mut fa = crate::test_helpers::make_finding(
                detect::FindingType::NPlusOneSql,
                detect::Severity::Warning,
            );
            fa.service = "order-svc".to_string();
            correlator.ingest(&[fa], t);
            let mut fb = crate::test_helpers::make_finding(
                follow_up_kind.clone(),
                detect::Severity::Warning,
            );
            fb.service = "payment-svc".to_string();
            correlator.ingest(&[fb], t + 1_000);
        }
    }

    #[tokio::test]
    async fn status_returns_200() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(status.get("version").is_some());
        assert!(status.get("uptime_seconds").is_some());
        assert!(status.get("active_traces").is_some());
        assert!(status.get("stored_findings").is_some());
    }

    #[tokio::test]
    async fn findings_returns_empty_array() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/findings")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let findings: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(findings.is_empty());
    }

    #[tokio::test]
    async fn correlations_returns_empty_array() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/correlations")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let correlations: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(correlations.is_empty());
    }

    #[tokio::test]
    async fn explain_unknown_trace_returns_error() {
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/explain/nonexistent-trace")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(val.get("error").is_some());
    }

    #[tokio::test]
    async fn findings_returns_populated_store() {
        let state = make_state();
        // Push a finding into the store.
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        state.findings_store.push_batch(&[finding], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/findings")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].finding.finding_type,
            detect::FindingType::NPlusOneSql
        );
    }

    #[tokio::test]
    async fn findings_filters_by_service() {
        let state = make_state();
        let mut f1 = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f1.service = "order-svc".to_string();
        let mut f2 = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f2.service = "payment-svc".to_string();
        state.findings_store.push_batch(&[f1, f2], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/findings?service=order-svc")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].finding.service, "order-svc");
    }

    #[tokio::test]
    async fn findings_by_trace_id() {
        let state = make_state();
        let mut f1 = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        f1.trace_id = "trace-abc".to_string();
        let mut f2 = crate::test_helpers::make_finding(
            detect::FindingType::RedundantSql,
            detect::Severity::Info,
        );
        f2.trace_id = "trace-xyz".to_string();
        state.findings_store.push_batch(&[f1, f2], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/findings/trace-abc")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].finding.trace_id, "trace-abc");
    }

    #[test]
    fn stored_finding_serde_roundtrip() {
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        let stored = StoredFinding {
            finding,
            stored_at_ms: 12345,
        };
        let json = serde_json::to_string(&stored).unwrap();
        let back: StoredFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stored_at_ms, 12345);
        assert_eq!(back.finding.finding_type, detect::FindingType::NPlusOneSql);
    }

    #[tokio::test]
    async fn correlations_returns_active_correlations_when_correlator_present() {
        use crate::detect::correlate_cross::{CorrelationConfig, CrossTraceCorrelator};

        // Build a correlator and ingest a small pattern that should produce
        // a detectable correlation.
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            min_co_occurrences: 2,
            min_confidence: 0.5,
            lag_threshold_ms: 5_000,
            ..Default::default()
        });
        seed_correlator_with_pair(&mut correlator, &detect::FindingType::PoolSaturation);

        let state = make_state_with_correlator(Some(Arc::new(tokio::sync::Mutex::new(correlator))));

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/correlations")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let correlations: Vec<CrossTraceCorrelation> = serde_json::from_slice(&body).unwrap();
        assert!(
            !correlations.is_empty(),
            "expected at least one correlation to be returned"
        );
    }

    #[tokio::test]
    async fn findings_limit_is_capped() {
        let state = make_state();
        // Push more findings than the hard cap (MAX_FINDINGS_LIMIT = 1000).
        let findings: Vec<detect::Finding> = (0..50)
            .map(|i| {
                let mut f = crate::test_helpers::make_finding(
                    detect::FindingType::NPlusOneSql,
                    detect::Severity::Warning,
                );
                f.trace_id = format!("trace-{i}");
                f
            })
            .collect();
        state.findings_store.push_batch(&findings, 1000).await;

        let app = query_api_router(state);
        // Request a huge limit: handler should cap it to MAX_FINDINGS_LIMIT.
        let req = Request::builder()
            .uri("/api/findings?limit=100000")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Store has 50, cap is 1000: response should be 50 (bounded by store).
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let stored: Vec<StoredFinding> = serde_json::from_slice(&body).unwrap();
        assert_eq!(stored.len(), 50);
    }

    #[tokio::test]
    async fn handle_export_report_returns_200_with_empty_envelope_on_cold_start() {
        // No events processed yet: the daemon has nothing meaningful
        // to snapshot. Returns 200 with an empty Report envelope and a
        // `warnings` entry. Pre-0.5.16 returned 503, which tripped
        // Kubernetes probes. The empty shape lets clients distinguish
        // "no events yet" from "ran and found nothing" without a 5xx.
        let app = query_api_router(make_state());
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report =
            serde_json::from_slice(&body).expect("cold-start body must parse as Report");
        assert_eq!(report.analysis.events_processed, 0);
        assert_eq!(report.analysis.traces_analyzed, 0);
        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.green_summary.total_io_ops, 0);
        assert_eq!(
            report.warnings,
            vec!["daemon has not yet processed any events".to_string()]
        );
    }

    #[tokio::test]
    async fn handle_export_report_returns_report_shape_when_events_ingested() {
        use crate::detect::correlate_cross::{CorrelationConfig, CrossTraceCorrelator};

        // Build a state whose lifetime counters are non-zero, whose
        // findings store has at least one entry, and whose correlator
        // holds at least one correlation. That exercises every slot
        // the handler assembles into the Report. The correlator config
        // mirrors `correlations_returns_active_correlations_when_correlator_present`
        // so a handful of co-occurrences is enough to clear the confidence bar.
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            min_co_occurrences: 2,
            min_confidence: 0.5,
            lag_threshold_ms: 5_000,
            ..Default::default()
        });
        seed_correlator_with_pair(&mut correlator, &detect::FindingType::SlowHttp);

        let state = make_state_with_correlator(Some(Arc::new(tokio::sync::Mutex::new(correlator))));

        // Populate both counters + findings store so the handler sees
        // a non-cold-start signal and has a finding to emit.
        state.metrics.events_processed_total.inc_by(42.0);
        state.metrics.traces_analyzed_total.inc_by(5.0);
        let finding = crate::test_helpers::make_finding(
            detect::FindingType::NPlusOneSql,
            detect::Severity::Warning,
        );
        state.findings_store.push_batch(&[finding], 1000).await;

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");

        assert_eq!(report.analysis.events_processed, 42);
        assert_eq!(report.analysis.traces_analyzed, 5);
        // duration_ms is intentionally 0 on the export path (see
        // handler doc), not the daemon uptime that an
        // `as_millis()` would produce.
        assert_eq!(report.analysis.duration_ms, 0);
        // The shared green_summary cell is initialized to disabled(0)
        // and this test does not seed it, so total_io_ops stays at 0.
        // The live-write path is exercised by
        // `handle_export_report_serves_live_green_summary_after_batch`.
        assert_eq!(report.green_summary.total_io_ops, 0);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.correlations.len(), 1);
        assert_eq!(report.correlations[0].source.service, "order-svc");
        assert_eq!(report.correlations[0].target.service, "payment-svc");
    }

    #[tokio::test]
    async fn handle_export_report_propagates_scoring_config_when_emaps_configured() {
        use crate::score::carbon::ScoringConfig;
        use crate::score::electricity_maps::config::{
            ApiVersion, EmissionFactorType, TemporalGranularity,
        };

        // Daemon path mirror: the daemon does not run scoring on the
        // /api/export/report snapshot, but the Electricity Maps client
        // configuration is known at startup. The handler must surface
        // it on green_summary.scoring_config so an operator pulling
        // the snapshot does not mistakenly conclude EM is off.
        let scoring = ScoringConfig {
            api_version: ApiVersion::V4,
            emission_factor_type: EmissionFactorType::Direct,
            temporal_granularity: TemporalGranularity::FifteenMinutes,
        };

        let mut state_owned = make_state().clone_for_test();
        state_owned.scoring_config = Some(scoring.clone());
        let state = Arc::new(state_owned);
        state.metrics.events_processed_total.inc_by(1.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert_eq!(report.green_summary.scoring_config, Some(scoring));
    }

    #[tokio::test]
    async fn handle_export_report_returns_200_with_warnings_when_events_in_but_no_batch_yet() {
        // Cold-start tail: events have been ingested
        // (`events_processed_total > 0`) but the first eviction tick
        // has not fired, so `traces_analyzed_total == 0` and the
        // green_summary cell is still `disabled(0)`. The handler must
        // serve the empty envelope (not 503) to avoid tripping
        // Kubernetes probes during this transient window.
        let state = make_state();
        state.metrics.events_processed_total.inc_by(5.0);
        // traces_analyzed_total left at 0
        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        // The handler clamps `events_processed` to 0 on cold-start since
        // the snapshot is meant to look "empty", regardless of the raw
        // counter that may have already incremented.
        assert_eq!(report.analysis.events_processed, 0);
        assert_eq!(report.analysis.traces_analyzed, 0);
        assert_eq!(
            report.warnings,
            vec!["daemon has not yet processed any events".to_string()]
        );
    }

    #[tokio::test]
    async fn handle_export_report_serves_live_green_summary_after_batch() {
        // The cell is mutated by the event loop after each batch. The
        // handler must clone that cell instead of emitting
        // GreenSummary::disabled(0), so live daemon snapshots carry
        // the latest CO2 picture.
        let state = make_state();
        state.metrics.events_processed_total.inc_by(10.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);

        {
            let mut guard = state.green_summary.write().await;
            guard.total_io_ops = 42;
            guard.avoidable_io_ops = 7;
            guard.io_waste_ratio = 0.166;
        }

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert_eq!(report.green_summary.total_io_ops, 42);
        assert_eq!(report.green_summary.avoidable_io_ops, 7);
        assert!((report.green_summary.io_waste_ratio - 0.166).abs() < 1e-9);
    }

    #[tokio::test]
    async fn handle_export_report_omits_scoring_config_when_emaps_not_configured() {
        // Symmetric guard: when EM is not configured at daemon
        // startup, the snapshot must not advertise a methodology.
        let state = make_state();
        state.metrics.events_processed_total.inc_by(1.0);
        state.metrics.traces_analyzed_total.inc_by(1.0);

        let app = query_api_router(state);
        let req = Request::builder()
            .uri("/api/export/report")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let report: Report = serde_json::from_slice(&body).expect("body parses as Report");
        assert!(report.green_summary.scoring_config.is_none());
    }

    impl QueryApiState {
        /// Test-only shallow clone, mirrors every slot via Arc cloning.
        /// `scoring_config` is the only field the new test mutates,
        /// every other field is shared with the original Arc.
        fn clone_for_test(&self) -> Self {
            Self {
                findings_store: Arc::clone(&self.findings_store),
                window: Arc::clone(&self.window),
                detect_config: self.detect_config.clone(),
                start_time: self.start_time,
                correlator: self.correlator.clone(),
                metrics: Arc::clone(&self.metrics),
                scoring_config: self.scoring_config.clone(),
                green_summary: Arc::clone(&self.green_summary),
            }
        }
    }
}
