//! Prometheus metrics export for daemon mode.
//!
//! Exposes a `/metrics` endpoint on the same axum HTTP server (port 4318)
//! with counters and gauges for monitoring perf-sentinel in real time.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use prometheus::{Counter, CounterVec, Encoder, Gauge, Opts, Registry, TextEncoder};

/// Shared metrics state for the daemon.
#[derive(Clone)]
pub struct MetricsState {
    registry: Registry,
    /// Findings detected, labeled by type and severity.
    pub findings_total: CounterVec,
    /// Current I/O waste ratio (updated on each trace batch).
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
}

impl MetricsState {
    /// Create a new metrics state with all metrics registered.
    ///
    /// # Panics
    ///
    /// Panics if prometheus metric creation or registration fails (should not happen).
    #[must_use]
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
            "Current I/O waste ratio from latest batch",
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

        Self {
            registry,
            findings_total,
            io_waste_ratio,
            traces_analyzed_total,
            events_processed_total,
            active_traces,
            total_io_ops,
            avoidable_io_ops,
        }
    }

    /// Render all metrics in Prometheus text exposition format.
    ///
    /// # Panics
    ///
    /// Panics if encoding fails (should not happen with valid metrics).
    #[must_use]
    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .expect("encoding should not fail");
        String::from_utf8(buffer).expect("prometheus output should be valid UTF-8")
    }
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
    ) -> ([(axum::http::header::HeaderName, &'static str); 1], String) {
        (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            metrics.render(),
        )
    }

    Router::new()
        .route("/metrics", get(handle_metrics))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
