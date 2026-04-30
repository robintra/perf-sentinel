//! JSON ingestion with auto-format detection.
//!
//! Detects the input format (native, Jaeger, Zipkin) and dispatches to the
//! appropriate parser. Format detection peeks at the JSON structure:
//! - Has `"data"` key with trace objects containing `"spans"` -> Jaeger
//! - Is array where items have `"traceId"` + `"localEndpoint"` -> Zipkin
//! - Otherwise -> native perf-sentinel format

use crate::event::SpanEvent;
use crate::ingest::IngestSource;

/// Defense-in-depth nesting cap for the native ingest path. The native
/// span-event format is flat (top-level array of objects, each with at
/// most a `source` and a few scalar fields), so depth 32 is well above
/// what valid input ever needs. We pre-scan the bytes BEFORE handing
/// them to `serde_json::from_slice` because `serde_json` has a built-in
/// recursion limit of 128 (its compile-time default, no public setter
/// to tighten it). The pre-scan is O(N) in payload bytes, negligible
/// next to the JSON parse cost.
pub const MAX_JSON_DEPTH: usize = 32;

/// Reject the payload when its bracket nesting exceeds [`MAX_JSON_DEPTH`].
///
/// This is a byte-level pre-scan, not a full JSON parse: it counts `[`
/// and `{` opens against `]` and `}` closes, ignoring any character that
/// appears inside a `"..."` string (with `\"` escape support). False
/// positives (rejecting valid input) are impossible because we never
/// inflate the depth on string contents. False negatives (accepting an
/// over-deep payload) are impossible because every structural open
/// increments depth.
///
/// `pub` so CLI subcommands that accept user-supplied JSON through paths
/// that bypass `JsonIngest` (e.g. `report --input` in Report mode,
/// `report --before`) can enforce the same defense-in-depth cap.
#[must_use]
pub fn exceeds_max_depth(raw: &[u8]) -> bool {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    for &b in raw {
        if in_string {
            advance_string_state(b, &mut in_string, &mut escape);
            continue;
        }
        if bump_depth(b, &mut depth, &mut in_string) {
            return true;
        }
    }
    false
}

/// Advance the string-scanning state machine by one byte while inside a
/// `"..."` literal. Handles `\"` escapes and the closing `"`. Pulled out
/// of [`exceeds_max_depth`] to keep its cognitive complexity under the
/// S3776 threshold.
#[inline]
fn advance_string_state(b: u8, in_string: &mut bool, escape: &mut bool) {
    if *escape {
        *escape = false;
    } else if b == b'\\' {
        *escape = true;
    } else if b == b'"' {
        *in_string = false;
    }
}

/// Apply a structural byte to the bracket-depth counter. Returns `true`
/// iff the depth rose above [`MAX_JSON_DEPTH`] (the caller short-circuits
/// and rejects the payload).
#[inline]
fn bump_depth(b: u8, depth: &mut usize, in_string: &mut bool) -> bool {
    match b {
        b'"' => *in_string = true,
        b'[' | b'{' => {
            *depth += 1;
            if *depth > MAX_JSON_DEPTH {
                return true;
            }
        }
        b']' | b'}' => *depth = depth.saturating_sub(1),
        _ => {}
    }
    false
}

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

        // Apply the project-wide nesting cap before dispatching to a
        // format-specific parser. Pre-0.5.15 only the Native arm enforced
        // it, leaving Jaeger and Zipkin paths on serde_json's looser
        // 128-frame default.
        if exceeds_max_depth(raw) {
            return Err(JsonIngestError::PayloadTooDeep {
                max_depth: MAX_JSON_DEPTH,
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
                // Sanitize cloud.region at the JSON ingest boundary, symmetric
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
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JsonIngestError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error(
        "payload nesting exceeds maximum depth of {max_depth} (defense against deeply-nested attacker payloads)"
    )]
    PayloadTooDeep { max_depth: usize },
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

    // ----- Sanitize cloud_region on native JSON path -----

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
        // A malicious client on the JSON socket trying to log-forge via
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

    #[test]
    fn deeply_nested_native_payload_is_rejected_below_stack_overflow() {
        // Build `[[[[...]]]]` with depth above `MAX_JSON_DEPTH`. The
        // pre-scan guard must reject before serde_json walks the tree.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::with_capacity(depth * 2);
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(matches!(
            result,
            Err(JsonIngestError::PayloadTooDeep { .. })
        ));
    }

    #[test]
    fn deeply_nested_jaeger_payload_is_rejected() {
        // Pre-0.5.15 only the Native arm enforced MAX_JSON_DEPTH. A Jaeger
        // payload with 33+ frames of nesting would slip through to
        // JaegerIngest and rely on serde_json's looser 128-frame default.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::from(r#"{"data":[{"spans":[{"tags":["#);
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        payload.push_str("]}]}]}");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "deeply-nested Jaeger input must be rejected: {result:?}"
        );
    }

    #[test]
    fn deeply_nested_zipkin_payload_is_rejected() {
        // Symmetric guard for the Zipkin v2 path.
        let depth = MAX_JSON_DEPTH + 4;
        let mut payload = String::from(
            r#"[{"traceId":"abc","localEndpoint":{"serviceName":"s"},"annotations":["#,
        );
        for _ in 0..depth {
            payload.push('[');
        }
        for _ in 0..depth {
            payload.push(']');
        }
        payload.push_str("]}]");
        let ingest = JsonIngest::new(1_048_576);
        let result = ingest.ingest(payload.as_bytes());
        assert!(
            matches!(result, Err(JsonIngestError::PayloadTooDeep { .. })),
            "deeply-nested Zipkin input must be rejected: {result:?}"
        );
    }

    #[test]
    fn depth_scan_ignores_brackets_inside_strings() {
        // A valid native event whose `target` field contains `[[[...`.
        // The pre-scan must not count those brackets, otherwise it
        // would falsely reject SQL queries like `WHERE id IN (...)` or
        // template strings.
        let json = native_event_with_cloud_region("eu-west-3").replace(
            "\"SELECT 1\"",
            "\"SELECT * FROM t WHERE col = '[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]'\"",
        );
        let ingest = JsonIngest::new(1_048_576);
        let events = ingest
            .ingest(json.as_bytes())
            .expect("string-internal brackets must not trigger the depth guard");
        assert_eq!(events.len(), 1);
    }
}
