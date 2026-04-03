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

/// Maximum allowed length for a `trace_id` or `span_id`.
///
/// OpenTelemetry specifies 32 hex chars for trace IDs and 16 for span IDs.
/// We allow up to 128 chars to accommodate non-standard formats.
pub const MAX_ID_LENGTH: usize = 128;

/// Truncate an ID field (`trace_id`, `span_id`) to [`MAX_ID_LENGTH`].
///
/// Uses char-boundary-aware truncation to avoid panicking on multi-byte UTF-8.
#[must_use]
pub fn sanitize_id(id: &str) -> String {
    if id.len() <= MAX_ID_LENGTH {
        return id.to_string();
    }
    let mut end = MAX_ID_LENGTH;
    while end > 0 && !id.is_char_boundary(end) {
        end -= 1;
    }
    id[..end].to_string()
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

    #[test]
    fn sanitize_id_short_unchanged() {
        assert_eq!(sanitize_id("abc-123"), "abc-123");
    }

    #[test]
    fn sanitize_id_truncates_long() {
        let long = "a".repeat(200);
        let result = sanitize_id(&long);
        assert_eq!(result.len(), MAX_ID_LENGTH);
    }

    #[test]
    fn sanitize_id_exact_length_unchanged() {
        let exact = "b".repeat(MAX_ID_LENGTH);
        assert_eq!(sanitize_id(&exact), exact);
    }

    #[test]
    fn sanitize_id_multibyte_no_panic() {
        // 4-byte emoji repeated to exceed MAX_ID_LENGTH (200 bytes total)
        let id = "\u{1F600}".repeat(50);
        assert!(id.len() > MAX_ID_LENGTH);
        let result = sanitize_id(&id);
        assert!(result.len() <= MAX_ID_LENGTH);
        // Must be valid UTF-8 (would panic in .to_string() if not)
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn sanitize_id_two_byte_chars_no_panic() {
        // 2-byte UTF-8 chars: é is 2 bytes
        let id = "é".repeat(100); // 200 bytes
        let result = sanitize_id(&id);
        assert!(result.len() <= MAX_ID_LENGTH);
        // Result should contain whole chars only (even byte count for 2-byte chars)
        assert_eq!(result.len() % 2, 0);
    }
}
