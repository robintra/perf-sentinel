//! JSON ingestion with auto-format detection.
//!
//! Detects the input format (native, Jaeger, Zipkin) and dispatches to the
//! appropriate parser. Format detection peeks at the JSON structure:
//! - Has `"data"` key with trace objects containing `"spans"` -> Jaeger
//! - Is array where items have `"traceId"` + `"localEndpoint"` -> Zipkin
//! - Otherwise -> native perf-sentinel format

use crate::event::SpanEvent;
use crate::ingest::IngestSource;

/// The detected input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    /// Native perf-sentinel JSON array of `SpanEvent`.
    Native,
    /// Jaeger JSON export format.
    Jaeger,
    /// Zipkin JSON v2 format.
    Zipkin,
}

/// Ingests span events from JSON input with auto-format detection.
pub struct JsonIngest {
    max_size: usize,
}

impl JsonIngest {
    #[must_use]
    pub const fn new(max_size: usize) -> Self {
        Self { max_size }
    }
}

impl IngestSource for JsonIngest {
    type Error = JsonIngestError;

    fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
        if raw.len() > self.max_size {
            return Err(JsonIngestError::PayloadTooLarge {
                size: raw.len(),
                max: self.max_size,
            });
        }

        match detect_format(raw) {
            InputFormat::Jaeger => {
                let ingest = crate::ingest::jaeger::JaegerIngest::new(self.max_size);
                ingest
                    .ingest(raw)
                    .map_err(|e| JsonIngestError::Format(e.to_string()))
            }
            InputFormat::Zipkin => {
                let ingest = crate::ingest::zipkin::ZipkinIngest::new(self.max_size);
                ingest
                    .ingest(raw)
                    .map_err(|e| JsonIngestError::Format(e.to_string()))
            }
            InputFormat::Native => {
                let mut events: Vec<SpanEvent> =
                    serde_json::from_slice(raw).map_err(JsonIngestError::Parse)?;
                // C1: sanitize cloud.region at the JSON ingest boundary, symmetric
                // with the OTLP path. Invalid values (empty, > 64 bytes, non-ASCII
                // alphanumeric plus `-`/`_`) are replaced with None to prevent
                // log-forging through downstream tracing::debug! format strings.
                for event in &mut events {
                    if let Some(region) = event.cloud_region.as_deref()
                        && !crate::score::carbon::is_valid_region_id(region)
                    {
                        event.cloud_region = None;
                    }
                    crate::event::sanitize_span_event(event);
                }
                Ok(events)
            }
        }
    }
}

/// Detect the format of the JSON input using lightweight byte-level heuristics.
///
/// Peeks at the first few kilobytes to identify the format without parsing the full
/// payload into a `serde_json::Value`, avoiding a 2x parse cost.
#[must_use]
pub fn detect_format(raw: &[u8]) -> InputFormat {
    let peek = std::str::from_utf8(&raw[..raw.len().min(1024)]).unwrap_or("");

    // Jaeger: { "data": [{ ..., "spans": [...] }] }
    if peek.trim_start().starts_with('{') && peek.contains("\"data\"") {
        let deeper = std::str::from_utf8(&raw[..raw.len().min(4096)]).unwrap_or("");
        if deeper.contains("\"spans\"") {
            return InputFormat::Jaeger;
        }
    }

    // Zipkin: [{ "traceId": "...", "localEndpoint": {...} }]
    if peek.trim_start().starts_with('[')
        && peek.contains("\"traceId\"")
        && peek.contains("\"localEndpoint\"")
    {
        return InputFormat::Zipkin;
    }

    InputFormat::Native
}

/// Errors that can occur during JSON ingestion.
#[derive(Debug, thiserror::Error)]
pub enum JsonIngestError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("format detection error: {0}")]
    Format(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_payload() {
        let ingest = JsonIngest::new(10);
        let result = ingest.ingest(&[0u8; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_empty_array() {
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(b"[]").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn detect_native_format() {
        let json = r#"[{"type": "sql", "target": "SELECT 1"}]"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Native);
    }

    #[test]
    fn detect_jaeger_format() {
        let json = r#"{"data": [{"traceID": "abc", "spans": [], "processes": {}}]}"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Jaeger);
    }

    #[test]
    fn detect_zipkin_format() {
        let json = r#"[{"traceId": "abc", "id": "s1", "localEndpoint": {"serviceName": "svc"}}]"#;
        assert_eq!(detect_format(json.as_bytes()), InputFormat::Zipkin);
    }

    #[test]
    fn detect_empty_array_is_native() {
        assert_eq!(detect_format(b"[]"), InputFormat::Native);
    }

    #[test]
    fn detect_invalid_json_falls_to_native() {
        assert_eq!(detect_format(b"not json"), InputFormat::Native);
    }

    #[test]
    fn auto_ingest_jaeger() {
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1",
                    "operationName": "op",
                    "references": [],
                    "startTime": 1720621921123000,
                    "duration": 500,
                    "processID": "p1",
                    "tags": [
                        {"key": "db.statement", "value": "SELECT 1"},
                        {"key": "db.system", "value": "pg"}
                    ]
                }],
                "processes": {"p1": {"serviceName": "svc"}}
            }]
        }"#;
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    #[test]
    fn auto_ingest_zipkin() {
        let json = r#"[{
            "traceId": "t1",
            "id": "s1",
            "name": "query",
            "timestamp": 1720621921123000,
            "duration": 500,
            "localEndpoint": {"serviceName": "svc"},
            "tags": {"db.statement": "SELECT 1", "db.system": "pg"}
        }]"#;
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "SELECT 1");
    }

    // ----- Review fix C1: sanitize cloud_region on native JSON path -----

    fn native_event_with_cloud_region(cloud_region: &str) -> String {
        format!(
            r#"[{{
                "timestamp": "2025-07-10T14:32:01.123Z",
                "trace_id": "trace-1",
                "span_id": "span-1",
                "service": "order-svc",
                "cloud_region": {cr},
                "type": "sql",
                "operation": "SELECT",
                "target": "SELECT 1",
                "duration_us": 1000,
                "source": {{
                    "endpoint": "POST /api/orders/42/submit",
                    "method": "OrderService::create_order"
                }}
            }}]"#,
            cr = serde_json::to_string(cloud_region).unwrap()
        )
    }

    #[test]
    fn native_json_valid_cloud_region_preserved() {
        // Valid region names round-trip intact.
        let json = native_event_with_cloud_region("eu-west-3");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cloud_region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn native_json_invalid_cloud_region_is_sanitized_to_none() {
        // C1: a malicious client on the JSON socket trying to log-forge via
        // a newline in cloud_region must have the value replaced with None,
        // symmetric with the OTLP boundary sanitization.
        let json = native_event_with_cloud_region("eu-west-3\n2026 WARN fake alert");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].cloud_region.is_none(),
            "cloud_region with control char must be sanitized"
        );
    }

    #[test]
    fn native_json_oversized_cloud_region_sanitized() {
        // 65 chars exceeds the 64-byte cap.
        let long_region = "a".repeat(65);
        let json = native_event_with_cloud_region(&long_region);
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn native_json_cloud_region_with_space_sanitized() {
        let json = native_event_with_cloud_region("eu west 3");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn native_json_cloud_region_with_dot_sanitized() {
        // Dot is not in the allowlist (prevents path-traversal-style tricks).
        let json = native_event_with_cloud_region("eu.west.3");
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events[0].cloud_region.is_none());
    }
}
