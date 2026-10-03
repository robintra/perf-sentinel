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

fn otlp_server_root(span_id: u8, endpoint: &str) -> opentelemetry_proto::tonic::trace::v1::Span {
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
        sanitizer_aware_min_cv: detect::sanitizer_aware::DEFAULT_MIN_CV,
    }
}

fn empty_carbon_ctx() -> score::carbon::CarbonContext {
    score::carbon::CarbonContext::default()
}

/// Zero-capacity store shared by the `process_traces` tests: they
/// assert on findings and metrics. Retention has its own suite.
fn noop_traces_store() -> &'static crate::daemon::traces_store::TracesStore {
    static STORE: std::sync::OnceLock<crate::daemon::traces_store::TracesStore> =
        std::sync::OnceLock::new();
    STORE.get_or_init(|| crate::daemon::traces_store::TracesStore::new(0, 0))
}

/// Build a `ProcessTracesCtx` for tests with sensible defaults.
/// The sticky slot is leaked per call: test-only, a few bytes each.
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
        service_meter: Box::leak(Box::new(AnalysisServiceMeter::new(true, true, metrics))),
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
        slow_window: None,
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
    // 6 events with different params produce an N+1 finding
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
    // 2 events with different templates produce no finding
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

fn slow_trace(trace_id: &str) -> Trace {
    let event = crate::test_helpers::make_sql_event_with_duration(
        trace_id,
        "s1",
        "SELECT * FROM orders WHERE id = 42",
        "2025-07-10T14:32:01.123Z",
        600_000,
    );
    Trace {
        trace_id: trace_id.to_string(),
        spans: vec![normalize::normalize(event)],
    }
}

async fn slow_sql_findings(store: &findings_store::FindingsStore) -> Vec<detect::Finding> {
    let filter = findings_store::FindingsFilter {
        finding_type: Some("slow_sql".to_string()),
        limit: 100,
        ..Default::default()
    };
    store
        .query(&filter)
        .await
        .into_iter()
        .map(|sf| sf.finding)
        .collect()
}

#[tokio::test]
async fn process_traces_emits_cross_batch_slow_finding() {
    let now = current_time_ms();
    let mut tracker = super::super::slow_window::SlowWindowTracker::new(900_000, 60_000, 500, 3);
    tracker.observe(&[slow_trace("a")], &[], now - 300_000);
    tracker.observe(&[slow_trace("b")], &[], now - 120_000);
    let metrics = MetricsState::new();
    let carbon = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    let mut ctx = test_ctx(&detect_config, &carbon, &metrics, &store, true, &cell);
    ctx.slow_window = Some(&mut tracker);
    let c = slow_trace("c");
    process_traces(vec![(c.trace_id, c.spans)], ctx).await;

    let findings = slow_sql_findings(&store).await;
    assert_eq!(findings.len(), 1);
    assert!(!findings[0].signature.is_empty());
    assert_eq!(findings[0].pattern.occurrences, 3);
    assert!(
        metrics
            .render()
            .lines()
            .any(|l| l.starts_with("perf_sentinel_findings_total{") && l.contains("slow_sql"))
    );
}

#[tokio::test]
async fn process_traces_without_slow_window_keeps_batch_behaviour() {
    let metrics = MetricsState::new();
    let carbon = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    let c = slow_trace("c");
    process_traces(
        vec![(c.trace_id, c.spans)],
        test_ctx(&detect_config, &carbon, &metrics, &store, true, &cell),
    )
    .await;
    assert!(slow_sql_findings(&store).await.is_empty());
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
            .with_label_values(&["orders-svc", ""])
            .get()
            .abs()
            < f64::EPSILON
    );
}

#[tokio::test]
async fn consumer_context_batch_names_an_unknown_event() {
    let metrics = MetricsState::new();
    let window = test_window();
    let mut event = make_normalized_for_service("trace-1", "orders-svc", "SELECT 1");
    event.event.source.endpoint = "unknown".to_string();
    event.event.parent_span_id = Some("consumer-1".to_string());
    window.lock().await.push(event, current_time_ms());
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                trace_id: "trace-1".to_string(),
                service: Arc::from("orders-svc"),
                span_id: "consumer-1".to_string(),
                parent_span_id: None,
                endpoint: None,
                consumer_endpoint: Some("rabbitmq crm.dossiers".to_string()),
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
    assert_eq!(trace[0].event.source.endpoint, "rabbitmq crm.dossiers");
}

