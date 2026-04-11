//! Shared test helpers for sentinel-core unit tests.

use crate::correlate::Trace;
use crate::event::{EventSource, EventType, SpanEvent};
use crate::normalize;

pub fn make_sql_event(trace_id: &str, span_id: &str, target: &str, ts: &str) -> SpanEvent {
    make_sql_event_with_duration(trace_id, span_id, target, ts, 800)
}

pub fn make_http_event(trace_id: &str, span_id: &str, target: &str, ts: &str) -> SpanEvent {
    make_http_event_with_duration(trace_id, span_id, target, ts, 12000)
}

pub fn make_sql_event_with_duration(
    trace_id: &str,
    span_id: &str,
    target: &str,
    ts: &str,
    duration_us: u64,
) -> SpanEvent {
    SpanEvent {
        timestamp: ts.to_string(),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        service: "order-svc".to_string(),
        cloud_region: None,
        event_type: EventType::Sql,
        operation: "SELECT".to_string(),
        target: target.to_string(),
        duration_us,
        source: EventSource {
            endpoint: "POST /api/orders/42/submit".to_string(),
            method: "OrderService::create_order".to_string(),
        },
        status_code: None,
        response_size_bytes: None,
    }
}

pub fn make_http_event_with_duration(
    trace_id: &str,
    span_id: &str,
    target: &str,
    ts: &str,
    duration_us: u64,
) -> SpanEvent {
    SpanEvent {
        timestamp: ts.to_string(),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        service: "order-svc".to_string(),
        cloud_region: None,
        event_type: EventType::HttpOut,
        operation: "GET".to_string(),
        target: target.to_string(),
        duration_us,
        source: EventSource {
            endpoint: "POST /api/orders/42/submit".to_string(),
            method: "OrderService::create_order".to_string(),
        },
        status_code: Some(200),
        response_size_bytes: None,
    }
}

pub fn make_http_event_with_size(
    trace_id: &str,
    span_id: &str,
    target: &str,
    ts: &str,
    response_size_bytes: Option<u64>,
) -> SpanEvent {
    let mut event = make_http_event(trace_id, span_id, target, ts);
    event.response_size_bytes = response_size_bytes;
    event
}

/// Build 3 identical SQL events that trigger a redundant finding
/// (same template AND same params, i.e. exact duplicates).
pub fn make_redundant_events() -> Vec<SpanEvent> {
    (1..=3_i32)
        .map(|i| {
            make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                "SELECT * FROM order_item WHERE order_id = 42",
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            )
        })
        .collect()
}

/// Build `count` SQL events with the same template and different
/// `order_id` params, spaced `stride_ms` milliseconds apart starting
/// from `14:32:01`. Used to construct N+1-style test fixtures with
/// arbitrary cardinality and timing.
pub fn make_sql_series_events_with_stride(count: i32, stride_ms: i32) -> Vec<SpanEvent> {
    (1..=count)
        .map(|i| {
            make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * stride_ms),
            )
        })
        .collect()
}

/// Build `count` SQL events with a default 50ms stride. Shortcut for
/// tests that don't need a specific stride.
pub fn make_sql_series_events(count: i32) -> Vec<SpanEvent> {
    make_sql_series_events_with_stride(count, 50)
}

/// Build 6 SQL events that trigger an N+1 finding (same template,
/// different `order_id` params, within the default 500ms window).
/// Reused across pipeline, score, and `quality_gate` tests.
pub fn make_n_plus_one_events() -> Vec<SpanEvent> {
    make_sql_series_events(6)
}

/// Build a minimal `Finding` with the given type and severity.
/// All other fields use sensible defaults. Tests that need specific
/// values (e.g. a different template or `trace_id`) can mutate the
/// returned struct.
pub fn make_finding(
    finding_type: crate::detect::FindingType,
    severity: crate::detect::Severity,
) -> crate::detect::Finding {
    crate::detect::Finding {
        finding_type,
        severity,
        trace_id: "trace-1".to_string(),
        service: "order-svc".to_string(),
        source_endpoint: "POST /api/orders/42/submit".to_string(),
        pattern: crate::detect::Pattern {
            template: "SELECT * FROM t WHERE id = ?".to_string(),
            occurrences: 6,
            window_ms: 200,
            distinct_params: 6,
        },
        suggestion: "batch".to_string(),
        first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
        green_impact: Some(crate::detect::GreenImpact {
            estimated_extra_io_ops: 5,
            io_intensity_score: 6.0,
        }),
        confidence: crate::detect::Confidence::default(),
    }
}

pub fn make_trace(events: Vec<SpanEvent>) -> Trace {
    assert!(!events.is_empty(), "make_trace requires at least one event");
    let trace_id = events[0].trace_id.clone();
    let spans = normalize::normalize_all(events);
    Trace { trace_id, spans }
}
