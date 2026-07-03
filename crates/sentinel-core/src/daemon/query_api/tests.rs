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
        ack_store: None,
        toml_acks: Arc::new(HashMap::new()),
        ack_api_key: None,
        daemon_config: crate::config::DaemonConfig::default(),
        energy_backends: EnergyBackendsConfigured::default(),
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
        let _ = correlator.ingest(&[fa], t);
        let mut fb =
            crate::test_helpers::make_finding(follow_up_kind.clone(), detect::Severity::Warning);
        fb.service = "payment-svc".to_string();
        let _ = correlator.ingest(&[fb], t + 1_000);
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
    // Gauge/capacity pairs backing the Trends headroom chart (0.8.8).
    // The test config uses the DaemonConfig defaults, so the caps
    // must round-trip as non-zero values.
    assert!(status["max_active_traces"].as_u64().unwrap() > 0);
    assert!(status["analysis_queue_capacity"].as_u64().unwrap() > 0);
    assert!(status["max_retained_findings"].as_u64().unwrap() > 0);
    assert!(status.get("analysis_queue_depth").is_some());
}

#[tokio::test]
async fn config_exposes_daemon_params_without_secrets() {
    let app = query_api_router(make_state());
    let req = Request::builder()
        .uri("/api/config")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let cfg: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Representative scalars and the correlation sub-block round-trip.
    assert!(cfg["max_active_traces"].as_u64().unwrap() > 0);
    assert_eq!(cfg["environment"], "staging");
    assert!(cfg.get("trace_ttl_ms").is_some());
    assert!(cfg.get("sampling_rate").is_some());
    assert!(cfg.get("correlation_enabled").is_some());
    // Secrets are summarized to booleans, never echoed: no raw key
    // or path fields exist on the response at all.
    assert_eq!(cfg["tls_configured"], false);
    assert_eq!(cfg["ack_api_key_set"], false);
    assert!(cfg.get("api_key").is_none());
    assert!(cfg.get("cert_path").is_none());
    assert!(cfg.get("key_path").is_none());
    assert!(cfg.get("tls").is_none());
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
async fn handle_export_report_propagates_scoring_config_on_cold_start() {
    use crate::score::carbon::ScoringConfig;
    use crate::score::electricity_maps::config::{
        ApiVersion, EmissionFactorType, TemporalGranularity,
    };

    // Cold-start path mirror: even when no events have been ingested
    // yet, an operator pulling /api/export/report must see the
    // Electricity Maps audit chip if EM is configured at startup.
    // Regression-guards the `green_summary.scoring_config.clone_from`
    // call on the cold-start branch.
    let scoring = ScoringConfig {
        api_version: ApiVersion::V4,
        emission_factor_type: EmissionFactorType::Lifecycle,
        temporal_granularity: TemporalGranularity::Hourly,
    };

    let mut state_owned = make_state().clone_for_test();
    state_owned.scoring_config = Some(scoring.clone());
    let state = Arc::new(state_owned);
    // Both counters left at 0, exercises the cold-start branch.

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
    assert_eq!(report.green_summary.scoring_config, Some(scoring));
    assert_eq!(report.warnings.len(), 1);
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

#[tokio::test]
async fn export_report_warning_details_includes_cold_start_kind() {
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
    let report: Report = serde_json::from_slice(&body).expect("parses");
    assert_eq!(report.warning_details.len(), 1);
    assert_eq!(report.warning_details[0].kind, "cold_start");
    assert_eq!(
        report.warning_details[0].message,
        "daemon has not yet processed any events"
    );
}

#[tokio::test]
async fn export_report_warning_details_includes_ingestion_drops_when_counter_positive() {
    let state = make_state();
    // Make the cold-start guard pass so the normal path runs.
    state.metrics.events_processed_total.inc_by(1.0);
    state.metrics.traces_analyzed_total.inc_by(1.0);
    // Pre-load the channel_full counter so the normal path picks
    // it up and surfaces an `ingestion_drops` warning.
    state
        .metrics
        .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
    state
        .metrics
        .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
    state
        .metrics
        .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
    state
        .metrics
        .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);
    state
        .metrics
        .record_otlp_reject(crate::report::metrics::OtlpRejectReason::ChannelFull);

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
    let report: Report = serde_json::from_slice(&body).expect("parses");
    let drops = report
        .warning_details
        .iter()
        .find(|w| w.kind == "ingestion_drops")
        .expect("ingestion_drops warning present");
    assert!(
        drops.message.contains("5 ") && drops.message.contains("OTLP"),
        "message should reference the count and OTLP, got: {}",
        drops.message
    );
    let tuning = report
        .warning_details
        .iter()
        .find(|w| w.kind == "tuning")
        .expect("channel saturation also yields a tuning hint");
    assert!(
        tuning.message.contains("ingest_queue_capacity") && tuning.message.contains("1024"),
        "hint should name the knob and its current value, got: {}",
        tuning.message
    );
}

/// Collect only the `tuning` messages for the given state.
fn tuning_messages(metrics: &MetricsState, daemon: &crate::config::DaemonConfig) -> Vec<String> {
    collect_warning_details(metrics, daemon)
        .into_iter()
        .filter(|w| w.kind == crate::report::warnings::TUNING)
        .map(|w| w.message)
        .collect()
}

#[test]
fn tuning_advisor_stays_silent_on_healthy_counters() {
    let metrics = MetricsState::new();
    assert!(tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty());
}

#[test]
fn tuning_advisor_flags_analysis_sheds_with_queue_capacity() {
    let metrics = MetricsState::new();
    metrics.analysis_shed_batches_total.inc_by(7);
    let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("shed 7 batches")
            && msgs[0].contains("analysis_queue_capacity")
            && msgs[0].contains("1024"),
        "got: {}",
        msgs[0]
    );
}