#[tokio::test]
async fn late_outer_server_replaces_known_nested_route_within_service() {
    let metrics = MetricsState::new();
    let window = test_window();
    let mut nested_sql = make_normalized_for_service("trace-nested", "laravel-svc", "SELECT 1");
    nested_sql.event.span_id = "sql".to_string();
    nested_sql.event.parent_span_id = Some("nested-server".to_string());
    nested_sql.event.source.endpoint = "/api/payments/history".to_string();
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let first = super::super::IngestBatch {
        events: vec![nested_sql.event],
        source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
            consumer_endpoint: None,
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
            consumer_endpoint: None,
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
    let mut nested_sql = make_normalized_for_service("trace-internal", "laravel-svc", "SELECT 1");
    nested_sql.event.span_id = "sql".to_string();
    nested_sql.event.parent_span_id = Some("inner-server".to_string());
    nested_sql.event.source.endpoint = "/api/payments/history".to_string();
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let first = super::super::IngestBatch {
        events: vec![nested_sql.event],
        source_endpoint_updates: vec![
            super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
                trace_id: "trace-internal".to_string(),
                service: Arc::from("laravel-svc"),
                span_id: "inner-server".to_string(),
                parent_span_id: Some("internal".to_string()),
                endpoint: Some("/api/payments/history".to_string()),
            },
            super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
            consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    assert!(
        ingest_event_batch(
            super::super::IngestBatch {
                events: vec![callee_sql.event],
                source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                    consumer_endpoint: None,
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
                    consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);
    let root_batch = |trace_id: &str, endpoint: &str| super::super::IngestBatch {
        events: Vec::new(),
        source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
            consumer_endpoint: None,
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
            make_normalized_messaging("trace-new", &format!("span-{index}"), "root-new", "orders")
                .event
        })
        .collect();
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events,
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events: vec![event],
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events,
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
            make_normalized_messaging("trace-1", &format!("new-{index}"), "root-1", "orders").event
        })
        .collect();
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let started = std::time::Instant::now();
    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events,
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
            make_normalized_messaging("trace-1", &format!("span-{index}"), "root-0", "orders").event
        })
        .collect();
    let source_endpoint_updates = (0..=CAP)
        .map(|index| super::super::SourceEndpointUpdate {
            consumer_endpoint: None,
            trace_id: "trace-1".to_string(),
            service: Arc::from("orders-svc"),
            span_id: format!("root-{index}"),
            parent_span_id: None,
            endpoint: Some(format!("/api/{index}")),
        })
        .collect();
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);
    ingest_event_batch(
        super::super::IngestBatch {
            events: Vec::new(),
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    let evicted = ingest_event_batch(
        super::super::IngestBatch {
            events: vec![trace_b],
            source_endpoint_updates: vec![super::super::SourceEndpointUpdate {
                consumer_endpoint: None,
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);
    for _ in 0..2 {
        let batch = rx.recv().await.expect("daemon ingest batch sent");
        let evicted = ingest_event_batch(batch, 1.0, &window, &metrics, &mut service_meter).await;
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);
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
            .with_label_values(&["orders-svc", ""])
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
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

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
    // Fast path: no scrapers yields Cow::Borrowed, no clone.
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

    // The next delta covers no wall clock already billed and is
    // delivered in full.
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
    // The marker states a fact about the timeline. A stale tick spaced
    // under MIN_BILLABLE_MS bills nothing, so consuming it there would
    // lose the fact and bill the recovery delta twice. With
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
    // MIN_BILLABLE_MS accrues rather than bills, which keeps a busy
    // daemon off the CarbonContext clone.
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
    // The scraper still answers, so liveness stays fresh. Only the
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
    // Alumet sits above Scaphandre in precedence, so a service
    // measured by both must carry Alumet's coefficient and tag. Guards
    // the insertion order in `build_tick_ctx` (reverse precedence,
    // Alumet inserted last).
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
        per_grouping_labels: true,

        confidence: Confidence::DaemonStaging,
        metrics: metrics.clone(),
        findings_store: findings_store.clone(),
        hub_export: None,
        correlator: None,
        green_summary_cell: green_summary_cell.clone(),
        archive_tx: None,
        waste_sticky_ttl_ms: 0,
        slow_window_ms: 0,
        slow_episode_ms: 60_000,
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
    let mut meter = ServiceMeter::new(2, true);

    for service in ["svc-a", "svc-b", "svc-c"] {
        meter.record(service, "", &metrics, 1000.0);
        meter.record(service, "", &metrics, 1000.0);
    }

    // svc-c arrived after the cap: both its ops overflow. The two
    // attributed services keep counting.
    assert_eq!(metrics.service_io_ops_overflow_total.get(), 2);
    for service in ["svc-a", "svc-b"] {
        let count = metrics
            .service_io_ops_total
            .with_label_values(&[service, ""])
            .get();
        assert!((count - 2.0).abs() < f64::EPSILON);
    }
    assert!(meter.capped.warned);
}

#[test]
fn last_span_gauge_follows_the_service_cap_and_the_latest_batch() {
    let metrics = MetricsState::new();
    let mut meter = ServiceMeter::new(1, true);
    meter.record("svc-a", "prod", &metrics, 1_700_000_000.0);
    // A later batch of the same service moves the stamp forward. A
    // frozen gauge would read as an outage.
    meter.record("svc-a", "prod", &metrics, 1_700_000_042.0);
    // Refused by the cap, so it mints no series and cannot widen
    // cardinality past the bound the counter already respects.
    meter.record("svc-b", "prod", &metrics, 1_700_000_042.0);

    let stamp = metrics
        .service_last_span_timestamp_seconds
        .with_label_values(&["svc-a"])
        .get();
    assert!((stamp - 1_700_000_042.0).abs() < f64::EPSILON);
    let output = metrics.render();
    assert!(
        output.contains("perf_sentinel_service_last_span_timestamp_seconds{service=\"svc-a\"}"),
        "the admitted service is exposed: {output}"
    );
    assert!(
        !output.contains("perf_sentinel_service_last_span_timestamp_seconds{service=\"svc-b\"}"),
        "a capped-out service mints no gauge: {output}"
    );
}

#[test]
fn ingest_service_meter_reserves_the_fold_name() {
    let metrics = MetricsState::new();
    let mut meter = ServiceMeter::new(2, true);
    meter.record("svc-a", "", &metrics, 1000.0);

    // `_other` is reserved: counted, but never taking a cap slot.
    meter.record(SERVICE_OVERFLOW_LABEL, "", &metrics, 1000.0);
    assert_eq!(meter.capped.admitted.len(), 1);
    let other = metrics
        .service_io_ops_total
        .with_label_values(&[SERVICE_OVERFLOW_LABEL, ""])
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
            .with_label_values(&[s, ""])
            .get()
    };
    assert!((per_service("svc-a") - 6.0).abs() < f64::EPSILON);
    assert!((per_service("svc-b") - 2.0).abs() < f64::EPSILON);
    let summed = per_service("svc-a") + per_service("svc-b") + per_service(SERVICE_OVERFLOW_LABEL);
    assert!((metrics.total_io_ops.get() - summed).abs() < f64::EPSILON);
}

#[test]
fn analysis_service_meter_folds_past_cap_into_other() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
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
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);

    // A real service named like the fold bucket merges into it
    // without taking a cap slot or counting as overflow.
    assert_eq!(
        meter.service_label(SERVICE_OVERFLOW_LABEL, &metrics),
        SERVICE_OVERFLOW_LABEL
    );
    assert!(meter.names.admitted.is_empty());
    assert_eq!(metrics.analysis_service_overflow_total.get(), 0);
}

#[test]
fn fresh_meter_publishes_no_overflow_histogram_series() {
    // `_other` means "past the cap". Pre-warming it would put a
    // permanent phantom value in every scrape and in the dashboard's
    // pickers, so with either knob on nothing is minted before the
    // first slow span.
    for (per_service, per_grouping) in [(true, true), (true, false), (false, true)] {
        let metrics = MetricsState::new();
        let _meter = AnalysisServiceMeter::new(per_service, per_grouping, &metrics);
        let rendered = metrics.render();
        assert!(
            !rendered.contains("perf_sentinel_slow_duration_seconds_count{"),
            "({per_service}, {per_grouping}): no histogram series before the first slow span: {rendered}"
        );
    }

    // Both knobs off is the 0.17 shape: its single unlabeled series
    // is pre-warmed so "absent" keeps meaning "worker not running".
    let metrics_off = MetricsState::new();
    let _off = AnalysisServiceMeter::new(false, false, &metrics_off);
    assert!(
        metrics_off.render().contains(
            "perf_sentinel_slow_duration_seconds_count{grouping=\"\",service=\"\",type=\"sql\"} 0"
        ),
        "both knobs off pre-warms the unlabeled series as 0.17 did"
    );
}

#[test]
fn analysis_overflow_counts_services_not_endpoint_fanout() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.names.cap = 0;

    // One over-cap service with a wide endpoint fan-out must charge
    // the overflow counter once per attribution row, not once per
    // endpoint: `emit_findings_and_update_metrics` folds first.
    for _ in 0..3 {
        assert_eq!(
            meter.service_label("svc-a", &metrics),
            SERVICE_OVERFLOW_LABEL
        );
    }
    assert_eq!(metrics.analysis_service_overflow_total.get(), 3);
}

