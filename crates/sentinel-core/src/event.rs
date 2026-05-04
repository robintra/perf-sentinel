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
/// Delegates to [`truncate_field`] after a one-time clone to keep the
/// char-boundary walk in a single place.
#[must_use]
pub fn sanitize_id(id: &str) -> String {
    let mut s = id.to_string();
    truncate_field(&mut s, MAX_ID_LENGTH);
    s
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

/// Maximum length for `code_function` and `code_namespace` (bytes).
pub const MAX_CODE_FUNCTION_LENGTH: usize = 512;

/// Maximum length for `code_filepath` (bytes).
pub const MAX_CODE_FILEPATH_LENGTH: usize = 1024;

/// Maximum length for `code_namespace` (bytes).
pub const MAX_CODE_NAMESPACE_LENGTH: usize = 512;

/// Maximum length for a single instrumentation scope name (bytes).
/// Real OpenTelemetry scope names are short (`io.opentelemetry.spring-data-3.0`
/// is 33 bytes), so 256 leaves comfortable headroom while bounding the
/// memory amplification of the per-finding Vec clone.
pub const MAX_SCOPE_NAME_LENGTH: usize = 256;

/// Maximum number of instrumentation scopes captured per span. Matches
/// the OTLP parent-walk depth bound (`CODE_ATTRS_MAX_DEPTH = 8`). The
/// JSON ingest path has no such structural bound, so the cap fires there.
pub const MAX_INSTRUMENTATION_SCOPES: usize = 8;

/// Truncate a string to `max_len` bytes on a char boundary.
pub(crate) fn truncate_field(s: &mut String, max_len: usize) {
    if s.len() <= max_len {
        return;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Drop the field if it contains any ASCII control character, otherwise truncate.
///
/// Mirrors the silent-drop posture used for `cloud.region` invalid values.
/// Control characters in `code.*` would render badly in TUI/CLI output and
/// could enable log-injection if any future log site emitted them raw.
fn sanitize_optional_string(field: &mut Option<String>, max_len: usize) {
    if field
        .as_deref()
        .is_some_and(crate::config::has_control_char)
    {
        *field = None;
        return;
    }
    if let Some(s) = field.as_mut() {
        truncate_field(s, max_len);
    }
}

/// Drop entries with control characters, truncate the remainder to
/// `max_len` and cap the Vec at `max_count`.
///
/// Used for `instrumentation_scopes` (OpenTelemetry scope names from
/// arbitrary agents, including the JSON ingest path which has no
/// structural depth bound). Bounds both the per-element and per-event
/// memory amplification when those scope names propagate into the
/// per-finding clone.
fn sanitize_string_vec(field: &mut Vec<String>, max_len: usize, max_count: usize) {
    field.retain(|s| !crate::config::has_control_char(s));
    if field.len() > max_count {
        field.truncate(max_count);
    }
    for s in field.iter_mut() {
        truncate_field(s, max_len);
    }
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
    sanitize_optional_string(&mut event.code_function, MAX_CODE_FUNCTION_LENGTH);
    sanitize_optional_string(&mut event.code_filepath, MAX_CODE_FILEPATH_LENGTH);
    sanitize_optional_string(&mut event.code_namespace, MAX_CODE_NAMESPACE_LENGTH);
    sanitize_string_vec(
        &mut event.instrumentation_scopes,
        MAX_SCOPE_NAME_LENGTH,
        MAX_INSTRUMENTATION_SCOPES,
    );
}

/// Source code location extracted from `OTel` `code.*` span attributes.
///
/// Not all instrumentation agents emit these attributes. When present,
/// they allow findings to point to the exact function and file where the
/// anti-pattern originates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filepath: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl CodeLocation {
    /// Returns `true` when all fields are `None`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.function.is_none()
            && self.filepath.is_none()
            && self.lineno.is_none()
            && self.namespace.is_none()
    }

    /// Render the location as `namespace.function (filepath:lineno)`,
    /// omitting absent parts. Returns an empty string when the location
    /// has nothing displayable, so callers can skip the line entirely
    /// rather than printing a bare `Source:` label.
    ///
    /// Single source of truth for the CLI text output, the SARIF
    /// `physicalLocation` message, and the TUI detail panel.
    #[must_use]
    pub fn display_string(&self) -> String {
        let mut src = String::new();
        if let Some(ref ns) = self.namespace {
            src.push_str(ns);
            src.push('.');
        }
        if let Some(ref func) = self.function {
            src.push_str(func);
        }
        let has_name = !src.is_empty();
        if let Some(ref fp) = self.filepath {
            if has_name {
                src.push_str(" (");
            }
            src.push_str(fp);
            if let Some(ln) = self.lineno {
                src.push(':');
                src.push_str(&ln.to_string());
            }
            if has_name {
                src.push(')');
            }
        }
        src
    }
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
    /// `OTel` `code.function` attribute: the function name in the instrumented code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_function: Option<String>,
    /// `OTel` `code.filepath` attribute: the source file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_filepath: Option<String>,
    /// `OTel` `code.lineno` attribute: the line number in the source file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_lineno: Option<u32>,
    /// `OTel` `code.namespace` attribute: the namespace (e.g. Java package).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_namespace: Option<String>,
    /// OpenTelemetry instrumentation scope names captured at ingest time:
    /// the leaf span's scope at index 0, then each unique ancestor scope
    /// up to a bounded depth. Lets framework detection identify Spring
    /// Data, Hibernate, Quarkus, Helidon and friends from the
    /// `io.opentelemetry.<module>` strings emitted by the agent, without
    /// relying on user-code naming conventions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instrumentation_scopes: Vec<String>,
}

impl SpanEvent {
    /// Build a [`CodeLocation`] from this span's `code_*` fields.
    ///
    /// Returns `None` when all four fields are absent.
    #[must_use]
    pub fn code_location(&self) -> Option<CodeLocation> {
        if self.code_function.is_none()
            && self.code_filepath.is_none()
            && self.code_lineno.is_none()
            && self.code_namespace.is_none()
        {
            return None;
        }
        Some(CodeLocation {
            function: self.code_function.clone(),
            filepath: self.code_filepath.clone(),
            lineno: self.code_lineno,
            namespace: self.code_namespace.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_location_display_string_full() {
        let loc = CodeLocation {
            function: Some("OrderItemRepository.findByOrderId".to_string()),
            filepath: Some("order-service/src/main/java/OrderItemRepository.java".to_string()),
            lineno: Some(42),
            namespace: Some("com.example.order.repository".to_string()),
        };
        assert_eq!(
            loc.display_string(),
            "com.example.order.repository.OrderItemRepository.findByOrderId \
             (order-service/src/main/java/OrderItemRepository.java:42)"
        );
    }

    #[test]
    fn code_location_display_string_function_only() {
        let loc = CodeLocation {
            function: Some("fetchUser".to_string()),
            filepath: None,
            lineno: None,
            namespace: None,
        };
        assert_eq!(loc.display_string(), "fetchUser");
    }

    #[test]
    fn code_location_display_string_filepath_only() {
        let loc = CodeLocation {
            function: None,
            filepath: Some("src/main.rs".to_string()),
            lineno: Some(7),
            namespace: None,
        };
        // No function or namespace, so no parentheses wrap; filepath
        // still emits with its line number.
        assert_eq!(loc.display_string(), "src/main.rs:7");
    }

    #[test]
    fn code_location_display_string_empty_when_all_none() {
        let loc = CodeLocation {
            function: None,
            filepath: None,
            lineno: None,
            namespace: None,
        };
        assert_eq!(loc.display_string(), "");
        assert!(loc.is_empty());
    }

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

    // ------------------------------------------------------------------
    // CodeLocation and code_* fields
    // ------------------------------------------------------------------

    #[test]
    fn code_location_is_empty_when_all_none() {
        let loc = CodeLocation {
            function: None,
            filepath: None,
            lineno: None,
            namespace: None,
        };
        assert!(loc.is_empty());
    }

    #[test]
    fn code_location_not_empty_with_function() {
        let loc = CodeLocation {
            function: Some("processItems".to_string()),
            filepath: None,
            lineno: None,
            namespace: None,
        };
        assert!(!loc.is_empty());
    }

    #[test]
    fn span_event_code_location_none_when_all_absent() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        assert!(event.code_location().is_none());
    }

    #[test]
    fn span_event_code_location_some_when_present() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_function = Some("processItems".to_string());
        event.code_filepath = Some("src/OrderService.java".to_string());
        event.code_lineno = Some(42);
        event.code_namespace = Some("com.example".to_string());
        let loc = event.code_location().unwrap();
        assert_eq!(loc.function.as_deref(), Some("processItems"));
        assert_eq!(loc.filepath.as_deref(), Some("src/OrderService.java"));
        assert_eq!(loc.lineno, Some(42));
        assert_eq!(loc.namespace.as_deref(), Some("com.example"));
    }

    #[test]
    fn serde_roundtrip_with_code_fields() {
        let json = r#"{
            "timestamp": "2025-07-10T14:32:01.123Z",
            "trace_id": "abc123",
            "span_id": "span-1",
            "service": "svc",
            "type": "sql",
            "operation": "SELECT",
            "target": "SELECT 1",
            "duration_us": 100,
            "source": { "endpoint": "GET /test", "method": "test" },
            "code_function": "processItems",
            "code_filepath": "src/OrderService.java",
            "code_lineno": 42,
            "code_namespace": "com.example"
        }"#;
        let event: SpanEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.code_function.as_deref(), Some("processItems"));
        assert_eq!(event.code_lineno, Some(42));
        let serialized = serde_json::to_string(&event).unwrap();
        let back: SpanEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn code_fields_omitted_when_none() {
        let event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("code_function"));
        assert!(!json.contains("code_filepath"));
        assert!(!json.contains("code_lineno"));
        assert!(!json.contains("code_namespace"));
    }

    #[test]
    fn sanitize_truncates_long_code_function() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_function = Some("x".repeat(1000));
        sanitize_span_event(&mut event);
        assert!(event.code_function.as_ref().unwrap().len() <= MAX_CODE_FUNCTION_LENGTH);
    }

    #[test]
    fn sanitize_truncates_long_code_filepath() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_filepath = Some("x".repeat(2000));
        sanitize_span_event(&mut event);
        assert!(event.code_filepath.as_ref().unwrap().len() <= MAX_CODE_FILEPATH_LENGTH);
    }

    #[test]
    fn sanitize_drops_code_function_with_control_char() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_function = Some("findItems\x1b[31m".to_string());
        sanitize_span_event(&mut event);
        assert!(event.code_function.is_none());
    }

    #[test]
    fn sanitize_drops_code_filepath_with_newline() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_filepath = Some("src/main.rs\nINJECT".to_string());
        sanitize_span_event(&mut event);
        assert!(event.code_filepath.is_none());
    }

    #[test]
    fn sanitize_drops_code_namespace_with_del() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_namespace = Some("com.foo\x7fX".to_string());
        sanitize_span_event(&mut event);
        assert!(event.code_namespace.is_none());
    }

    #[test]
    fn sanitize_keeps_clean_code_fields() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.code_function = Some("findItems".to_string());
        event.code_filepath = Some("src/main/java/com/foo/Repo.java".to_string());
        event.code_namespace = Some("com.foo.Repo".to_string());
        sanitize_span_event(&mut event);
        assert_eq!(event.code_function.as_deref(), Some("findItems"));
        assert_eq!(
            event.code_filepath.as_deref(),
            Some("src/main/java/com/foo/Repo.java")
        );
        assert_eq!(event.code_namespace.as_deref(), Some("com.foo.Repo"));
    }

    // ── instrumentation_scopes sanitization ─────────────────────

    #[test]
    fn sanitize_truncates_long_instrumentation_scope() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.instrumentation_scopes = vec!["x".repeat(1024)];
        sanitize_span_event(&mut event);
        assert_eq!(event.instrumentation_scopes.len(), 1);
        assert!(event.instrumentation_scopes[0].len() <= MAX_SCOPE_NAME_LENGTH);
    }

    #[test]
    fn sanitize_drops_instrumentation_scope_with_control_char() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.instrumentation_scopes = vec![
            "io.opentelemetry.spring-data".to_string(),
            "\x1b[31mio.opentelemetry.evil\x1b[0m".to_string(),
            "io.opentelemetry.hibernate".to_string(),
        ];
        sanitize_span_event(&mut event);
        assert_eq!(
            event.instrumentation_scopes,
            vec![
                "io.opentelemetry.spring-data".to_string(),
                "io.opentelemetry.hibernate".to_string()
            ]
        );
    }

    #[test]
    fn sanitize_caps_oversize_instrumentation_scopes_vec() {
        let mut event: SpanEvent = serde_json::from_str(sample_sql_json()).unwrap();
        event.instrumentation_scopes = (0..32)
            .map(|i| format!("io.opentelemetry.scope-{i}"))
            .collect();
        sanitize_span_event(&mut event);
        assert_eq!(
            event.instrumentation_scopes.len(),
            MAX_INSTRUMENTATION_SCOPES
        );
        assert_eq!(event.instrumentation_scopes[0], "io.opentelemetry.scope-0");
    }
}