#[test]
fn tuning_advisor_flags_memory_pressure_rejections() {
    let metrics = MetricsState::new();
    metrics.otlp_rejected_memory_pressure.inc_by(4);
    let daemon = crate::config::DaemonConfig {
        memory_high_water_pct: 80,
        ..crate::config::DaemonConfig::default()
    };
    let msgs = tuning_messages(&metrics, &daemon);
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("memory guard")
            && msgs[0].contains("memory_high_water_pct = 80")
            && msgs[0].contains("container memory limit"),
        "got: {}",
        msgs[0]
    );
}

#[test]
fn tuning_advisor_flags_trace_window_near_cap() {
    let metrics = MetricsState::new();
    metrics.active_traces.set(9_500.0);
    let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("max_active_traces")
            && msgs[0].contains("10000")
            && msgs[0].contains("trace_ttl_ms")
            && msgs[0].contains("30000 ms"),
        "got: {}",
        msgs[0]
    );

    metrics.active_traces.set(8_000.0);
    assert!(
        tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty(),
        "below 90% of the cap must not warn"
    );
}

#[test]
fn tuning_advisor_flags_service_cardinality_overflow() {
    let metrics = MetricsState::new();
    metrics.service_io_ops_overflow_total.inc_by(42);
    let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("42 ") && msgs[0].contains("1024-service"),
        "got: {}",
        msgs[0]
    );
}

#[test]
fn tuning_advisor_flags_pair_evictions_only_when_correlation_enabled() {
    let metrics = MetricsState::new();
    metrics.correlator_pairs_evicted_total.inc_by(900);

    let disabled = crate::config::DaemonConfig::default();
    assert!(!disabled.correlation.enabled, "default is opt-in");
    assert!(
        tuning_messages(&metrics, &disabled).is_empty(),
        "no correlator wired, the counter cannot be actionable"
    );

    let mut enabled = crate::config::DaemonConfig::default();
    enabled.correlation.enabled = true;
    let msgs = tuning_messages(&metrics, &enabled);
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("900 service pairs") && msgs[0].contains("max_tracked_pairs"),
        "got: {}",
        msgs[0]
    );
}

#[test]
fn tuning_advisor_flags_zero_span_retention() {
    let metrics = MetricsState::new();
    metrics.otlp_spans_received_total.inc_by(10_000);
    metrics
        .otlp_spans_filtered_total
        .with_label_values(&["not_io"])
        .inc_by(9_000);
    metrics
        .otlp_spans_filtered_total
        .with_label_values(&["missing_db_statement"])
        .inc_by(1_000);
    let msgs = tuning_messages(&metrics, &crate::config::DaemonConfig::default());
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].contains("all 10000 received") && msgs[0].contains("never produce findings"),
        "got: {}",
        msgs[0]
    );
}

#[test]
fn tuning_advisor_tolerates_dominant_but_partial_filtering() {
    // A well-instrumented fleet exporting every span legitimately
    // filters most of them as not_io. As long as SOME spans are
    // retained, the advisor must stay silent.
    let metrics = MetricsState::new();
    metrics.otlp_spans_received_total.inc_by(10_000);
    metrics
        .otlp_spans_filtered_total
        .with_label_values(&["not_io"])
        .inc_by(9_990);
    assert!(
        tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty(),
        "10 retained spans out of 10000 is a healthy fleet, not a defect"
    );
}