#[test]
fn histogram_meter_folds_past_cap_into_other() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.hist_names.cap = 1;

    meter.observe_slow("svc-a", "", &EventType::Sql, 1.0, &metrics);
    meter.observe_slow("svc-b", "", &EventType::Sql, 1.0, &metrics);

    assert_eq!(metrics.slow_duration_service_overflow_total.get(), 1);
    let sample_count = |service: &str| {
        metrics
            .slow_duration_seconds
            .with_label_values(&["sql", service, ""])
            .get_sample_count()
    };
    assert_eq!(sample_count("svc-a"), 1);
    assert_eq!(sample_count(SERVICE_OVERFLOW_LABEL), 1);
}

#[test]
fn per_service_labels_off_uses_empty_label() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(false, true, &metrics);

    // Findings and histogram series carry the empty value...
    assert_eq!(meter.finding_labels("svc-a", "", &metrics), ("", ""));
    meter.observe_slow("svc-a", "", &EventType::Sql, 1.0, &metrics);
    let unlabeled = metrics
        .slow_duration_seconds
        .with_label_values(&["sql", "", ""])
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
        .into_iter()
        .map(|s| {
            metrics
                .service_avoidable_io_ops_total
                .with_label_values(&[s, ""])
                .get()
        })
        .sum();
    assert!((global - summed).abs() < f64::EPSILON);
}

