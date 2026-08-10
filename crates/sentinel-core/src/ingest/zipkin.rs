//! Zipkin JSON v2 ingestion: parses Zipkin JSON spans into `SpanEvent`.
//!
//! Zipkin v2 format is a flat array of span objects:
//! ```json
//! [{ "traceId": "...", "id": "...", "parentId": "...", ... }]
//! ```
//!
//! `source.endpoint` walks the `parentId` chain with the same rules as the
//! OTLP path: outermost inbound HTTP route in the event service first,
//! otherwise the outermost application `code.*` frame, otherwise `"unknown"`.

use crate::event::{EventSource, SpanEvent};
use crate::ingest::IngestSource;
use crate::time::micros_to_iso8601;

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

/// Ingests span events from Zipkin JSON v2 format.
pub struct ZipkinIngest {
    max_size: usize,
    /// `None` keeps the built-in default, `Some(vec![])` turns grouping off.
    grouping_attributes: Option<Vec<Arc<str>>>,
}

impl ZipkinIngest {
    #[must_use]
    pub const fn new(max_size: usize) -> Self {
        Self {
            max_size,
            grouping_attributes: None,
        }
    }

    /// Override which attributes separate deployments.
    #[must_use]
    pub fn with_grouping_attributes(mut self, keys: Vec<Arc<str>>) -> Self {
        self.grouping_attributes = Some(keys);
        self
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
        Ok(convert_zipkin_spans(
            &spans,
            self.grouping_attributes.as_deref(),
        ))
    }
}

/// Errors that can occur during Zipkin JSON ingestion.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    kind: Option<String>,
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

fn convert_zipkin_spans(
    spans: &[ZipkinSpan],
    grouping_attributes: Option<&[Arc<str>]>,
) -> Vec<SpanEvent> {
    // Keyed by (trace, id): one payload interleaves traces, and both halves
    // of a shared RPC span share an id, so the CLIENT half must not displace
    // the SERVER half that carries the route.
    let mut span_index: HashMap<(&str, &str), &ZipkinSpan> = HashMap::new();
    for span in spans.iter().filter(|s| !s.id.is_empty()) {
        let key = (span.trace_id.as_str(), span.id.as_str());
        match span_index.get(&key) {
            Some(kept) if kept.kind.as_deref() != Some("CLIENT") => {}
            _ => {
                span_index.insert(key, span);
            }
        }
    }
    spans
        .iter()
        .filter_map(|s| convert_zipkin_span(s, &span_index, grouping_attributes))
        .collect()
}

/// Inbound HTTP endpoint carried by an ancestor span: `http.route` on any
/// kind, remaining HTTP fallbacks on any kind except CLIENT. Unspecified
/// kinds remain eligible for legacy instrumentation.
fn inbound_http_endpoint(span: &ZipkinSpan) -> Option<String> {
    http_endpoint(span, span.kind.as_deref() != Some("CLIENT"))
}

/// Inbound endpoint carried by the event span itself. A route template is a
/// safe inbound signal on any kind; legacy URL fallbacks require SERVER.
fn own_inbound_http_endpoint(span: &ZipkinSpan) -> Option<String> {
    http_endpoint(span, span.kind.as_deref() == Some("SERVER"))
}

fn http_endpoint(span: &ZipkinSpan, allow_url_fallback: bool) -> Option<String> {
    let tag = |key: &str| {
        span.tags
            .as_ref()
            .and_then(|t| t.get(key).map(String::as_str))
            .filter(|s| !s.trim().is_empty())
    };
    crate::ingest::http_route_endpoint(tag("http.route"), tag("url.path"), allow_url_fallback)
        .or_else(|| {
            if !allow_url_fallback {
                return None;
            }
            tag("http.target")
                .or_else(|| tag("http.url"))
                .or_else(|| tag("url.full"))
                .or_else(|| tag("url.path"))
                .map(ToString::to_string)
        })
}

/// Code-frame endpoint carried by this span's own tags, stable spellings
/// first, namespace derived from the qualified name as the OTLP path does.
fn tag_code_frame(span: &ZipkinSpan) -> Option<String> {
    let tag = |key: &str| {
        span.tags
            .as_ref()
            .and_then(|t| t.get(key).map(String::as_str))
    };
    let function_name = tag("code.function.name");
    let function = function_name.or_else(|| tag("code.function"));
    let namespace = tag("code.namespace").map(ToString::to_string).or_else(|| {
        function_name
            .and_then(crate::ingest::namespace_from_qualified_name)
            .map(ToString::to_string)
    });
    crate::ingest::code_frame_endpoint(namespace.as_deref(), function)
}

