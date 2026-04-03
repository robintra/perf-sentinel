//! Normalization stage: canonicalizes SQL queries and HTTP URLs.

pub mod http;
pub mod sql;

use crate::event::{EventType, MAX_ID_LENGTH, SpanEvent, sanitize_id};

/// A span event enriched with its normalized template and extracted parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedEvent {
    pub event: SpanEvent,
    pub template: String,
    pub params: Vec<String>,
}

/// Normalize a single event by dispatching on its type.
///
/// Also sanitizes `trace_id` and `span_id` to enforce maximum length.
#[must_use]
pub fn normalize(mut event: SpanEvent) -> NormalizedEvent {
    // Enforce ID length limits at the normalization boundary
    if event.trace_id.len() > MAX_ID_LENGTH {
        event.trace_id = sanitize_id(&event.trace_id);
    }
    if event.span_id.len() > MAX_ID_LENGTH {
        event.span_id = sanitize_id(&event.span_id);
    }
    if let Some(ref pid) = event.parent_span_id
        && pid.len() > MAX_ID_LENGTH
    {
        event.parent_span_id = Some(sanitize_id(pid));
    }
    match event.event_type {
        EventType::Sql => {
            let result = sql::normalize_sql(&event.target);
            NormalizedEvent {
                event,
                template: result.template,
                params: result.params,
            }
        }
        EventType::HttpOut => {
            let result = http::normalize_http(&event.operation, &event.target);
            NormalizedEvent {
                event,
                template: result.template,
                params: result.params,
            }
        }
    }
}

/// Normalize a batch of events.
#[must_use]
pub fn normalize_all(events: Vec<SpanEvent>) -> Vec<NormalizedEvent> {
    events.into_iter().map(normalize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};

    fn make_sql_event(target: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: "trace-1".to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
        }
    }

    fn make_http_event(method: &str, target: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: "trace-1".to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            event_type: EventType::HttpOut,
            operation: method.to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: Some(200),
        }
    }

    #[test]
    fn normalize_dispatches_sql() {
        let event = make_sql_event("SELECT * FROM users WHERE id = 42");
        let normalized = normalize(event);
        assert_eq!(normalized.template, "SELECT * FROM users WHERE id = ?");
        assert_eq!(normalized.params, vec!["42"]);
    }

    #[test]
    fn normalize_dispatches_http() {
        let event = make_http_event("GET", "/api/users/42");
        let normalized = normalize(event);
        assert_eq!(normalized.template, "GET /api/users/{id}");
    }

    #[test]
    fn normalize_all_processes_batch() {
        let events = vec![
            make_sql_event("SELECT 1"),
            make_http_event("POST", "/api/game/99/start"),
        ];
        let normalized = normalize_all(events);
        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0].template, "SELECT ?");
        assert_eq!(normalized[1].template, "POST /api/game/{id}/start");
    }

    #[test]
    fn normalize_truncates_oversized_trace_id() {
        let mut event = make_sql_event("SELECT 1");
        event.trace_id = "x".repeat(200);
        event.span_id = "y".repeat(200);
        event.parent_span_id = Some("z".repeat(200));
        let normalized = normalize(event);
        assert_eq!(normalized.event.trace_id.len(), MAX_ID_LENGTH);
        assert_eq!(normalized.event.span_id.len(), MAX_ID_LENGTH);
        assert_eq!(
            normalized.event.parent_span_id.unwrap().len(),
            MAX_ID_LENGTH
        );
    }

    #[test]
    fn normalize_preserves_normal_ids() {
        let event = make_sql_event("SELECT 1");
        let original_trace = event.trace_id.clone();
        let normalized = normalize(event);
        assert_eq!(normalized.event.trace_id, original_trace);
    }
}