/// A template shared by two services in one trace is one finding
/// owned by the first span's service. The counter charges each
/// service its own repeats, the owner one less for the necessary call.
#[tokio::test]
async fn service_avoidable_io_ops_split_follows_the_spans() {
    let events: Vec<_> = (1..=6)
        .map(|i| {
            make_normalized_for_service(
                "t1",
                if i <= 3 { "svc-a" } else { "svc-b" },
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

    let per = |service: &str| {
        metrics
            .service_avoidable_io_ops_total
            .with_label_values(&[service, ""])
            .get()
    };
    assert!((per("svc-a") - 2.0).abs() < f64::EPSILON);
    assert!((per("svc-b") - 3.0).abs() < f64::EPSILON);
    assert!((metrics.avoidable_io_ops.get() - 5.0).abs() < f64::EPSILON);
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
        // Large interval. Only the immediate first tick can fire, and on
        // an empty/fresh window it is a no-op.
        evict_ms: 60_000,
        slow_window_ms: 0,
        confidence: Confidence::DaemonStaging,
        analysis_queue_capacity: 1024,
        per_service_labels: true,
        per_grouping_labels: true,
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
        // them. The shutdown drain must process them.
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

// --- grouping label (0.19.0) ---

/// An ingest event carrying a grouping, the way the OTLP path leaves
/// it before `ingest_event_batch` meters it.
fn grouped(mut event: normalize::NormalizedEvent, grouping: &str) -> normalize::NormalizedEvent {
    event.event.grouping = crate::test_helpers::k8s_grouping(grouping);
    event
}

fn n_plus_one_trace(
    trace_id: &str,
    service: &str,
    grouping: &str,
    n: usize,
) -> (String, Vec<normalize::NormalizedEvent>) {
    let events = (1..=n)
        .map(|i| {
            grouped(
                make_normalized_for_service(
                    trace_id,
                    service,
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                ),
                grouping,
            )
        })
        .collect();
    (trace_id.to_string(), events)
}

#[test]
fn service_meter_folds_grouping_past_cap_into_other() {
    let metrics = MetricsState::new();
    let mut meter = ServiceMeter::new(2, true);
    meter.pairs.cap = 1;

    for grouping in ["prod", "prod", "staging", "staging"] {
        meter.record("svc-a", grouping, &metrics, 1000.0);
    }

    // Only the grouping axis folded. The service axis never overflowed.
    assert_eq!(metrics.service_io_ops_grouping_overflow_total.get(), 2);
    assert_eq!(metrics.service_io_ops_overflow_total.get(), 0);
    let per = |grouping: &str| {
        metrics
            .service_io_ops_total
            .with_label_values(&["svc-a", grouping])
            .get()
    };
    assert!((per("prod") - 2.0).abs() < f64::EPSILON);
    assert!((per(SERVICE_OVERFLOW_LABEL) - 2.0).abs() < f64::EPSILON);
    assert!(meter.pairs.warned);
}

#[test]
fn ingest_service_meter_reserves_the_grouping_fold_name() {
    let metrics = MetricsState::new();
    let mut meter = ServiceMeter::new(2, true);
    meter.record("svc-a", "prod", &metrics, 1000.0);
    meter.record("svc-a", SERVICE_OVERFLOW_LABEL, &metrics, 1000.0);

    assert_eq!(meter.pairs.len, 1);
    let other = metrics
        .service_io_ops_total
        .with_label_values(&["svc-a", SERVICE_OVERFLOW_LABEL])
        .get();
    assert!((other - 1.0).abs() < f64::EPSILON);
    assert_eq!(metrics.service_io_ops_grouping_overflow_total.get(), 0);
}

#[test]
fn service_meter_sums_across_groupings_to_the_service_total() {
    let metrics = MetricsState::new();
    let mut meter = ServiceMeter::new(2, true);
    for grouping in ["prod", "prod", "staging", "staging", "staging", ""] {
        meter.record("svc-a", grouping, &metrics, 1000.0);
    }

    // What the energy scrapers read: one total per service, the
    // grouping axis folded away, equal to the pre-0.19 value.
    assert_eq!(metrics.snapshot_service_io_ops()["svc-a"], 6);
    let per = |grouping: &str| {
        metrics
            .service_io_ops_total
            .with_label_values(&["svc-a", grouping])
            .get()
    };
    assert!((per("prod") - 2.0).abs() < f64::EPSILON);
    assert!((per("staging") - 3.0).abs() < f64::EPSILON);
    assert!((per("") - 1.0).abs() < f64::EPSILON);
}

#[test]
fn per_grouping_labels_off_uses_empty_label_on_ingest() {
    let metrics = MetricsState::new();
    // Unlike `per_service_labels`, this knob reaches the I/O counters.
    let mut meter = ServiceMeter::new(2, false);
    meter.record("svc-a", "prod", &metrics, 1000.0);

    let unlabeled = metrics
        .service_io_ops_total
        .with_label_values(&["svc-a", ""])
        .get();
    assert!((unlabeled - 1.0).abs() < f64::EPSILON);
    assert!(!metrics.render().contains("grouping=\"prod\""));
    assert_eq!(metrics.service_io_ops_grouping_overflow_total.get(), 0);
}

#[test]
fn analysis_grouping_meter_folds_past_cap_into_other() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.pairs.cap = 2;

    assert_eq!(
        meter.pair_labels("svc-a", "prod", &metrics),
        ("svc-a", "prod")
    );
    assert_eq!(
        meter.pair_labels("svc-a", "staging", &metrics),
        ("svc-a", "staging")
    );
    // Past the pair cap the service stays, the grouping folds.
    assert_eq!(
        meter.pair_labels("svc-a", "dev", &metrics),
        ("svc-a", SERVICE_OVERFLOW_LABEL)
    );
    // Already-admitted pairs keep their own label past the cap.
    assert_eq!(
        meter.pair_labels("svc-a", "prod", &metrics),
        ("svc-a", "prod")
    );
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 1);
    // Every folded call counts: the fold is never memoized.
    assert_eq!(
        meter.pair_labels("svc-a", "dev", &metrics),
        ("svc-a", SERVICE_OVERFLOW_LABEL)
    );
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 2);
    assert_eq!(metrics.analysis_service_overflow_total.get(), 0);
    assert!(meter.pairs.warned);
}

#[test]
fn analysis_grouping_meter_reserves_sentinel_names() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);

    assert_eq!(
        meter.pair_labels("svc-a", SERVICE_OVERFLOW_LABEL, &metrics),
        ("svc-a", SERVICE_OVERFLOW_LABEL)
    );
    assert_eq!(meter.pairs.len, 0);
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 0);
}

