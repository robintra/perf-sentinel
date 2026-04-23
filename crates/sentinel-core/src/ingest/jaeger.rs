//! Jaeger JSON ingestion: parses Jaeger JSON export format into `SpanEvent`.
//!
//! Jaeger exports traces as:
//! ```json
//! { "data": [{ "traceID": "...", "spans": [...], "processes": {...} }] }
//! ```

use std::collections::HashMap;

use serde::Deserialize;

use crate::event::{EventSource, EventType, SpanEvent};
use crate::ingest::IngestSource;
use crate::time::micros_to_iso8601;

/// Ingests span events from Jaeger JSON export format.
pub struct JaegerIngest {
    max_size: usize,
}

impl JaegerIngest {
    #[must_use]
    pub const fn new(max_size: usize) -> Self {
        Self { max_size }
    }
}

impl IngestSource for JaegerIngest {
    type Error = JaegerIngestError;

    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
        if raw.len() > self.max_size {
            return Err(JaegerIngestError::PayloadTooLarge {
                size: raw.len(),
                max: self.max_size,
            });
        }
        let export: JaegerExport = serde_json::from_slice(raw).map_err(JaegerIngestError::Parse)?;
        Ok(convert_jaeger_export(&export))
    }
}

/// Errors that can occur during Jaeger JSON ingestion.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JaegerIngestError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

// ── Jaeger JSON structures ─────────────────────────────────────────
//
// These structs and the conversion helper below are shared with the
// HTTP-mode `jaeger_query` ingestion module, which receives the exact
// same `{"data": [...]}` payload from the Jaeger query API.

