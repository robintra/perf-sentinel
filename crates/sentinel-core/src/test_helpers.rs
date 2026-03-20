//! Shared test helpers for sentinel-core unit tests.

use crate::correlate::Trace;
use crate::event::{EventSource, EventType, SpanEvent};
use crate::normalize;

pub fn make_sql_event(trace_id: &str, span_id: &str, target: &str, ts: &str) -> SpanEvent {
    SpanEvent {
        timestamp: ts.to_string(),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        service: "game".to_string(),
        event_type: EventType::Sql,
        operation: "SELECT".to_string(),
        target: target.to_string(),
        duration_us: 800,
        source: EventSource {
            endpoint: "POST /api/game/42/start".to_string(),
            method: "GameService::start_game".to_string(),
        },
        status_code: None,
    }
}

pub fn make_http_event(trace_id: &str, span_id: &str, target: &str, ts: &str) -> SpanEvent {
    SpanEvent {
        timestamp: ts.to_string(),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        service: "game".to_string(),
        event_type: EventType::HttpOut,
        operation: "GET".to_string(),
        target: target.to_string(),
        duration_us: 12000,
        source: EventSource {
            endpoint: "POST /api/game/42/start".to_string(),
            method: "GameService::start_game".to_string(),
        },
        status_code: Some(200),
    }
}

pub fn make_trace(events: Vec<SpanEvent>) -> Trace {
    assert!(!events.is_empty(), "make_trace requires at least one event");
    let trace_id = events[0].trace_id.clone();
    let spans = normalize::normalize_all(events);
    Trace { trace_id, spans }
}
