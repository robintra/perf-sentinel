//! Normalization stage: canonicalizes SQL queries and HTTP URLs.

pub mod http;
pub mod sql;

use crate::event::{EventType, SpanEvent};

/// A span event enriched with its normalized template and extracted parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedEvent {
    pub event: SpanEvent,
    pub template: String,
    pub params: Vec<String>,
}

/// Normalize a single event by dispatching on its type.
#[must_use]
pub fn normalize(event: SpanEvent) -> NormalizedEvent {
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
}
