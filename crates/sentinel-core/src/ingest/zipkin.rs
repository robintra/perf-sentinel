//! Zipkin JSON v2 ingestion: parses Zipkin JSON spans into `SpanEvent`.
//!
//! Zipkin v2 format is a flat array of span objects:
//! ```json
//! [{ "traceId": "...", "id": "...", "parentId": "...", ... }]
//! ```

use crate::event::{EventSource, EventType, SpanEvent};
use crate::ingest::IngestSource;
use crate::time::micros_to_iso8601;

use serde::Deserialize;
use std::collections::HashMap;

/// Ingests span events from Zipkin JSON v2 format.
pub struct ZipkinIngest {
    max_size: usize,
}

impl ZipkinIngest {
    #[must_use]
    pub const fn new(max_size: usize) -> Self {
        Self { max_size }
    }
}

impl IngestSource for ZipkinIngest {
    type Error = ZipkinIngestError;

    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
        if raw.len() > self.max_size {
            return Err(ZipkinIngestError::PayloadTooLarge {
                size: raw.len(),
                max: self.max_size,
            });
        }
        let spans: Vec<ZipkinSpan> =
            serde_json::from_slice(raw).map_err(ZipkinIngestError::Parse)?;
        Ok(convert_zipkin_spans(&spans))
    }
}

/// Errors that can occur during Zipkin JSON ingestion.
#[derive(Debug, thiserror::Error)]
pub enum ZipkinIngestError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

// ── Zipkin JSON v2 structures ──────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZipkinSpan {
    trace_id: String,
    id: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Timestamp in microseconds since epoch.
    #[serde(default)]
    timestamp: Option<u64>,
    /// Duration in microseconds.
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    local_endpoint: Option<ZipkinEndpoint>,
    #[serde(default)]
    tags: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZipkinEndpoint {
    #[serde(default)]
    service_name: Option<String>,
}

// ── Conversion ─────────────────────────────────────────────────────

fn convert_zipkin_spans(spans: &[ZipkinSpan]) -> Vec<SpanEvent> {
    spans.iter().filter_map(convert_zipkin_span).collect()
}