#[derive(Deserialize)]
pub(crate) struct JaegerExport {
    pub(crate) data: Vec<JaegerTrace>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JaegerTrace {
    #[serde(rename = "traceID")]
    trace_id: String,
    spans: Vec<JaegerSpan>,
    processes: HashMap<String, JaegerProcess>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JaegerSpan {
    #[serde(rename = "spanID")]
    span_id: String,
    operation_name: String,
    #[serde(default)]
    references: Vec<JaegerReference>,
    /// Start time in microseconds since epoch.
    start_time: u64,
    /// Duration in microseconds.
    duration: u64,
    #[serde(rename = "processID")]
    process_id: String,
    #[serde(default)]
    tags: Vec<JaegerTag>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JaegerReference {
    ref_type: String,
    #[serde(rename = "spanID")]
    span_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JaegerProcess {
    service_name: String,
}

#[derive(Deserialize)]
struct JaegerTag {
    key: String,
    value: serde_json::Value,
}

// ── Conversion ─────────────────────────────────────────────────────

pub(crate) fn convert_jaeger_export(export: &JaegerExport) -> Vec<SpanEvent> {
    let cap: usize = export.data.iter().map(|t| t.spans.len()).sum();
    let mut events = Vec::with_capacity(cap);
    for trace in &export.data {
        for span in &trace.spans {
            if let Some(event) = convert_jaeger_span(span, &trace.trace_id, &trace.processes) {
                events.push(event);
            }
        }
    }
    events
}

fn convert_jaeger_span(
    span: &JaegerSpan,
    trace_id: &str,
    processes: &HashMap<String, JaegerProcess>,
) -> Option<SpanEvent> {
    let tags = &span.tags;

    // Determine event type from tags
    let (event_type, target) = if let Some(stmt) =
        find_tag(tags, "db.statement").or_else(|| find_tag(tags, "db.query.text"))
    {
        (EventType::Sql, stmt)
    } else if let Some(url) = find_tag(tags, "http.url").or_else(|| find_tag(tags, "url.full")) {
        (EventType::HttpOut, url)
    } else {
        return None; // Not an I/O span
    };

    // Operation
    let operation = match event_type {
        EventType::Sql => find_tag(tags, "db.system").unwrap_or_else(|| "unknown".to_string()),
        EventType::HttpOut => find_tag(tags, "http.method")
            .or_else(|| find_tag(tags, "http.request.method"))
            .unwrap_or_else(|| "GET".to_string()),
    };

    // Service name from processes map
    let service = processes
        .get(&span.process_id)
        .map(|p| p.service_name.clone())
        .unwrap_or_default();

    // Parent span ID from CHILD_OF reference
    let parent_span_id = span
        .references
        .iter()
        .find(|r| r.ref_type == "CHILD_OF")
        .map(|r| r.span_id.clone());

    // Status code (HTTP only)
    let status_code = match event_type {
        EventType::HttpOut => find_tag(tags, "http.status_code")
            .or_else(|| find_tag(tags, "http.response.status_code"))
            .and_then(|s| s.parse().ok()),
        EventType::Sql => None,
    };

    // Source endpoint and method from tags (best effort)
    let endpoint = find_tag(tags, "http.route")
        .or_else(|| find_tag(tags, "http.target"))
        .unwrap_or_default();
    let method = find_tag(tags, "code.function").unwrap_or_else(|| span.operation_name.clone());

    // code.* attributes from span tags.
    let code_function = find_tag(tags, "code.function");
    let code_filepath = find_tag(tags, "code.filepath");
    let code_lineno = find_tag(tags, "code.lineno").and_then(|s| s.parse::<u32>().ok());
    let code_namespace = find_tag(tags, "code.namespace");

    let mut event = SpanEvent {
        timestamp: micros_to_iso8601(span.start_time),
        trace_id: trace_id.to_string(),
        span_id: span.span_id.clone(),
        parent_span_id,
        service,
        // Jaeger process tags do not carry cloud region. Users wanting
        // multi-region scoring with Jaeger ingestion should set
        // [green.service_regions] in the config to map service -> region.
        cloud_region: None,
        event_type,
        operation,
        target,
        duration_us: span.duration,
        source: EventSource { endpoint, method },
        status_code,
        response_size_bytes: None,
        code_function,
        code_filepath,
        code_lineno,
        code_namespace,
    };
    crate::event::sanitize_span_event(&mut event);
    Some(event)
}

fn find_tag(tags: &[JaegerTag], key: &str) -> Option<String> {
    tags.iter().find(|t| t.key == key).map(|t| match &t.value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_jaeger_json() -> &'static str {
        r#"{
            "data": [{
                "traceID": "abc123",
                "spans": [
                    {
                        "spanID": "span-1",
                        "operationName": "OrderService::create_order",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 1200,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT * FROM order_item WHERE order_id = 42" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    },
                    {
                        "spanID": "span-2",
                        "operationName": "http-call",
                        "references": [{ "refType": "CHILD_OF", "spanID": "span-1" }],
                        "startTime": 1720621921200000,
                        "duration": 15000,
                        "processID": "p1",
                        "tags": [
                            { "key": "http.url", "value": "http://user-svc:5000/api/users/123" },
                            { "key": "http.method", "value": "GET" },
                            { "key": "http.status_code", "value": "200" }
                        ]
                    },
                    {
                        "spanID": "span-3",
                        "operationName": "internal-op",
                        "references": [],
                        "startTime": 1720621921300000,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "internal.type", "value": "processing" }
                        ]
                    }
                ],
                "processes": {
                    "p1": { "serviceName": "order-svc" }
                }
            }]
        }"#
    }

    #[test]
    fn parses_jaeger_export() {
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(sample_jaeger_json().as_bytes()).unwrap();
        assert_eq!(events.len(), 2, "non-IO span should be skipped");
    }

    #[test]
    fn sql_span_maps_correctly() {
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(sample_jaeger_json().as_bytes()).unwrap();
        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .unwrap();

        assert_eq!(sql.trace_id, "abc123");
        assert_eq!(sql.span_id, "span-1");
        assert_eq!(sql.service, "order-svc");
        assert_eq!(sql.operation, "postgresql");
        assert_eq!(sql.target, "SELECT * FROM order_item WHERE order_id = 42");
        assert_eq!(sql.duration_us, 1200);
        assert!(sql.parent_span_id.is_none());
        assert!(sql.status_code.is_none());
        assert_eq!(sql.timestamp, "2024-07-10T14:32:01.123Z");
    }

    #[test]
    fn http_span_maps_correctly() {
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(sample_jaeger_json().as_bytes()).unwrap();
        let http = events
            .iter()
            .find(|e| e.event_type == EventType::HttpOut)
            .unwrap();

        assert_eq!(http.trace_id, "abc123");
        assert_eq!(http.span_id, "span-2");
        assert_eq!(http.operation, "GET");
        assert_eq!(http.target, "http://user-svc:5000/api/users/123");
        assert_eq!(http.duration_us, 15000);
        assert_eq!(http.status_code, Some(200));
        assert_eq!(http.parent_span_id.as_deref(), Some("span-1"));
    }

    #[test]
    fn rejects_oversized_payload() {
        let ingest = JaegerIngest::new(10);
        let result = ingest.ingest(sample_jaeger_json().as_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn malformed_json_missing_data_key() {
        let json = r#"{"traces": []}"#;
        let ingest = JaegerIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn malformed_json_missing_trace_id() {
        let json = r#"{"data": [{"spans": [], "processes": {}}]}"#;
        let ingest = JaegerIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn malformed_json_missing_spans() {
        let json = r#"{"data": [{"traceID": "t1", "processes": {}}]}"#;
        let ingest = JaegerIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn malformed_json_missing_span_id() {
        let json = r#"{"data": [{"traceID": "t1", "spans": [{"operationName": "op", "startTime": 0, "duration": 0, "processID": "p1", "tags": []}], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
        let ingest = JaegerIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn empty_data_array_produces_no_events() {
        let json = r#"{"data": []}"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn empty_spans_array_produces_no_events() {
        let json = r#"{"data": [{"traceID": "t1", "spans": [], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn unknown_process_id_produces_empty_service() {
        let json = r#"{"data": [{"traceID": "t1", "spans": [{"spanID": "s1", "operationName": "op", "startTime": 0, "duration": 100, "processID": "unknown", "tags": [{"key": "db.statement", "value": "SELECT 1"}]}], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].service, "");
    }

    #[test]
    fn numeric_tag_value_converted_to_string() {
        let json = r#"{"data": [{"traceID": "t1", "spans": [{"spanID": "s1", "operationName": "op", "startTime": 0, "duration": 100, "processID": "p1", "tags": [{"key": "http.url", "value": "http://svc/api"}, {"key": "http.status_code", "value": 200}]}], "processes": {"p1": {"serviceName": "svc"}}}]}"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status_code, Some(200));
    }

    #[test]
    fn stable_semconv_tags() {
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "query",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        { "key": "db.query.text", "value": "SELECT 1" },
                        { "key": "db.system", "value": "mysql" }
                    ]
                }, {
                    "spanID": "s2",
                    "operationName": "fetch",
                    "references": [],
                    "startTime": 1720621921200000,
                    "duration": 1000,
                    "processID": "p1",
                    "tags": [
                        { "key": "url.full", "value": "http://api/items" },
                        { "key": "http.request.method", "value": "POST" },
                        { "key": "http.response.status_code", "value": "201" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 2);

        let sql = &events[0];
        assert_eq!(sql.target, "SELECT 1");
        assert_eq!(sql.operation, "mysql");

        let http = &events[1];
        assert_eq!(http.target, "http://api/items");
        assert_eq!(http.operation, "POST");
        assert_eq!(http.status_code, Some(201));
    }
}