#[test]
fn absent_grouping_never_takes_a_cap_slot() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.pairs.cap = 0;
    meter.hist_pairs.cap = 0;

    // A span with no configured attribute renders `grouping=""`,
    // which PromQL reads as no label: it must neither consume a
    // slot nor fold into `_other`.
    assert_eq!(meter.pair_labels("svc-a", "", &metrics), ("svc-a", ""));
    assert_eq!(meter.finding_labels("svc-a", "", &metrics), ("svc-a", ""));
    meter.observe_slow("svc-a", "", &EventType::Sql, 1.0, &metrics);
    assert_eq!(
        metrics
            .slow_duration_seconds
            .with_label_values(&["sql", "svc-a", ""])
            .get_sample_count(),
        1
    );
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 0);
    assert_eq!(metrics.slow_duration_grouping_overflow_total.get(), 0);
}

#[test]
fn pair_cap_counts_pairs_not_grouping_values() {
    // The cap counts admitted (service, grouping) pairs, not grouping
    // values: a third value fits while pairs remain, and a repeated
    // value under a new service is a new pair.
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.pairs.cap = 3;

    assert_eq!(
        meter.pair_labels("svc-a", "ns-1", &metrics),
        ("svc-a", "ns-1")
    );
    assert_eq!(
        meter.pair_labels("svc-b", "ns-2", &metrics),
        ("svc-b", "ns-2")
    );
    assert_eq!(
        meter.finding_labels("svc-c", "ns-3", &metrics),
        ("svc-c", "ns-3")
    );
    // The same grouping under another service is a new pair.
    assert_eq!(
        meter.finding_labels("svc-d", "ns-3", &metrics),
        ("svc-d", SERVICE_OVERFLOW_LABEL)
    );
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 1);
}

#[test]
fn analysis_pair_cap_keys_on_the_effective_service() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.names.cap = 1;
    meter.pairs.cap = 2;

    assert_eq!(
        meter.finding_labels("svc-a", "prod", &metrics),
        ("svc-a", "prod")
    );
    // The service axis folds first. The folded service then owns
    // its own pairs.
    assert_eq!(
        meter.finding_labels("svc-b", "prod", &metrics),
        (SERVICE_OVERFLOW_LABEL, "prod")
    );
    // A second folded service reuses the folded service's pair: no
    // new slot, no grouping overflow. Keyed on the raw service it
    // would be a third pair and fold.
    assert_eq!(
        meter.finding_labels("svc-c", "prod", &metrics),
        (SERVICE_OVERFLOW_LABEL, "prod")
    );
    assert_eq!(meter.pairs.len, 2);
    // Pair cap full: a new grouping under an admitted service folds
    // its grouping half only.
    assert_eq!(
        meter.finding_labels("svc-a", "staging", &metrics),
        ("svc-a", SERVICE_OVERFLOW_LABEL)
    );
    assert_eq!(metrics.analysis_service_overflow_total.get(), 2);
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 1);
}