fn convert_zipkin_span(span: &ZipkinSpan) -> Option<SpanEvent> {
    let tags = span.tags.as_ref();

    let get_tag = |key: &str| -> Option<&str> { tags.and_then(|t| t.get(key).map(String::as_str)) };

    // Determine event type from tags
    let (event_type, target) =
        if let Some(stmt) = get_tag("db.statement").or_else(|| get_tag("db.query.text")) {
            (EventType::Sql, stmt.to_string())
        } else if let Some(url) = get_tag("http.url").or_else(|| get_tag("url.full")) {
            (EventType::HttpOut, url.to_string())
        } else {
            return None;
        };

    let operation = match event_type {
        EventType::Sql => get_tag("db.system").unwrap_or("unknown").to_string(),
        EventType::HttpOut => get_tag("http.method")
            .or_else(|| get_tag("http.request.method"))
            .unwrap_or("GET")
            .to_string(),
    };

    let service = span
        .local_endpoint
        .as_ref()
        .and_then(|ep| ep.service_name.as_deref())
        .unwrap_or_default()
        .to_string();

    let timestamp = span.timestamp.unwrap_or(0);
    let duration_us = span.duration.unwrap_or(0);

    let status_code = match event_type {
        EventType::HttpOut => get_tag("http.status_code")
            .or_else(|| get_tag("http.response.status_code"))
            .and_then(|s| s.parse().ok()),
        EventType::Sql => None,
    };

    let endpoint = get_tag("http.route")
        .or_else(|| get_tag("http.target"))
        .unwrap_or_default()
        .to_string();
    let method = get_tag("code.function")
        .map(String::from)
        .or_else(|| span.name.clone())
        .unwrap_or_default();

    let mut event = SpanEvent {
        timestamp: micros_to_iso8601(timestamp),
        trace_id: span.trace_id.clone(),
        span_id: span.id.clone(),
        parent_span_id: span.parent_id.clone(),
        service,
        // Zipkin endpoint metadata does not carry cloud region. Users
        // wanting multi-region scoring with Zipkin ingestion should set
        // [green.service_regions] in the config to map service -> region.
        cloud_region: None,
        event_type,
        operation,
        target,
        duration_us,
        source: EventSource { endpoint, method },
        status_code,
    };
    crate::event::sanitize_span_event(&mut event);
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_zipkin_json() -> &'static str {
        r#"[
            {
                "traceId": "abc123",
                "id": "span-1",
                "name": "OrderService::create_order",
                "timestamp": 1720621921123000,
                "duration": 1200,
                "localEndpoint": { "serviceName": "order-svc" },
                "tags": {
                    "db.statement": "SELECT * FROM order_item WHERE order_id = 42",
                    "db.system": "postgresql"
                }
            },
            {
                "traceId": "abc123",
                "id": "span-2",
                "parentId": "span-1",
                "name": "http-call",
                "timestamp": 1720621921200000,
                "duration": 15000,
                "localEndpoint": { "serviceName": "order-svc" },
                "tags": {
                    "http.url": "http://user-svc:5000/api/users/123",
                    "http.method": "GET",
                    "http.status_code": "200"
                }
            },
            {
                "traceId": "abc123",
                "id": "span-3",
                "name": "internal-processing",
                "timestamp": 1720621921300000,
                "duration": 500,
                "localEndpoint": { "serviceName": "order-svc" },
                "tags": {
                    "internal.type": "processing"
                }
            }
        ]"#
    }

    #[test]
    fn parses_zipkin_export() {
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(sample_zipkin_json().as_bytes()).unwrap();
        assert_eq!(events.len(), 2, "non-IO span should be skipped");
    }

    #[test]
    fn sql_span_maps_correctly() {
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(sample_zipkin_json().as_bytes()).unwrap();
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
        assert_eq!(sql.timestamp, "2024-07-10T14:32:01.123Z");
    }

    #[test]
    fn http_span_maps_correctly() {
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(sample_zipkin_json().as_bytes()).unwrap();
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
        let ingest = ZipkinIngest::new(10);
        let result = ingest.ingest(sample_zipkin_json().as_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn malformed_json_not_array() {
        let json = r#"{"traceId": "t1"}"#;
        let ingest = ZipkinIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn malformed_json_missing_trace_id() {
        let json = r#"[{"id": "s1"}]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn malformed_json_missing_span_id() {
        let json = r#"[{"traceId": "t1"}]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        assert!(ingest.ingest(json.as_bytes()).is_err());
    }

    #[test]
    fn empty_array_produces_no_events() {
        let json = "[]";
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn missing_optional_fields_handled() {
        let json = r#"[{"traceId": "t1", "id": "s1", "tags": {"db.statement": "SELECT 1"}}]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].duration_us, 0);
        assert_eq!(events[0].service, "");
        assert!(events[0].parent_span_id.is_none());
    }

    #[test]
    fn no_tags_skips_span() {
        let json = r#"[{"traceId": "t1", "id": "s1"}]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn empty_tags_skips_span() {
        let json = r#"[{"traceId": "t1", "id": "s1", "tags": {}}]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn zero_timestamp_and_duration() {
        let json = r#"[{"traceId": "t1", "id": "s1", "timestamp": 0, "duration": 0, "tags": {"db.statement": "SELECT 1"}}]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events[0].timestamp, "1970-01-01T00:00:00.000Z");
        assert_eq!(events[0].duration_us, 0);
    }

    #[test]
    fn stable_semconv_tags() {
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "query",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.query.text": "SELECT 1",
                    "db.system": "mysql"
                }
            },
            {
                "traceId": "t1",
                "id": "s2",
                "name": "fetch",
                "timestamp": 1720621921200000,
                "duration": 1000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "url.full": "http://api/items",
                    "http.request.method": "POST",
                    "http.response.status_code": "201"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
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
