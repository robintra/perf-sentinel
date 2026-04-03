//! Core event types for the perf-sentinel pipeline.

use serde::{Deserialize, Serialize};

/// The type of I/O operation a span represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Sql,
    HttpOut,
}

/// Source context for the span (which endpoint/method triggered it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSource {
    pub endpoint: String,
    pub method: String,
}

/// A single span event representing an I/O operation (SQL query, HTTP call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanEvent {
    pub timestamp: String,
    pub trace_id: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub service: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub operation: String,
    pub target: String,
    pub duration_us: u64,
    pub source: EventSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sql_json() -> &'static str {
        r#"{
            "timestamp": "2025-07-10T14:32:01.123Z",
            "trace_id": "abc123-def456",
            "span_id": "span-789",
            "service": "game",
            "type": "sql",
            "operation": "SELECT",
            "target": "SELECT * FROM player WHERE game_id = 42",
            "duration_us": 1200,
            "source": {
                "endpoint": "POST /api/game/42/start",
                "method": "GameService::start_game"
            }
        }"#
    }

    fn sample_http_json() -> &'static str {
        r#"{
            "timestamp": "2025-07-10T14:32:01.456Z",
            "trace_id": "abc123-def456",
            "span_id": "span-790",
            "service": "game",
            "type": "http_out",
            "operation": "GET",
            "target": "http://account-chat:5000/api/account/player-123",
            "duration_us": 15000,
            "status_code": 200,
            "source": {
                "endpoint": "POST /api/game/42/start",
                "method": "GameService::start_game"
            }
        }"#
    }

    #[test]
    fn deserialize_sql_event() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        assert_eq!(event.event_type, EventType::Sql);
        assert_eq!(event.trace_id, "abc123-def456");
        assert_eq!(event.service, "game");
        assert_eq!(event.target, "SELECT * FROM player WHERE game_id = 42");
        assert_eq!(event.duration_us, 1200);
        assert!(event.status_code.is_none());
    }

    #[test]
    fn deserialize_http_event() {
        let event: SpanEvent = serde_json::from_str(sample_http_json()).unwrap();
        assert_eq!(event.event_type, EventType::HttpOut);
        assert_eq!(event.status_code, Some(200));
        assert_eq!(event.source.endpoint, "POST /api/game/42/start");
    }

    #[test]
    fn serde_roundtrip_sql() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        let json = serde_json::to_string(&event).unwrap();
        let back: SpanEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn serde_roundtrip_http() {
        let event: SpanEvent = serde_json::from_str(sample_http_json()).unwrap();
        let json = serde_json::to_string(&event).unwrap();
        let back: SpanEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn sql_event_omits_status_code_in_json() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("status_code"));
    }
}
