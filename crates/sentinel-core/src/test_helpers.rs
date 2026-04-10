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

pub fn make_trace(events: Vec<SpanEvent>) -> Trace {
    assert!(!events.is_empty(), "make_trace requires at least one event");
    let trace_id = events[0].trace_id.clone();
    let spans = normalize::normalize_all(events);
    Trace { trace_id, spans }
}