fn zipkin_service_name(span: &ZipkinSpan) -> Option<&str> {
    span.local_endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.service_name.as_deref())
        .filter(|service| !service.is_empty())
}

fn same_zipkin_service(leaf: &ZipkinSpan, ancestor: &ZipkinSpan) -> bool {
    matches!(
        (zipkin_service_name(leaf), zipkin_service_name(ancestor)),
        (Some(leaf_service), Some(ancestor_service)) if leaf_service == ancestor_service
    )
}

/// Whether the ancestor walk stops before `parent`. An anonymous leaf stays
/// inside the anonymous run and stops at the first named ancestor, a named
/// leaf stops at the first ancestor of another service.
fn walk_stops_at(anonymous_service: bool, leaf: &ZipkinSpan, parent: &ZipkinSpan) -> bool {
    if anonymous_service {
        zipkin_service_name(parent).is_some()
    } else {
        !same_zipkin_service(leaf, parent)
    }
}

/// Walk the contiguous same-service `parentId` chain: the outermost inbound
/// HTTP route wins, otherwise the outermost usable code frame (starting from
/// the leaf's own), otherwise `"unknown"`. Same depth bound as the OTLP path.
fn resolve_source_endpoint(
    own_endpoint: Option<String>,
    leaf: &ZipkinSpan,
    span_index: &HashMap<(&str, &str), &ZipkinSpan>,
) -> String {
    let anonymous_service = zipkin_service_name(leaf).is_none();
    if anonymous_service && let Some(endpoint) = own_endpoint {
        return endpoint;
    }
    let mut outermost_endpoint = own_endpoint;
    let mut outermost_frame = tag_code_frame(leaf);
    let mut current = leaf.parent_id.as_deref();
    for _ in 0..crate::ingest::ANCESTOR_WALK_MAX_DEPTH {
        let Some(pid) = current else {
            break;
        };
        let Some(parent) = span_index.get(&(leaf.trace_id.as_str(), pid)) else {
            break;
        };
        if walk_stops_at(anonymous_service, leaf, parent) {
            break;
        }
        if let Some(route) = inbound_http_endpoint(parent) {
            if anonymous_service {
                return route;
            }
            outermost_endpoint = Some(route);
        }
        if let Some(frame) = tag_code_frame(parent)
            && (!anonymous_service || outermost_frame.is_none())
        {
            outermost_frame = Some(frame);
        }
        current = parent.parent_id.as_deref();
    }
    outermost_endpoint
        .or(outermost_frame)
        .unwrap_or_else(|| "unknown".to_string())
}

