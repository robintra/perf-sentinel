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
use crate::detect::{self, DetectConfig};
use crate::explain;

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
}

/// Build the query API router.
pub fn query_api_router(state: Arc<QueryApiState>) -> Router {
    Router::new()
        .route("/api/findings", get(handle_findings))
        .route("/api/findings/{trace_id}", get(handle_findings_by_trace))
        .route("/api/explain/{trace_id}", get(handle_explain))
        .route("/api/correlations", get(handle_correlations))
        .route("/api/status", get(handle_status))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn make_state() -> Arc<QueryApiState> {
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
            },
            start_time: std::time::Instant::now(),
            correlator: None,
        })
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
        use crate::correlate::window::WindowConfig;
        use crate::detect::correlate_cross::{CorrelationConfig, CrossTraceCorrelator};

        // Build a correlator and ingest a small pattern that should produce
        // a detectable correlation.
        let mut correlator = CrossTraceCorrelator::new(CorrelationConfig {
            min_co_occurrences: 2,
            min_confidence: 0.5,
            lag_threshold_ms: 5_000,
            ..Default::default()
        });
        for i in 0..3 {
            let t = 1_000_000 + i * 10_000;
            let mut fa = crate::test_helpers::make_finding(
                detect::FindingType::NPlusOneSql,
                detect::Severity::Warning,
            );
            fa.service = "order-svc".to_string();
            correlator.ingest(&[fa], t);
            let mut fb = crate::test_helpers::make_finding(
                detect::FindingType::PoolSaturation,
                detect::Severity::Warning,
            );
            fb.service = "payment-svc".to_string();
            correlator.ingest(&[fb], t + 1_000);
        }

        let state = Arc::new(QueryApiState {
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
            },
            start_time: std::time::Instant::now(),
            correlator: Some(Arc::new(tokio::sync::Mutex::new(correlator))),
        });

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
}