#[test]
fn histogram_meter_folds_grouping_past_cap_into_other() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.hist_pairs.cap = 1;

    meter.observe_slow("svc-a", "prod", &EventType::Sql, 1.0, &metrics);
    meter.observe_slow("svc-a", "staging", &EventType::Sql, 1.0, &metrics);

    assert_eq!(metrics.slow_duration_grouping_overflow_total.get(), 1);
    assert_eq!(metrics.slow_duration_service_overflow_total.get(), 0);
    let sample_count = |grouping: &str| {
        metrics
            .slow_duration_seconds
            .with_label_values(&["sql", "svc-a", grouping])
            .get_sample_count()
    };
    assert_eq!(sample_count("prod"), 1);
    assert_eq!(sample_count(SERVICE_OVERFLOW_LABEL), 1);
}

#[test]
fn per_grouping_labels_off_uses_empty_label() {
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, false, &metrics);

    assert_eq!(meter.pair_labels("svc-a", "prod", &metrics), ("svc-a", ""));
    assert_eq!(
        meter.finding_labels("svc-a", "prod", &metrics),
        ("svc-a", "")
    );
    meter.observe_slow("svc-a", "prod", &EventType::Sql, 1.0, &metrics);
    assert_eq!(
        metrics
            .slow_duration_seconds
            .with_label_values(&["sql", "svc-a", ""])
            .get_sample_count(),
        1
    );
    // The service half still folds on its own cap.
    assert_eq!(meter.service_label("svc-a", &metrics), "svc-a");
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 0);
}

#[tokio::test]
async fn service_analyzed_io_ops_sums_across_groupings_to_the_service_total() {
    let metrics = MetricsState::new();
    let ctx = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    process_traces(
        vec![
            n_plus_one_trace("t1", "svc-a", "prod", 6),
            n_plus_one_trace("t2", "svc-a", "staging", 2),
            n_plus_one_trace("t3", "svc-b", "prod", 2),
        ],
        test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
    )
    .await;

    let per = |service: &str, grouping: &str| {
        metrics
            .service_analyzed_io_ops_total
            .with_label_values(&[service, grouping])
            .get()
    };
    assert!((per("svc-a", "prod") - 6.0).abs() < f64::EPSILON);
    assert!((per("svc-a", "staging") - 2.0).abs() < f64::EPSILON);
    assert!((per("svc-b", "prod") - 2.0).abs() < f64::EPSILON);
    // Summed over its groupings a service reads its 0.18 value, and
    // summed over everything the counter still equals the global one.
    assert!((per("svc-a", "prod") + per("svc-a", "staging") - 8.0).abs() < f64::EPSILON);
    let all = per("svc-a", "prod") + per("svc-a", "staging") + per("svc-b", "prod");
    assert!((metrics.total_io_ops.get() - all).abs() < f64::EPSILON);
}

#[tokio::test]
async fn service_avoidable_io_ops_sums_across_groupings_to_global_counter() {
    let metrics = MetricsState::new();
    let ctx = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    process_traces(
        vec![
            n_plus_one_trace("t1", "svc-a", "prod", 6),
            n_plus_one_trace("t2", "svc-a", "staging", 6),
        ],
        test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
    )
    .await;

    let per = |grouping: &str| {
        metrics
            .service_avoidable_io_ops_total
            .with_label_values(&["svc-a", grouping])
            .get()
    };
    assert!(per("prod") > 0.0);
    assert!((per("prod") - per("staging")).abs() < f64::EPSILON);
    assert!((metrics.avoidable_io_ops.get() - per("prod") - per("staging")).abs() < f64::EPSILON);
}

