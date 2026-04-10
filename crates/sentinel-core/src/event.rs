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

/// Maximum length for the `service` field (bytes).
pub const MAX_SERVICE_LENGTH: usize = 256;

/// Maximum length for the `operation` field (bytes).
pub const MAX_OPERATION_LENGTH: usize = 256;

/// Maximum length for the `target` field (bytes).
/// The SQL normalizer has its own 64 KB limit; this provides
/// defense-in-depth at the ingestion boundary.
pub const MAX_TARGET_LENGTH: usize = 65_536;

/// Maximum length for `source.endpoint` and `source.method` (bytes).
pub const MAX_SOURCE_LENGTH: usize = 512;

/// Truncate a string to `max_len` bytes on a char boundary.
fn truncate_field(s: &mut String, max_len: usize) {
    if s.len() <= max_len {
        return;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Sanitize all string fields on a [`SpanEvent`] to enforce length limits.
///
/// Maximum length for the `timestamp` field (bytes).
/// ISO 8601 with microseconds and timezone is at most ~30 chars.
const MAX_TIMESTAMP_LENGTH: usize = 64;

/// Called at every ingestion boundary (OTLP, JSON, Jaeger, Zipkin) to
/// prevent unbounded memory growth from oversized attribute values.
/// Also truncates IDs (`trace_id`, `span_id`, `parent_span_id`) that
/// are not already sanitized at the ingestion boundary for some formats
/// (Jaeger, Zipkin, native JSON).
pub fn sanitize_span_event(event: &mut SpanEvent) {
    truncate_field(&mut event.timestamp, MAX_TIMESTAMP_LENGTH);
    truncate_field(&mut event.trace_id, MAX_ID_LENGTH);
    truncate_field(&mut event.span_id, MAX_ID_LENGTH);
    if let Some(ref mut pid) = event.parent_span_id {
        truncate_field(pid, MAX_ID_LENGTH);
    }
    if let Some(ref mut region) = event.cloud_region {
        truncate_field(region, MAX_ID_LENGTH); // is_valid_region_id caps at 64
    }
    truncate_field(&mut event.service, MAX_SERVICE_LENGTH);
    truncate_field(&mut event.operation, MAX_OPERATION_LENGTH);
    truncate_field(&mut event.target, MAX_TARGET_LENGTH);
    truncate_field(&mut event.source.endpoint, MAX_SOURCE_LENGTH);
    truncate_field(&mut event.source.method, MAX_SOURCE_LENGTH);
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
    /// Cloud region this span was emitted from, sourced from the `OTel`
    /// `cloud.region` resource attribute (or span attribute as fallback).
    ///
    /// Used by the carbon scoring stage to apply per-region carbon
    /// intensity coefficients in multi-region deployments. `None` when
    /// the attribute is absent or when ingesting from formats that don't
    /// carry it (`Jaeger`, `Zipkin`, raw `JSON` without explicit field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_region: Option<String>,
    #[serde(rename = "type")]
    pub event_type: EventType,
    /// SQL: `db.system` for OTLP (e.g. "postgresql"), verb for native JSON.
    /// HTTP: request method (e.g. "GET").
    pub operation: String,
    pub target: String,
    pub duration_us: u64,
    pub source: EventSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// HTTP response body size in bytes, sourced from the `OTel`
    /// `http.response.body.size` attribute (or legacy
    /// `http.response_content_length`). Used by the carbon scoring stage
    /// for HTTP payload size tier classification and network transport
    /// energy estimation. `None` for SQL spans or when the attribute is
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_size_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sql_json() -> &'static str {
        r#"{
            "timestamp": "2025-07-10T14:32:01.123Z",
            "trace_id": "abc123-def456",
            "span_id": "span-789",
            "service": "order-svc",
            "type": "sql",
            "operation": "SELECT",
            "target": "SELECT * FROM order_item WHERE order_id = 42",
            "duration_us": 1200,
            "source": {
                "endpoint": "POST /api/orders/42/submit",
                "method": "OrderService::create_order"
            }
        }"#
    }

    fn sample_http_json() -> &'static str {
        r#"{
            "timestamp": "2025-07-10T14:32:01.456Z",
            "trace_id": "abc123-def456",
            "span_id": "span-790",
            "service": "order-svc",
            "type": "http_out",
            "operation": "GET",
            "target": "http://user-svc:5000/api/users/user-123",
            "duration_us": 15000,
            "status_code": 200,
            "source": {
                "endpoint": "POST /api/orders/42/submit",
                "method": "OrderService::create_order"
            }
        }"#
    }

    #[test]
    fn deserialize_sql_event() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        assert_eq!(event.event_type, EventType::Sql);
        assert_eq!(event.trace_id, "abc123-def456");
        assert_eq!(event.service, "order-svc");
        assert_eq!(event.target, "SELECT * FROM order_item WHERE order_id = 42");
        assert_eq!(event.duration_us, 1200);
        assert!(event.status_code.is_none());
    }

    #[test]
    fn deserialize_http_event() {
        let event: SpanEvent = serde_json::from_str(sample_http_json()).unwrap();
        assert_eq!(event.event_type, EventType::HttpOut);
        assert_eq!(event.status_code, Some(200));
        assert_eq!(event.source.endpoint, "POST /api/orders/42/submit");
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
    fn deserialize_event_without_cloud_region_defaults_to_none() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        assert!(event.cloud_region.is_none());
    }

    #[test]
    fn serde_roundtrip_with_cloud_region() {
        let json = r#"{
            "timestamp": "2025-07-10T14:32:01.123Z",
            "trace_id": "abc123-def456",
            "span_id": "span-789",
            "service": "order-svc",
            "cloud_region": "eu-west-3",
            "type": "sql",
            "operation": "SELECT",
            "target": "SELECT 1",
            "duration_us": 1200,
            "source": {
                "endpoint": "POST /api/orders/42/submit",
                "method": "OrderService::create_order"
            }
        }"#;
        let event: SpanEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.cloud_region.as_deref(), Some("eu-west-3"));
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(serialized.contains("\"cloud_region\":\"eu-west-3\""));
        let back: SpanEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn cloud_region_omitted_when_none() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("cloud_region"));
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

    // ------------------------------------------------------------------
    // sanitize_span_event
    // ------------------------------------------------------------------

    fn make_event_with_field(field: &str, value: &str) -> SpanEvent {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        match field {
            "service" => event.service = value.to_string(),
            "operation" => event.operation = value.to_string(),
            "target" => event.target = value.to_string(),
            "endpoint" => event.source.endpoint = value.to_string(),
            "method" => event.source.method = value.to_string(),
            _ => panic!("unknown field: {field}"),
        }
        event
    }

    #[test]
    fn sanitize_truncates_long_service() {
        let mut event = make_event_with_field("service", &"x".repeat(500));
        sanitize_span_event(&mut event);
        assert!(event.service.len() <= MAX_SERVICE_LENGTH);
    }

    #[test]
    fn sanitize_truncates_long_operation() {
        let mut event = make_event_with_field("operation", &"x".repeat(500));
        sanitize_span_event(&mut event);
        assert!(event.operation.len() <= MAX_OPERATION_LENGTH);
    }

    #[test]
    fn sanitize_truncates_long_target() {
        let mut event = make_event_with_field("target", &"x".repeat(100_000));
        sanitize_span_event(&mut event);
        assert!(event.target.len() <= MAX_TARGET_LENGTH);
    }

    #[test]
    fn sanitize_truncates_long_endpoint() {
        let mut event = make_event_with_field("endpoint", &"x".repeat(1000));
        sanitize_span_event(&mut event);
        assert!(event.source.endpoint.len() <= MAX_SOURCE_LENGTH);
    }

    #[test]
    fn sanitize_truncates_long_method() {
        let mut event = make_event_with_field("method", &"x".repeat(1000));
        sanitize_span_event(&mut event);
        assert!(event.source.method.len() <= MAX_SOURCE_LENGTH);
    }

    #[test]
    fn sanitize_short_fields_unchanged() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        let before = event.clone();
        sanitize_span_event(&mut event);
        assert_eq!(event, before);
    }

    #[test]
    fn sanitize_multibyte_char_boundary() {
        // Service with 4-byte emojis that would split mid-char at MAX_SERVICE_LENGTH
        let mut event = make_event_with_field("service", &"\u{1F600}".repeat(100));
        sanitize_span_event(&mut event);
        assert!(event.service.len() <= MAX_SERVICE_LENGTH);
        // Must be valid UTF-8 (String invariant guarantees this, but verify)
        assert!(event.service.is_char_boundary(event.service.len()));
    }
}