fn convert_zipkin_span(
    span: &ZipkinSpan,
    span_index: &HashMap<(&str, &str), &ZipkinSpan>,
    grouping_attributes: Option<&[Arc<str>]>,
) -> Option<SpanEvent> {
    let tags = span.tags.as_ref();

    let get_tag = |key: &str| -> Option<&str> { tags.and_then(|t| t.get(key).map(String::as_str)) };

    // Determine event type from tags. Read the stable db.system.name before the
    // older db.system (matching the OTLP path) and canonicalize, so the same
    // engine labels and gates identically across ingest formats.
    let db_system = get_tag("db.system.name")
        .or_else(|| get_tag("db.system"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(super::canonical_db_system);
    // Drop non-SQL datastore spans (Redis, MongoDB, ...) unconditionally:
    // their statement is not relational SQL and we do not model these stores.
    if db_system.is_some_and(super::is_non_sql_db_system) {
        return None;
    }
    let (io_kind, target) =
        if let Some(stmt) = get_tag("db.statement").or_else(|| get_tag("db.query.text")) {
            (super::TagIoKind::Sql, stmt.to_string())
        } else {
            if span.kind.as_deref() == Some("SERVER") {
                return None;
            }
            // A SERVER URL describes the inbound request, not an outbound call.
            // Unspecified legacy spans remain eligible for HTTP classification.
            (
                super::TagIoKind::HttpOut,
                get_tag("http.url")
                    .or_else(|| get_tag("url.full"))?
                    .to_string(),
            )
        };
    let event_type = io_kind.event_type();

    let operation = match io_kind {
        super::TagIoKind::Sql => db_system.unwrap_or("sql").to_string(),
        super::TagIoKind::HttpOut => get_tag("http.method")
            .or_else(|| get_tag("http.request.method"))
            .unwrap_or("GET")
            .to_string(),
    };

    let service: Arc<str> = zipkin_service_name(span).map_or_else(|| Arc::from(""), Arc::from);
    let grouping =
        crate::ingest::collect_grouping(grouping_attributes, |key| get_tag(key).map(Arc::from));

    let timestamp = span.timestamp.unwrap_or(0);
    let duration_us = span.duration.unwrap_or(0);

    let status_code = match io_kind {
        super::TagIoKind::HttpOut => get_tag("http.status_code")
            .or_else(|| get_tag("http.response.status_code"))
            .and_then(|s| s.parse().ok()),
        super::TagIoKind::Sql => None,
    };

    // code.* attributes from span tags, stable semconv names first, same
    // precedence as the OTLP path.
    let code_function_name = get_tag("code.function.name");
    let code_function: Option<Arc<str>> = code_function_name
        .or_else(|| get_tag("code.function"))
        .map(Arc::from);
    let code_filepath: Option<Arc<str>> = get_tag("code.file.path")
        .or_else(|| get_tag("code.filepath"))
        .map(Arc::from);
    let code_lineno = get_tag("code.line.number")
        .or_else(|| get_tag("code.lineno"))
        .and_then(|s| s.parse::<u32>().ok());
    let code_namespace: Option<Arc<str>> = get_tag("code.namespace").map(Arc::from).or_else(|| {
        code_function_name
            .and_then(crate::ingest::namespace_from_qualified_name)
            .map(Arc::from)
    });

    // On a DB span an HTTP tag is the inbound route propagated onto it, so it
    // wins. On an outbound span it is the callee's path, so only the walk answers.
    let own_endpoint = match io_kind {
        super::TagIoKind::Sql => crate::ingest::http_route_endpoint(
            get_tag("http.route"),
            get_tag("url.path"),
            span.kind.as_deref() == Some("SERVER"),
        )
        .or_else(|| get_tag("http.target").map(ToString::to_string))
        .filter(|s| !s.trim().is_empty()),
        super::TagIoKind::HttpOut => own_inbound_http_endpoint(span),
    };
    let endpoint = resolve_source_endpoint(own_endpoint, span, span_index);
    let method = get_tag("code.function")
        .map(String::from)
        .or_else(|| span.name.clone())
        .unwrap_or_default();

    let mut event = SpanEvent {
        timestamp: micros_to_iso8601(timestamp),
        trace_id: span.trace_id.clone(),
        span_id: span.id.clone(),
        parent_span_id: span.parent_id.clone(),
        // Zipkin v2 has no span links.
        link_trace_id: None,
        service,
        grouping,
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
        response_size_bytes: None,
        code_function,
        code_filepath,
        code_lineno,
        code_namespace,
        // Zipkin does not carry OpenTelemetry instrumentation scope
        // information. Empty list disables the scope-based framework
        // detection path; namespace heuristics still fire.
        instrumentation_scopes: Vec::new(),
    };
    crate::event::sanitize_span_event(&mut event);
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

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
    fn namespaces_are_extracted_from_span_tags() {
        let json = r#"[{
            "traceId": "t1",
            "id": "s1",
            "name": "query",
            "timestamp": 1720621921123000,
            "duration": 1200,
            "localEndpoint": { "serviceName": "svc" },
            "tags": {
                "db.statement": "SELECT 1",
                "service.namespace": "payments",
                "k8s.namespace.name": "prod-eu"
            }
        }]"#;

        let events = ZipkinIngest::new(64 * 1024)
            .ingest(json.as_bytes())
            .unwrap();

        let captured: Vec<(&str, &str)> = events[0]
            .grouping
            .iter()
            .map(|g| (g.key.as_ref(), g.value.as_ref()))
            .collect();
        assert_eq!(
            captured,
            vec![
                ("k8s.namespace.name", "prod-eu"),
                ("service.namespace", "payments"),
            ],
            "both values are kept, config order, Kubernetes first"
        );
        assert_eq!(events[0].grouping_value(), Some("prod-eu"));
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
        assert_eq!(&*sql.service, "order-svc");
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
    fn non_sql_datastore_span_is_dropped() {
        // A Redis span carries a db.statement that is not relational SQL;
        // it must be dropped, never tokenized as SQL.
        let json = r#"[
            {
                "traceId": "t1", "id": "s1",
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "db.system": "redis", "db.statement": "GET user:123" }
            },
            {
                "traceId": "t1", "id": "s2",
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "db.system": "postgresql", "db.statement": "SELECT 1" }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Sql);
        assert_eq!(events[0].operation, "postgresql");
    }

    #[test]
    fn db_system_alias_is_canonicalized() {
        // A Zipkin span tagging db.system="postgres" must label the operation
        // "postgresql", same as the OTLP and Jaeger paths.
        let json = r#"[
            {
                "traceId": "t1", "id": "s1",
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "db.system": "postgres", "db.statement": "SELECT 1" }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Sql);
        assert_eq!(events[0].operation, "postgresql");
    }

    #[test]
    fn stable_db_system_name_non_sql_is_dropped() {
        // A non-SQL store reported only under the stable db.system.name key
        // ("aws.dynamodb") must be dropped, not tokenized as SQL.
        let json = r#"[
            {
                "traceId": "t1", "id": "s1",
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "db.system.name": "aws.dynamodb", "db.statement": "GET key" }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert!(events.is_empty());
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
        assert_eq!(&*events[0].service, "");
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
    fn slashless_http_route_is_canonicalized_before_http_target() {
        // Zipkin reads endpoint tags from the current span. When both
        // http.route and http.target are present, route must win so the
        // ack signature stays stable.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "query",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql",
                    "http.route": "api/orders/{id}",
                    "http.target": "/api/orders/42"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "/api/orders/{id}");
    }

    #[test]
    fn named_route_uses_url_path_for_own_and_ancestor_endpoints() {
        let json = r#"[
            {
                "traceId": "t1",
                "id": "root",
                "name": "request",
                "kind": "SERVER",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "symfony-svc" },
                "tags": {
                    "http.route": "app_fault_nplusonesql",
                    "url.path": "/api/fault/n-plus-one-sql"
                }
            },
            {
                "traceId": "t1",
                "id": "child",
                "parentId": "root",
                "name": "query",
                "timestamp": 1720621921123100,
                "duration": 500,
                "localEndpoint": { "serviceName": "symfony-svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql"
                }
            },
            {
                "traceId": "t2",
                "id": "own",
                "name": "query",
                "kind": "SERVER",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "symfony-svc" },
                "tags": {
                    "db.statement": "SELECT 2",
                    "db.system": "postgresql",
                    "http.route": "app_fault_nplusonesql",
                    "url.path": "/api/fault/n-plus-one-sql"
                }
            }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();

        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| event.source.endpoint == "/api/fault/n-plus-one-sql")
        );
    }

    #[test]
    fn server_route_and_legacy_url_is_context_not_http_out() {
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "kind": "SERVER",
                "name": "post /api/orders/{id}",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "http.route": "api/orders/{id}",
                    "http.url": "http://order-svc/api/orders/42"
                }
            }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn server_url_full_without_route_is_context_not_http_out() {
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "kind": "SERVER",
                "name": "post /api/orders/42",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "url.full": "http://order-svc/api/orders/42" }
            }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn unspecified_outgoing_url_uses_its_parent_server_route() {
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "kind": "SERVER",
                "name": "post /api/orders",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "http.route": "api/orders" }
            },
            {
                "traceId": "t1",
                "id": "s2",
                "parentId": "s1",
                "name": "get",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "http.url": "https://partner.example/v1/pay" }
            }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();
        let outgoing = events
            .iter()
            .find(|event| event.event_type == EventType::HttpOut)
            .expect("outgoing event present");

        assert_eq!(outgoing.source.endpoint, "/api/orders");
    }

    #[test]
    fn unspecified_root_url_does_not_self_source() {
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "get",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "http.url": "https://partner.example/v1/pay" }
            }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::HttpOut);
        assert_eq!(events[0].source.endpoint, "unknown");
    }

    #[test]
    fn http_target_used_only_when_route_absent() {
        // Documented Zipkin fallback: instrumentation that omits
        // http.route falls back to http.target.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "query",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql",
                    "http.target": "/api/orders/42"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "/api/orders/42");
    }

    #[test]
    fn code_frame_used_when_no_http_tag() {
        // A job span carrying its own frame lands on the same endpoint as
        // over OTLP.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "query",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql",
                    "code.function.name": "com.foo.PurgeJob.execute"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "com.foo.PurgeJob.execute");
    }

    #[test]
    fn endpoint_resolves_through_ancestors() {
        // Route on the SERVER span two levels up, an outbound CLIENT span in
        // between whose URL must not win, SQL leaf at the bottom.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "post /api/orders",
                "kind": "SERVER",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "http.route": "api/orders" }
            },
            {
                "traceId": "t1",
                "id": "s2",
                "parentId": "s1",
                "name": "get",
                "kind": "CLIENT",
                "timestamp": 1720621921123100,
                "duration": 3000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "http.url": "https://partner.example/v1/pay",
                    "url.path": "/v1/pay"
                }
            },
            {
                "traceId": "t1",
                "id": "s3",
                "parentId": "s2",
                "name": "query",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql leaf present");
        assert_eq!(sql.source.endpoint, "/api/orders");
    }

    #[test]
    fn outermost_route_stops_at_the_service_boundary() {
        let json = r#"[
            {
                "traceId": "same-service",
                "id": "outer",
                "kind": "SERVER",
                "name": "post /api/fault/pool-saturation",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "laravel-svc" },
                "tags": { "http.route": "/api/fault/pool-saturation" }
            },
            {
                "traceId": "same-service",
                "id": "nested",
                "parentId": "outer",
                "kind": "SERVER",
                "name": "get /api/payments/history",
                "timestamp": 1720621921123100,
                "duration": 3000,
                "localEndpoint": { "serviceName": "laravel-svc" },
                "tags": { "http.route": "/api/payments/history" }
            },
            {
                "traceId": "same-service",
                "id": "sql",
                "parentId": "nested",
                "name": "query",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "laravel-svc" },
                "tags": {
                    "db.statement": "SELECT * FROM payments",
                    "db.system": "postgresql"
                }
            },
            {
                "traceId": "cross-service",
                "id": "caller",
                "kind": "SERVER",
                "name": "post /api/orders",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "orders-svc" },
                "tags": { "http.route": "/api/orders" }
            },
            {
                "traceId": "cross-service",
                "id": "callee",
                "parentId": "caller",
                "kind": "SERVER",
                "name": "get /api/payments/history",
                "timestamp": 1720621921123100,
                "duration": 3000,
                "localEndpoint": { "serviceName": "payments-svc" },
                "tags": { "http.route": "/api/payments/history" }
            },
            {
                "traceId": "cross-service",
                "id": "sql",
                "parentId": "callee",
                "name": "query",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "payments-svc" },
                "tags": {
                    "db.statement": "SELECT * FROM payments",
                    "db.system": "postgresql"
                }
            }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();
        let same_service = events
            .iter()
            .find(|event| event.trace_id == "same-service")
            .expect("same-service SQL event present");
        let cross_service = events
            .iter()
            .find(|event| event.trace_id == "cross-service")
            .expect("cross-service SQL event present");

        assert_eq!(same_service.source.endpoint, "/api/fault/pool-saturation");
        assert_eq!(cross_service.source.endpoint, "/api/payments/history");
        assert_eq!(&*cross_service.service, "payments-svc");
    }

    #[test]
    fn absent_services_keep_the_nearest_route() {
        let json = r#"[
            { "traceId": "t", "id": "caller", "kind": "SERVER",
              "tags": { "http.route": "/api/orders" } },
            { "traceId": "t", "id": "callee", "parentId": "caller", "kind": "SERVER",
              "tags": { "http.route": "/api/payments/history" } },
            { "traceId": "t", "id": "sql", "parentId": "callee",
              "tags": { "db.statement": "SELECT 1", "db.system": "postgresql" } }
        ]"#;

        let events = ZipkinIngest::new(1_048_576)
            .ingest(json.as_bytes())
            .unwrap();
        assert_eq!(events[0].source.endpoint, "/api/payments/history");
    }

    #[test]
    fn route_at_hop_nine_is_outside_the_shared_depth_limit() {
        let mut spans = vec![serde_json::json!({
            "traceId": "t", "id": "p9", "kind": "SERVER",
            "localEndpoint": { "serviceName": "svc" },
            "tags": { "http.route": "/too-deep" }
        })];
        for id in (1_u8..9).rev() {
            spans.push(serde_json::json!({
                "traceId": "t", "id": format!("p{id}"), "parentId": format!("p{}", id + 1),
                "localEndpoint": { "serviceName": "svc" },
                "tags": if id == 8 {
                    serde_json::json!({ "http.route": "/at-limit" })
                } else {
                    serde_json::json!({})
                }
            }));
        }
        spans.push(serde_json::json!({
            "traceId": "t", "id": "sql", "parentId": "p1",
            "localEndpoint": { "serviceName": "svc" },
            "tags": { "db.statement": "SELECT 1", "db.system": "postgresql" }
        }));
        let payload = serde_json::to_string(&spans).unwrap();

        let events = ZipkinIngest::new(1_048_576)
            .ingest(payload.as_bytes())
            .unwrap();
        assert_eq!(events[0].source.endpoint, "/at-limit");
    }

    #[test]
    fn shared_span_keeps_the_server_half() {
        // Zipkin reports both halves of an RPC under one trace+id pair. The
        // CLIENT half carries the callee's URL and no route, so letting it
        // win the index would send every child finding to "unknown".
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "kind": "SERVER",
                "name": "post /api/orders",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "http.route": "POST /api/orders" }
            },
            {
                "traceId": "t1",
                "id": "s1",
                "kind": "CLIENT",
                "shared": true,
                "name": "post /api/orders",
                "timestamp": 1720621921122000,
                "duration": 6000,
                "localEndpoint": { "serviceName": "caller" },
                "tags": { "http.url": "https://svc-b/api/orders" }
            },
            {
                "traceId": "t1",
                "id": "s2",
                "parentId": "s1",
                "name": "query",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql leaf present");
        assert_eq!(sql.source.endpoint, "POST /api/orders");
    }

    #[test]
    fn outbound_span_does_not_take_its_own_http_target() {
        // On an outbound span the HTTP tag is the callee's path, not the
        // route being served, so only the walk answers.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "kind": "SERVER",
                "name": "post /api/orders",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "http.route": "POST /api/orders" }
            },
            {
                "traceId": "t1",
                "id": "s2",
                "parentId": "s1",
                "kind": "CLIENT",
                "name": "get",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "http.url": "https://partner.example/v1/pay",
                    "http.target": "/v1/pay",
                    "url.path": "/v1/pay"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        let out = events
            .iter()
            .find(|e| e.event_type == EventType::HttpOut)
            .expect("outbound event present");
        assert_eq!(out.source.endpoint, "POST /api/orders");
        assert_eq!(out.target, "https://partner.example/v1/pay");
    }

    #[test]
    fn parent_stable_url_path_provides_source_endpoint() {
        // Stable SERVER spans use url.path when no route template is available.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "post /api/fault/pool-saturation",
                "kind": "SERVER",
                "timestamp": 1720621921123000,
                "duration": 5000,
                "localEndpoint": { "serviceName": "svc" },
                "tags": { "url.path": "/api/fault/pool-saturation" }
            },
            {
                "traceId": "t1",
                "id": "s2",
                "parentId": "s1",
                "name": "query",
                "timestamp": 1720621921123200,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql",
                    "code.function.name": "com.foo.FaultPool.query"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "/api/fault/pool-saturation");
    }

    #[test]
    fn endpoint_falls_back_to_unknown_not_empty() {
        // The empty string put an empty component in the ack signature; the
        // documented fallback is "unknown" on every ingestion path.
        let json = r#"[
            {
                "traceId": "t1",
                "id": "s1",
                "name": "query",
                "timestamp": 1720621921123000,
                "duration": 500,
                "localEndpoint": { "serviceName": "svc" },
                "tags": {
                    "db.statement": "SELECT 1",
                    "db.system": "postgresql"
                }
            }
        ]"#;
        let ingest = ZipkinIngest::new(1_048_576);
        let events = ingest.ingest(json.as_bytes()).unwrap();
        assert_eq!(events[0].source.endpoint, "unknown");
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