#[test]
fn tuning_advisor_ignores_zero_retention_below_min_volume() {
    let metrics = MetricsState::new();
    metrics.otlp_spans_received_total.inc_by(999);
    metrics
        .otlp_spans_filtered_total
        .with_label_values(&["not_io"])
        .inc_by(999);
    assert!(
        tuning_messages(&metrics, &crate::config::DaemonConfig::default()).is_empty(),
        "under {TUNING_ZERO_RETENTION_MIN_RECEIVED} received spans the signal is noise"
    );
}

/// GET /api/energy against the given state, parsed.
async fn fetch_energy(state: Arc<QueryApiState>) -> EnergyStatusResponse {
    let app = query_api_router(state);
    let req = Request::builder()
        .uri("/api/energy")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&body).expect("parses as EnergyStatusResponse")
}

#[tokio::test]
async fn energy_endpoint_reports_unconfigured_backends_without_metrics() {
    // No backend configured: every row is configured=false with no
    // age/counter fields (the pre-registered zero gauges must not
    // read as a misleading fresh scrape).
    let energy = fetch_energy(make_state()).await;
    assert_eq!(energy.backends.len(), 5);
    for b in &energy.backends {
        assert!(!b.configured, "{} should be unconfigured", b.backend);
        assert!(b.last_scrape_age_seconds.is_none(), "{}", b.backend);
        assert!(b.scrapes_ok.is_none(), "{}", b.backend);
    }
    let names: Vec<&str> = energy.backends.iter().map(|b| b.backend.as_str()).collect();
    assert_eq!(
        names,
        [
            "scaphandre",
            "kepler",
            "redfish",
            "cloud_energy",
            "electricity_maps"
        ]
    );
}

#[tokio::test]
async fn energy_endpoint_reports_configured_backend_metrics() {
    let mut state = (*make_state()).clone_for_test();
    state.energy_backends.scaphandre = true;
    state.metrics.scaphandre_scrape_success.inc_by(7);
    state.metrics.scaphandre_scrape_failed.inc_by(2);
    state.metrics.scaphandre_last_scrape_age_seconds.set(3.5);

    let energy = fetch_energy(Arc::new(state)).await;
    let scaphandre = energy
        .backends
        .iter()
        .find(|b| b.backend == "scaphandre")
        .expect("scaphandre row");
    assert!(scaphandre.configured);
    assert_eq!(scaphandre.scrapes_ok, Some(7));
    assert_eq!(scaphandre.scrapes_failed, Some(2));
    assert!((scaphandre.last_scrape_age_seconds.unwrap() - 3.5).abs() < f64::EPSILON);
    // The others stay unconfigured and field-less.
    let kepler = energy
        .backends
        .iter()
        .find(|b| b.backend == "kepler")
        .unwrap();
    assert!(!kepler.configured);
    assert!(kepler.scrapes_ok.is_none());
}

#[tokio::test]
async fn energy_endpoint_derives_electricity_maps_from_scoring_config() {
    use crate::score::carbon::ScoringConfig;
    use crate::score::electricity_maps::config::{
        ApiVersion, EmissionFactorType, TemporalGranularity,
    };

    let mut state = (*make_state()).clone_for_test();
    state.scoring_config = Some(ScoringConfig {
        api_version: ApiVersion::V4,
        emission_factor_type: EmissionFactorType::Lifecycle,
        temporal_granularity: TemporalGranularity::Hourly,
    });
    let energy = fetch_energy(Arc::new(state)).await;
    let emaps = energy
        .backends
        .iter()
        .find(|b| b.backend == "electricity_maps")
        .expect("electricity_maps row");
    assert!(emaps.configured);
    // No freshness gauge exists for the EM API by design.
    assert!(emaps.last_scrape_age_seconds.is_none());
}

#[allow(clippy::unused_async)]
async fn make_state_with_acks(
    ack_store: Option<Arc<AckStore>>,
    toml_acks: HashMap<String, ResolvedTomlAck>,
    ack_api_key: Option<String>,
) -> Arc<QueryApiState> {
    let mut state = (*make_state_with_correlator(None)).clone_for_test();
    state.ack_store = ack_store;
    state.toml_acks = Arc::new(toml_acks);
    state.ack_api_key = ack_api_key;
    Arc::new(state)
}

async fn fresh_ack_store() -> (tempfile::TempDir, Arc<AckStore>) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = AckStore::new(dir.path().join("acks.jsonl")).await.unwrap();
    (dir, store)
}

