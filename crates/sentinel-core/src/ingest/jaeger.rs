//! Jaeger JSON ingestion: parses Jaeger JSON export format into `SpanEvent`.
//!
//! Jaeger exports traces as:
//! ```json
//! { "data": [{ "traceID": "...", "spans": [...], "processes": {...} }] }
//! ```
//!
//! `source.endpoint` walks the `CHILD_OF` chain with the same rules as the
//! OTLP path: nearest inbound HTTP route first, otherwise the outermost
//! application `code.*` frame, otherwise `"unknown"`.

use std::collections::HashMap;
use std::sync::Arc;

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
// same `{"data": [...]}` payload from the Jaeger query API. Kept at
// `pub(super)` scope so visibility stays within `crate::ingest`.

#[derive(Deserialize)]
pub(super) struct JaegerExport {
    pub(super) data: Vec<JaegerTrace>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JaegerTrace {
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

pub(super) fn convert_jaeger_export(export: &JaegerExport) -> Vec<SpanEvent> {
    let cap: usize = export.data.iter().map(|t| t.spans.len()).sum();
    let mut events = Vec::with_capacity(cap);
    for trace in &export.data {
        // Build the per-process Arc<str> once per trace, then Arc::clone
        // into each span. A trace routinely has hundreds of spans sharing
        // the same processID, so this collapses N allocations to one.
        let service_arcs: HashMap<&str, Arc<str>> = trace
            .processes
            .iter()
            .map(|(pid, p)| (pid.as_str(), Arc::from(p.service_name.as_str())))
            .collect();
        // Span index for the ancestor walk, per trace.
        let span_index: HashMap<&str, &JaegerSpan> = trace
            .spans
            .iter()
            .filter(|s| !s.span_id.is_empty())
            .map(|s| (s.span_id.as_str(), s))
            .collect();
        for span in &trace.spans {
            if let Some(event) =
                convert_jaeger_span(span, &trace.trace_id, &service_arcs, &span_index)
            {
                events.push(event);
            }
        }
    }
    events
}

/// Parent span id from the `CHILD_OF` reference, if any.
fn child_of(span: &JaegerSpan) -> Option<&str> {
    span.references
        .iter()
        .find(|r| r.ref_type == "CHILD_OF")
        .map(|r| r.span_id.as_str())
}

/// Inbound HTTP endpoint carried by this span's own tags: `http.route` on any
/// kind, `http.url`/`url.full` on any kind except CLIENT (an outbound call's
/// URL names the callee, not the route being served).
fn inbound_http_endpoint(span: &JaegerSpan) -> Option<String> {
    let usable = |s: &String| !s.trim().is_empty();
    find_tag(&span.tags, "http.route")
        .filter(usable)
        .or_else(|| {
            if find_tag(&span.tags, "span.kind").as_deref() == Some("client") {
                return None;
            }
            find_tag(&span.tags, "http.target")
                .or_else(|| find_tag(&span.tags, "http.url"))
                .or_else(|| find_tag(&span.tags, "url.full"))
                .filter(usable)
        })
}

/// Code-frame endpoint carried by this span's own tags, stable spellings
/// first, namespace derived from the qualified name as the OTLP path does.
fn tag_code_frame(tags: &[JaegerTag]) -> Option<String> {
    let function_name = find_tag(tags, "code.function.name");
    let function = function_name
        .clone()
        .or_else(|| find_tag(tags, "code.function"));
    let namespace = find_tag(tags, "code.namespace").or_else(|| {
        function_name
            .as_deref()
            .and_then(crate::ingest::namespace_from_qualified_name)
            .map(ToString::to_string)
    });
    crate::ingest::code_frame_endpoint(namespace.as_deref(), function.as_deref())
}

/// Walk the `CHILD_OF` chain: nearest inbound HTTP route wins, otherwise the
/// outermost usable code frame (starting from the leaf's own), otherwise
/// `"unknown"`. Same rules and depth bound as the OTLP path.
fn resolve_source_endpoint(
    leaf_frame: Option<String>,
    parent_id: Option<&str>,
    span_index: &HashMap<&str, &JaegerSpan>,
) -> String {
    let mut outermost_frame = leaf_frame;
    let mut current = parent_id;
    let mut depth = 0;
    while let Some(pid) = current {
        let Some(parent) = span_index.get(pid) else {
            break;
        };
        if let Some(route) = inbound_http_endpoint(parent) {
            return route;
        }
        if let Some(frame) = tag_code_frame(&parent.tags) {
            outermost_frame = Some(frame);
        }
        if depth >= crate::ingest::ANCESTOR_WALK_MAX_DEPTH {
            break;
        }
        current = child_of(parent);
        depth += 1;
    }
    outermost_frame.unwrap_or_else(|| "unknown".to_string())
}

fn convert_jaeger_span(
    span: &JaegerSpan,
    trace_id: &str,
    service_arcs: &HashMap<&str, Arc<str>>,
    span_index: &HashMap<&str, &JaegerSpan>,
) -> Option<SpanEvent> {
    let tags = &span.tags;

    // Determine event type from tags. Read the stable db.system.name before the
    // older db.system (matching the OTLP path) and canonicalize, so the same
    // engine labels and gates identically across ingest formats.
    let db_system_raw = find_tag(tags, "db.system.name").or_else(|| find_tag(tags, "db.system"));
    let db_system = db_system_raw
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(super::canonical_db_system);
    // Drop non-SQL datastore spans (Redis, MongoDB, ...) unconditionally:
    // their statement is not relational SQL and we do not model these stores.
    if db_system.is_some_and(super::is_non_sql_db_system) {
        return None;
    }
    let (event_type, target) = if let Some(stmt) =
        find_tag(tags, "db.statement").or_else(|| find_tag(tags, "db.query.text"))
    {
        (EventType::Sql, stmt)
    } else {
        // Not an I/O span unless it carries an HTTP target.
        (
            EventType::HttpOut,
            find_tag(tags, "http.url").or_else(|| find_tag(tags, "url.full"))?,
        )
    };

    // Operation. This path never yields Messaging, its gate admits SQL and
    // HTTP only, so messaging rides along with the outbound-call arm.
    let operation = match event_type {
        EventType::Sql => db_system.unwrap_or("sql").to_string(),
        EventType::HttpOut | EventType::Messaging => find_tag(tags, "http.method")
            .or_else(|| find_tag(tags, "http.request.method"))
            .unwrap_or_else(|| "GET".to_string()),
    };

    // Service name from the per-trace Arc cache, cloned (O(1)) per span.
    let service: Arc<str> = service_arcs
        .get(span.process_id.as_str())
        .map_or_else(|| Arc::from(""), Arc::clone);

    // Parent span ID from CHILD_OF reference
    let parent_span_id = child_of(span).map(ToString::to_string);

    // Status code (HTTP only)
    let status_code = match event_type {
        EventType::HttpOut | EventType::Messaging => find_tag(tags, "http.status_code")
            .or_else(|| find_tag(tags, "http.response.status_code"))
            .and_then(|s| s.parse().ok()),
        EventType::Sql => None,
    };

    // code.* attributes from span tags, stable semconv names first, same
    // precedence as the OTLP path.
    let code_function_name = find_tag(tags, "code.function.name");
    let code_function: Option<Arc<str>> = code_function_name
        .clone()
        .or_else(|| find_tag(tags, "code.function"))
        .map(Arc::from);
    let code_filepath: Option<Arc<str>> = find_tag(tags, "code.file.path")
        .or_else(|| find_tag(tags, "code.filepath"))
        .map(Arc::from);
    let code_lineno = find_tag(tags, "code.line.number")
        .or_else(|| find_tag(tags, "code.lineno"))
        .and_then(|s| s.parse::<u32>().ok());
    let code_namespace: Option<Arc<str>> = find_tag(tags, "code.namespace")
        .or_else(|| {
            code_function_name
                .as_deref()
                .and_then(crate::ingest::namespace_from_qualified_name)
                .map(ToString::to_string)
        })
        .map(Arc::from);

    // On a DB span an HTTP tag is the inbound route propagated onto it, so it
    // wins. On an outbound span it is the callee's path, so only the walk answers.
    let endpoint = match event_type {
        EventType::Sql => find_tag(tags, "http.route")
            .or_else(|| find_tag(tags, "http.target"))
            .filter(|s| !s.trim().is_empty()),
        EventType::HttpOut | EventType::Messaging => None,
    }
    .unwrap_or_else(|| resolve_source_endpoint(tag_code_frame(tags), child_of(span), span_index));
    let method = find_tag(tags, "code.function").unwrap_or_else(|| span.operation_name.clone());

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
        // Jaeger does not carry OpenTelemetry instrumentation scope
        // information. Empty list disables the scope-based framework
        // detection path; namespace heuristics still fire.
        instrumentation_scopes: Vec::new(),
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
    fn non_sql_datastore_span_is_dropped() {
        // A Redis span carries a db.statement that is not relational SQL;
        // it must be dropped, never tokenized as SQL.
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1", "operationName": "redis-get",
                        "references": [], "startTime": 1, "duration": 10, "processID": "p1",
                        "tags": [
                            { "key": "db.system", "value": "redis" },
                            { "key": "db.statement", "value": "GET user:123" }
                        ]
                    },
                    {
                        "spanID": "s2", "operationName": "sql",
                        "references": [], "startTime": 1, "duration": 10, "processID": "p1",
                        "tags": [
                            { "key": "db.system", "value": "postgresql" },
                            { "key": "db.statement", "value": "SELECT 1" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Sql);
        assert_eq!(events[0].operation, "postgresql");
    }

    #[test]
    fn db_system_alias_is_canonicalized() {
        // A Jaeger trace tagging db.system="postgres" must label the operation
        // "postgresql", same as the OTLP and Zipkin paths.
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1", "operationName": "q",
                    "startTime": 0, "duration": 100, "processID": "p1",
                    "tags": [
                        { "key": "db.system", "value": "postgres" },
                        { "key": "db.statement", "value": "SELECT 1" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Sql);
        assert_eq!(events[0].operation, "postgresql");
    }

    #[test]
    fn stable_db_system_name_non_sql_is_dropped() {
        // A non-SQL store reported only under the stable db.system.name key
        // ("aws.dynamodb") must be dropped, not tokenized as SQL: its statement
        // can carry a key/document value.
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [{
                    "spanID": "s1", "operationName": "q",
                    "startTime": 0, "duration": 100, "processID": "p1",
                    "tags": [
                        { "key": "db.system.name", "value": "aws.dynamodb" },
                        { "key": "db.statement", "value": "SELECT * FROM Orders WHERE Id = 'secret'" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
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
        assert_eq!(&*sql.service, "order-svc");
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
        assert_eq!(&*events[0].service, "");
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
    fn parent_span_http_route_takes_precedence_over_http_target() {
        // Jaeger reads endpoint tags from the current span (not the parent
        // like OTLP). When both http.route and http.target are present,
        // route must win so the ack signature stays stable.
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
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "http.route", "value": "POST /api/orders/{id}" },
                        { "key": "http.target", "value": "/api/orders/42" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "POST /api/orders/{id}");
    }

    #[test]
    fn http_target_used_only_when_route_absent() {
        // Documented Jaeger fallback: instrumentation that omits
        // http.route falls back to http.target. The endpoint string is
        // less stable but still useful.
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
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "http.target", "value": "/api/orders/42" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "/api/orders/42");
    }

    #[test]
    fn code_frame_used_when_no_http_tag() {
        // Non-HTTP entry point: without the code frame the endpoint would be
        // empty, which names no origin and collides in the ack signature.
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
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "code.function", "value": "execute" },
                        { "key": "code.namespace", "value": "com.foo.PurgeJob" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "com.foo.PurgeJob.execute");
    }

    #[test]
    fn endpoint_resolves_through_ancestors() {
        // The Spring shape the lab measured 0/43 on: the route sits on the
        // SERVER span two levels above the JDBC leaf, and the intermediate
        // CLIENT span's URL must not win over it.
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/orders",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.route", "value": "POST /api/orders" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "GET",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123100,
                        "duration": 3000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "client" },
                            { "key": "http.url", "value": "https://partner.example/v1/pay" }
                        ]
                    },
                    {
                        "spanID": "s3",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s2" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql leaf present");
        assert_eq!(sql.source.endpoint, "POST /api/orders");
    }

    #[test]
    fn walk_accepts_http_target_on_an_ancestor() {
        // An SDK older than semconv 1.23 records http.target and no
        // http.route. The leaf check accepted it, the walk must too.
        let json = r#"{
            "data": [{
                "traceID": "t1",
                "spans": [
                    {
                        "spanID": "s1",
                        "operationName": "POST /api/orders",
                        "references": [],
                        "startTime": 1720621921123000,
                        "duration": 5000,
                        "processID": "p1",
                        "tags": [
                            { "key": "span.kind", "value": "server" },
                            { "key": "http.target", "value": "/api/orders/42" }
                        ]
                    },
                    {
                        "spanID": "s2",
                        "operationName": "query",
                        "references": [{ "refType": "CHILD_OF", "spanID": "s1" }],
                        "startTime": 1720621921123200,
                        "duration": 500,
                        "processID": "p1",
                        "tags": [
                            { "key": "db.statement", "value": "SELECT 1" },
                            { "key": "db.system", "value": "postgresql" }
                        ]
                    }
                ],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql leaf present");
        assert_eq!(sql.source.endpoint, "/api/orders/42");
    }

    #[test]
    fn endpoint_falls_back_to_unknown_not_empty() {
        // The empty string put an empty component in the ack signature; the
        // documented fallback is "unknown" on every ingestion path.
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
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events[0].source.endpoint, "unknown");
    }

    #[test]
    fn code_frame_endpoint_reads_stable_semconv() {
        // An OTel 1.27+ agent emits only `code.function.name`. Reading the
        // legacy spelling alone left the endpoint empty here while the same
        // trace over OTLP resolved, so one ack could not cover both paths.
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
                        { "key": "db.statement", "value": "SELECT 1" },
                        { "key": "db.system", "value": "postgresql" },
                        { "key": "code.function.name", "value": "com.foo.PurgeJob.execute" }
                    ]
                }],
                "processes": { "p1": { "serviceName": "svc" } }
            }]
        }"#;
        let ingest = JaegerIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "com.foo.PurgeJob.execute");
        assert_eq!(
            events[0].code_namespace.as_deref(),
            Some("com.foo.PurgeJob"),
            "namespace must be derived from the FQ name, as the OTLP path does"
        );
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