#[tokio::test]
async fn service_avoidable_io_ops_split_charges_every_service_under_the_findings_grouping() {
    // t1: one finding owned by svc-a, half its spans from svc-b, all
    // in prod. t2: svc-b alone in staging. svc-b's share of the t1
    // finding must land under prod, the finding's grouping, not
    // under staging or under "".
    let t1: Vec<_> = (1..=6)
        .map(|i| {
            grouped(
                make_normalized_for_service(
                    "t1",
                    if i <= 3 { "svc-a" } else { "svc-b" },
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                ),
                "prod",
            )
        })
        .collect();
    let metrics = MetricsState::new();
    let ctx = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    process_traces(
        vec![
            ("t1".to_string(), t1),
            n_plus_one_trace("t2", "svc-b", "staging", 6),
        ],
        test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
    )
    .await;

    // Read the render first: `with_label_values` would mint the
    // very series this asserts is absent.
    assert!(
        !metrics
            .render()
            .contains("service_avoidable_io_ops_total{grouping=\"\",service=\"svc-b\"")
    );
    let per = |service: &str, grouping: &str| {
        metrics
            .service_avoidable_io_ops_total
            .with_label_values(&[service, grouping])
            .get()
    };
    assert!((per("svc-a", "prod") - 2.0).abs() < f64::EPSILON);
    assert!((per("svc-b", "prod") - 3.0).abs() < f64::EPSILON);
    assert!((per("svc-b", "staging") - 5.0).abs() < f64::EPSILON);
    assert!((metrics.avoidable_io_ops.get() - 10.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn findings_total_carries_the_findings_grouping() {
    let metrics = MetricsState::new();
    let ctx = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    process_traces(
        vec![n_plus_one_trace("t1", "svc-a", "prod", 6)],
        test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
    )
    .await;

    let rendered = metrics.render();
    assert!(
        rendered.contains("perf_sentinel_findings_total{grouping=\"prod\",service=\"svc-a\""),
        "findings_total must carry the finding's grouping: {rendered}"
    );
    assert!(!rendered.contains("perf_sentinel_findings_total{grouping=\"\""));
}

#[tokio::test]
async fn ingest_event_batch_labels_io_ops_with_the_spans_grouping() {
    // The production read of a span's grouping on the ingest side
    // lives in `ingest_event_batch`, not in the meter the other tests
    // drive directly.
    let metrics = MetricsState::new();
    let window = test_window();
    let mut event = crate::test_helpers::make_sql_event_with_duration(
        "trace-g",
        "span-g",
        "SELECT 1",
        "2025-07-10T14:32:01.123Z",
        100,
    );
    event.service = Arc::from("orders-svc");
    event.grouping = crate::test_helpers::k8s_grouping("prod");
    let mut service_meter = ServiceMeter::new(MAX_SERVICE_CARDINALITY, true);

    ingest_event_batch(
        super::super::IngestBatch {
            events: vec![event],
            source_endpoint_updates: vec![],
        },
        1.0,
        &window,
        &metrics,
        &mut service_meter,
    )
    .await;

    assert!(
        !metrics
            .render()
            .contains("service_io_ops_total{grouping=\"\",service=\"orders-svc\"")
    );
    let labeled = metrics
        .service_io_ops_total
        .with_label_values(&["orders-svc", "prod"])
        .get();
    assert!((labeled - 1.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn record_slow_durations_labels_the_histogram_with_the_spans_grouping() {
    // 600 ms against the default 500 ms threshold: the only test
    // that reaches `record_slow_durations` with a slow span.
    let mut event = crate::test_helpers::make_sql_event_with_duration(
        "t1",
        "s1",
        "SELECT 1",
        "2025-07-10T14:32:01.123Z",
        600_000,
    );
    event.grouping = crate::test_helpers::k8s_grouping("prod");
    let metrics = MetricsState::new();
    let ctx = empty_carbon_ctx();
    let store = findings_store::FindingsStore::new(100);
    let detect_config = default_detect_config();
    let cell = fresh_green_cell();
    process_traces(
        vec![("t1".to_string(), vec![normalize::normalize(event)])],
        test_ctx(&detect_config, &ctx, &metrics, &store, true, &cell),
    )
    .await;

    let count = metrics
        .slow_duration_seconds
        .with_label_values(&["sql", "order-svc", "prod"])
        .get_sample_count();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn analysis_worker_honours_per_grouping_labels_off() {
    // The knob travels `DaemonConfig` -> `EventLoopConfig` ->
    // `AnalysisWorkerCtx` -> `AnalysisServiceMeter::new`. Every other
    // test hardcodes it true on both sides, which a swapped argument
    // would satisfy.
    let metrics = Arc::new(MetricsState::new());
    let store = Arc::new(findings_store::FindingsStore::new(100));
    let cell = fresh_green_cell();
    let base = Arc::new(empty_carbon_ctx());
    let sources = no_scrapers(&base);

    let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(4);
    enqueue_for_analysis(
        vec![n_plus_one_trace("t1", "svc-a", "prod", 6)],
        &sources,
        &work_tx,
        &metrics,
    );
    let mut wctx = test_worker_ctx(&metrics, &store, &cell);
    wctx.per_grouping_labels = false;
    let worker = tokio::spawn(run_analysis_worker(work_rx, wctx));
    drop(work_tx);
    worker.await.expect("worker should drain and exit");

    let rendered = metrics.render();
    assert!(
        rendered.contains("perf_sentinel_findings_total{grouping=\"\",service=\"svc-a\""),
        "{rendered}"
    );
    assert!(!rendered.contains("grouping=\"prod\""), "{rendered}");
}

#[tokio::test]
async fn event_loop_honours_per_grouping_labels_off_on_ingest() {
    // The ingest meter is built inside `drive_event_loop` from
    // `loop_cfg.per_grouping_labels`. The batch is sent from the
    // shutdown future so the loop ingests it during the final drain,
    // same shape as the queued-root-context test above.
    let metrics = Arc::new(MetricsState::new());
    let store = Arc::new(findings_store::FindingsStore::new(100));
    let cell = fresh_green_cell();
    let base = Arc::new(empty_carbon_ctx());
    let sources = no_scrapers(&base);
    let window = test_window();

    let mut event = crate::test_helpers::make_sql_event_with_duration(
        "trace-g",
        "span-g",
        "SELECT 1",
        "2025-07-10T14:32:01.123Z",
        100,
    );
    event.service = Arc::from("orders-svc");
    event.grouping = crate::test_helpers::k8s_grouping("prod");

    let (tx, mut rx) = mpsc::channel::<super::super::IngestBatch>(1);
    let shutdown_fut = async move {
        tx.send(super::super::IngestBatch {
            events: vec![event],
            source_endpoint_updates: vec![],
        })
        .await
        .expect("batch queued before shutdown");
    };
    let (work_tx, work_rx) = mpsc::channel::<AnalysisBatch>(4);
    let worker = tokio::spawn(run_analysis_worker(
        work_rx,
        test_worker_ctx(&metrics, &store, &cell),
    ));
    let grpc = tokio::spawn(std::future::pending::<()>());
    let http = tokio::spawn(std::future::pending::<()>());
    let mut loop_cfg = test_loop_cfg();
    loop_cfg.per_grouping_labels = false;

    let result = drive_event_loop(
        &mut rx,
        &window,
        &metrics,
        &sources,
        dummy_shutdown(&grpc, &http),
        loop_cfg,
        work_tx,
        worker,
        shutdown_fut,
    )
    .await;

    assert!(result.is_ok());
    let rendered = metrics.render();
    assert!(
        rendered
            .contains("perf_sentinel_service_io_ops_total{grouping=\"\",service=\"orders-svc\"} 1"),
        "{rendered}"
    );
    // The analysis worker keeps its own knob (true here), so only the
    // ingest family is asserted unlabeled.
    assert!(
        !rendered.contains("perf_sentinel_service_io_ops_total{grouping=\"prod\""),
        "{rendered}"
    );
}

#[test]
fn finding_labels_share_the_pair_slot_when_the_service_knob_is_off() {
    // The pair is keyed on the effective service whatever the knob
    // says, so a finding and its I/O counter rows take one slot and
    // agree on the grouping.
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(false, true, &metrics);
    meter.pairs.cap = 1;

    assert_eq!(
        meter.pair_labels("svc-a", "prod", &metrics),
        ("svc-a", "prod")
    );
    assert_eq!(
        meter.finding_labels("svc-a", "prod", &metrics),
        ("", "prod")
    );
    assert_eq!(meter.pairs.len, 1);
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 0);
    // A new grouping under the same service folds on both paths.
    assert_eq!(
        meter.finding_labels("svc-a", "staging", &metrics),
        ("", SERVICE_OVERFLOW_LABEL)
    );
    assert_eq!(metrics.analysis_grouping_overflow_total.get(), 1);
}

#[test]
fn ingest_pair_cap_keys_on_the_service() {
    // A grouping already admitted under one service is a new pair
    // under another, which distinguishes a pair cap from a value cap
    // on the ingest side.
    let metrics = MetricsState::new();
    let mut meter = ServiceMeter::new(2, true);
    meter.pairs.cap = 1;

    meter.record("svc-a", "prod", &metrics, 1000.0);
    meter.record("svc-b", "prod", &metrics, 1000.0);

    let per = |service: &str, grouping: &str| {
        metrics
            .service_io_ops_total
            .with_label_values(&[service, grouping])
            .get()
    };
    assert!((per("svc-a", "prod") - 1.0).abs() < f64::EPSILON);
    assert!((per("svc-b", SERVICE_OVERFLOW_LABEL) - 1.0).abs() < f64::EPSILON);
    assert_eq!(metrics.service_io_ops_grouping_overflow_total.get(), 1);
    assert_eq!(meter.pairs.len, 1);
}

#[test]
fn histogram_pair_cap_keys_on_the_folded_service() {
    // Services past the histogram cap share the `_other` pairs: a
    // second folded service reuses the first one's slot instead of
    // burning the pair budget on a series that already exists.
    let metrics = MetricsState::new();
    let mut meter = AnalysisServiceMeter::new(true, true, &metrics);
    meter.hist_names.cap = 0;
    meter.hist_pairs.cap = 1;

    meter.observe_slow("svc-b", "prod", &EventType::Sql, 1.0, &metrics);
    meter.observe_slow("svc-c", "prod", &EventType::Sql, 1.0, &metrics);

    let count = metrics
        .slow_duration_seconds
        .with_label_values(&["sql", SERVICE_OVERFLOW_LABEL, "prod"])
        .get_sample_count();
    assert_eq!(count, 2);
    assert_eq!(metrics.slow_duration_grouping_overflow_total.get(), 0);
    assert_eq!(metrics.slow_duration_service_overflow_total.get(), 2);
    assert_eq!(meter.hist_pairs.len, 1);
}