/// Test fixture: a TOML baseline ack with no expiry, attributed to
/// the canned `ci-bot` author. Reused across tests that exercise
/// the TOML-wins conflict path.
fn toml_baseline_fixture(sig: &str) -> ResolvedTomlAck {
    ResolvedTomlAck {
        inner: Acknowledgment {
            signature: sig.to_string(),
            acknowledged_by: "ci-bot".to_string(),
            acknowledged_at: "2026-05-04".to_string(),
            reason: "permanent baseline".to_string(),
            expires_at: None,
        },
        expires_at_dt: None,
    }
}

/// Build a POST `/api/findings/{sig}/ack` request with an empty
/// JSON body and no auth headers. Centralizes the boilerplate so
/// the per-test focus is the assertion, not the HTTP setup.
fn post_ack_request(sig: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/findings/{sig}/ack"))
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap()
}

/// Build a DELETE `/api/findings/{sig}/ack` request, no body, no
/// auth headers.
fn delete_ack_request(sig: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(format!("/api/findings/{sig}/ack"))
        .body(Body::empty())
        .unwrap()
}

/// Build a GET request to `path`, no body.
fn get_request(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

async fn seed_finding(state: &Arc<QueryApiState>, service: &str) -> String {
    let mut f = crate::test_helpers::make_finding(
        detect::FindingType::NPlusOneSql,
        detect::Severity::Warning,
    );
    f.service = service.to_string();
    let sig = compute_signature(&f);
    state.findings_store.push_batch(&[f], 1000).await;
    sig
}

#[tokio::test]
async fn ack_endpoint_persists_and_filters_finding() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
    let sig = seed_finding(&state, "order-svc").await;

    let app = query_api_router(Arc::clone(&state));
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/findings/{sig}/ack"))
        .header("Content-Type", "application/json")
        .header("X-User-Id", "alice@example.com")
        .body(Body::from("{\"reason\":\"deferred\"}"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(state.metrics.ack_operations_ack_success.get(), 1);

    let req = Request::builder()
        .uri("/api/findings")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(list.is_empty(), "acked finding should not appear: {list:?}");
}

#[tokio::test]
async fn ack_endpoint_returns_409_when_already_acked() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
    let sig = seed_finding(&state, "order-svc").await;
    let app = query_api_router(Arc::clone(&state));
    let resp = app.clone().oneshot(post_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app.oneshot(post_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        state
            .metrics
            .ack_operations_failed_total
            .with_label_values(&["ack", "already_acked"])
            .get(),
        1
    );
}

#[tokio::test]
async fn unack_endpoint_makes_finding_reappear() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
    let sig = seed_finding(&state, "order-svc").await;
    let app = query_api_router(Arc::clone(&state));

    let resp = app.clone().oneshot(post_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app.clone().oneshot(delete_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(state.metrics.ack_operations_unack_success.get(), 1);

    let resp = app.oneshot(get_request("/api/findings")).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn findings_with_include_acked_annotates_daemon_source() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
    let sig = seed_finding(&state, "order-svc").await;
    let app = query_api_router(Arc::clone(&state));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/findings/{sig}/ack"))
                .header("Content-Type", "application/json")
                .header("X-User-Id", "alice")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(get_request("/api/findings?include_acked=true"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
    let ack = &list[0]["acknowledged_by"];
    assert_eq!(ack["source"], "daemon");
    assert_eq!(ack["by"], "alice");
}

#[tokio::test]
async fn toml_acks_win_over_daemon_on_conflict() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_correlator(None);
    let sig = seed_finding(&state, "order-svc").await;
    let mut toml = HashMap::new();
    toml.insert(sig.clone(), toml_baseline_fixture(&sig));
    let state = make_state_with_acks(Some(store), toml, None).await;
    // Re-seed since make_state_with_acks rebuilt state.
    let sig2 = seed_finding(&state, "order-svc").await;
    assert_eq!(sig, sig2);

    let app = query_api_router(Arc::clone(&state));
    // POST on a TOML-acked signature returns 409 (the daemon will not
    // shadow the immutable baseline with a runtime line that has no
    // visible effect).
    let resp = app.clone().oneshot(post_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // The TOML ack is still surfaced on the read path with
    // `acknowledged_by.source == "toml"`.
    let resp = app
        .oneshot(get_request("/api/findings?include_acked=true"))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["acknowledged_by"]["source"], "toml");
    assert_eq!(list[0]["acknowledged_by"]["acknowledged_by"], "ci-bot");
}

#[tokio::test]
async fn ack_endpoint_requires_api_key_when_configured() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(
        Some(store),
        HashMap::new(),
        Some("a-long-enough-secret".to_string()),
    )
    .await;
    let sig = seed_finding(&state, "order-svc").await;
    let app = query_api_router(Arc::clone(&state));

    // Missing key: 401
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/findings/{sig}/ack"))
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Wrong key: 401
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/findings/{sig}/ack"))
                .header("Content-Type", "application/json")
                .header("X-API-Key", "wrong-key-xxxxxxxxxx")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Correct key: 201
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/findings/{sig}/ack"))
                .header("Content-Type", "application/json")
                .header("X-API-Key", "a-long-enough-secret")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    assert_eq!(
        state
            .metrics
            .ack_operations_failed_total
            .with_label_values(&["ack", "unauthorized"])
            .get(),
        2,
        "missing key + wrong key both bump unauthorized"
    );
    assert_eq!(state.metrics.ack_operations_ack_success.get(), 1);
}

#[tokio::test]
async fn ack_failure_increments_no_store_when_disabled() {
    let state = make_state_with_acks(None, HashMap::new(), None).await;
    let sig = seed_finding(&state, "order-svc").await;
    let app = query_api_router(Arc::clone(&state));

    let resp = app.oneshot(post_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        state
            .metrics
            .ack_operations_failed_total
            .with_label_values(&["ack", "no_store"])
            .get(),
        1
    );
}

#[tokio::test]
async fn toml_conflict_increments_already_acked() {
    let (_dir, store) = fresh_ack_store().await;
    let bootstrap = make_state_with_correlator(None);
    let sig = seed_finding(&bootstrap, "order-svc").await;
    let mut toml = HashMap::new();
    toml.insert(sig.clone(), toml_baseline_fixture(&sig));
    let state = make_state_with_acks(Some(store), toml, None).await;
    let sig2 = seed_finding(&state, "order-svc").await;
    assert_eq!(sig, sig2);

    let app = query_api_router(Arc::clone(&state));
    let resp = app.oneshot(post_ack_request(&sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        state
            .metrics
            .ack_operations_failed_total
            .with_label_values(&["ack", "already_acked"])
            .get(),
        1,
        "TOML conflict bumps the same series as AckError::AlreadyAcked"
    );
}

#[tokio::test]
async fn ack_failure_increments_invalid_signature() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
    let app = query_api_router(Arc::clone(&state));

    // Tail uppercase hex fails the canonical-format check in
    // `daemon::ack::validate_signature` which requires lowercase
    // hex on the trailing 16-char SHA prefix.
    let bad_sig = "foo:bar:0123456789ABCDEF";
    let resp = app.oneshot(post_ack_request(bad_sig)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state
            .metrics
            .ack_operations_failed_total
            .with_label_values(&["ack", "invalid_signature"])
            .get(),
        1
    );
}

#[tokio::test]
async fn list_acks_endpoint_returns_active() {
    let (_dir, store) = fresh_ack_store().await;
    let state = make_state_with_acks(Some(store), HashMap::new(), None).await;
    let sig = seed_finding(&state, "order-svc").await;
    let app = query_api_router(state);

    app.clone().oneshot(post_ack_request(&sig)).await.unwrap();

    let resp = app.oneshot(get_request("/api/acks")).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["signature"], sig);
}

#[test]
fn finding_response_does_not_collide_with_stored_finding_fields() {
    // Pin the JSON shape: `acknowledged_by` lives at the top level
    // alongside the flattened StoredFinding fields, never nested
    // under `finding`. A future refactor that adds an
    // `acknowledged_by` to either StoredFinding or Finding would
    // shadow this and break clients that parse the source field.
    let finding = crate::test_helpers::make_finding(
        detect::FindingType::NPlusOneSql,
        detect::Severity::Warning,
    );
    let resp = FindingResponse {
        stored: StoredFinding {
            finding,
            stored_at_ms: 1234,
        },
        acknowledged_by: Some(AckSource::Daemon {
            by: "alice".to_string(),
            at: Utc::now(),
            reason: None,
            expires_at: None,
        }),
    };
    let v = serde_json::to_value(&resp).unwrap();
    let obj = v
        .as_object()
        .expect("FindingResponse serializes as an object");
    assert!(obj.contains_key("stored_at_ms"));
    assert!(obj.contains_key("finding"));
    assert!(obj.contains_key("acknowledged_by"));
    let inner = obj.get("finding").unwrap().as_object().unwrap();
    assert!(
        !inner.contains_key("acknowledged_by"),
        "acknowledged_by must stay at the top level, not nest inside finding"
    );
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
            ack_store: self.ack_store.clone(),
            toml_acks: Arc::clone(&self.toml_acks),
            ack_api_key: self.ack_api_key.clone(),
            daemon_config: self.daemon_config.clone(),
            energy_backends: self.energy_backends,
        }
    }
}
