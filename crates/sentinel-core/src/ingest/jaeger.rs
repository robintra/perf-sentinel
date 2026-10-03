//! Jaeger JSON ingestion: parses Jaeger JSON export format into `SpanEvent`.
//!
//! Jaeger exports traces as:
//! ```json
//! { "data": [{ "traceID": "...", "spans": [...], "processes": {...} }] }
//! ```
//!
//! `source.endpoint` walks the `CHILD_OF` chain with the same rules as the
//! OTLP path: outermost inbound HTTP route in the event service first,
//! otherwise the outermost application `code.*` frame, otherwise the
//! destination of the nearest CONSUMER span, otherwise `"unknown"`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::event::{EventSource, SpanEvent};
use crate::ingest::IngestSource;
use crate::time::micros_to_iso8601;

/// Ingests span events from Jaeger JSON export format.
pub struct JaegerIngest {
    max_size: usize,
    /// `None` keeps the built-in default, `Some(vec![])` turns grouping off.
    grouping_attributes: Option<Vec<Arc<str>>>,
}

impl JaegerIngest {
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
        Ok(convert_jaeger_export(
            &export,
            self.grouping_attributes.as_deref(),
        ))
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
// HTTP-mode `jaeger_query` ingestion module, which receives the
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
    #[serde(default)]
    tags: Vec<JaegerTag>,
}

type ProcessMetadata<'a> = (Arc<str>, &'a [JaegerTag]);

#[derive(Deserialize)]
struct JaegerTag {
    key: String,
    value: serde_json::Value,
}

// ── Conversion ─────────────────────────────────────────────────────

pub(super) fn convert_jaeger_export(
    export: &JaegerExport,
    grouping_attributes: Option<&[Arc<str>]>,
) -> Vec<SpanEvent> {
    let cap: usize = export.data.iter().map(|t| t.spans.len()).sum();
    let mut events = Vec::with_capacity(cap);
    for trace in &export.data {
        // Build the per-process Arc<str> once per trace, then Arc::clone
        // into each span. A trace routinely has hundreds of spans sharing
        // the same processID, so this collapses N allocations to one.
        let process_metadata: HashMap<&str, ProcessMetadata> = trace
            .processes
            .iter()
            .map(|(pid, process)| {
                (
                    pid.as_str(),
                    (
                        Arc::from(process.service_name.as_str()),
                        process.tags.as_slice(),
                    ),
                )
            })
            .collect();
        // Span index for the ancestor walk, per trace.
        let span_index: HashMap<&str, &JaegerSpan> = trace
            .spans
            .iter()
            .filter(|s| !s.span_id.is_empty())
            .map(|s| (s.span_id.as_str(), s))
            .collect();
        for span in &trace.spans {
            if let Some(event) = convert_jaeger_span(
                span,
                &trace.trace_id,
                &process_metadata,
                &span_index,
                grouping_attributes,
            ) {
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

/// Inbound HTTP endpoint carried by an ancestor span: `http.route` on any
/// kind, remaining HTTP fallbacks on any kind except CLIENT. Unspecified
/// kinds remain eligible for legacy instrumentation.
fn inbound_http_endpoint(span: &JaegerSpan) -> Option<String> {
    http_endpoint(
        span,
        find_tag(&span.tags, "span.kind").as_deref() != Some("client"),
    )
}

/// Inbound endpoint carried by the event span itself. A route template is a
/// safe inbound signal on any kind. Legacy URL fallbacks require SERVER.
fn own_inbound_http_endpoint(span: &JaegerSpan) -> Option<String> {
    http_endpoint(
        span,
        find_tag(&span.tags, "span.kind").as_deref() == Some("server"),
    )
}

fn http_endpoint(span: &JaegerSpan, allow_url_fallback: bool) -> Option<String> {
    let usable = |s: &String| !s.trim().is_empty();
    let route = find_tag(&span.tags, "http.route");
    let url_path = find_tag(&span.tags, "url.path");
    crate::ingest::http_route_endpoint(route.as_deref(), url_path.as_deref(), allow_url_fallback)
        .or_else(|| {
            if !allow_url_fallback {
                return None;
            }
            find_tag(&span.tags, "http.target")
                .filter(usable)
                .or_else(|| find_tag(&span.tags, "http.url").filter(usable))
                .or_else(|| find_tag(&span.tags, "url.full").filter(usable))
                .or_else(|| find_tag(&span.tags, "url.path").filter(usable))
        })
}

fn own_sql_http_endpoint(span: &JaegerSpan) -> Option<String> {
    crate::ingest::http_route_endpoint(
        find_tag(&span.tags, "http.route").as_deref(),
        find_tag(&span.tags, "url.path").as_deref(),
        find_tag(&span.tags, "span.kind").as_deref() == Some("server"),
    )
    .or_else(|| find_tag(&span.tags, "http.target"))
    .filter(|endpoint| !endpoint.trim().is_empty())
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

/// Message-driven entry endpoint of a CONSUMER span, see
/// [`crate::ingest::consumer_entry_endpoint`].
fn consumer_entry_endpoint(tags: &[JaegerTag]) -> Option<String> {
    if find_tag(tags, "span.kind").as_deref() != Some("consumer") {
        return None;
    }
    let flag = |key| find_tag(tags, key).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    crate::ingest::consumer_entry_endpoint(
        find_tag(tags, "messaging.system").as_deref(),
        find_tag(tags, "messaging.destination.template").as_deref(),
        find_tag(tags, "messaging.destination.name").as_deref(),
        find_tag(tags, "messaging.destination").as_deref(),
        flag("messaging.destination.temporary"),
        flag("messaging.destination.anonymous"),
    )
}

fn same_jaeger_service(
    leaf: &JaegerSpan,
    ancestor: &JaegerSpan,
    process_metadata: &HashMap<&str, ProcessMetadata>,
) -> bool {
    if leaf.process_id == ancestor.process_id {
        return true;
    }
    match (
        process_metadata
            .get(leaf.process_id.as_str())
            .map(|metadata| metadata.0.as_ref())
            .filter(|service| !service.is_empty()),
        process_metadata
            .get(ancestor.process_id.as_str())
            .map(|metadata| metadata.0.as_ref())
            .filter(|service| !service.is_empty()),
    ) {
        (Some(leaf_service), Some(ancestor_service)) => leaf_service == ancestor_service,
        _ => false,
    }
}

/// Walk the contiguous same-service `CHILD_OF` chain: the outermost inbound
/// HTTP route wins, otherwise the outermost usable code frame (starting from
/// the leaf's own), otherwise the nearest consumer destination, otherwise
/// `"unknown"`. Same depth bound as the OTLP path.
fn resolve_source_endpoint(
    own_endpoint: Option<String>,
    leaf_frame: Option<String>,
    leaf: &JaegerSpan,
    process_metadata: &HashMap<&str, ProcessMetadata>,
    span_index: &HashMap<&str, &JaegerSpan>,
) -> String {
    let mut outermost_endpoint = own_endpoint;
    let mut outermost_frame = leaf_frame;
    let mut nearest_consumer = None;
    let mut current = child_of(leaf);
    for _ in 0..crate::ingest::ANCESTOR_WALK_MAX_DEPTH {
        let Some(pid) = current else {
            break;
        };
        let Some(parent) = span_index.get(pid) else {
            break;
        };
        if !same_jaeger_service(leaf, parent, process_metadata) {
            break;
        }
        if let Some(route) = inbound_http_endpoint(parent) {
            outermost_endpoint = Some(route);
        }
        if let Some(frame) = tag_code_frame(&parent.tags) {
            outermost_frame = Some(frame);
        }
        if nearest_consumer.is_none() {
            nearest_consumer = consumer_entry_endpoint(&parent.tags);
        }
        current = child_of(parent);
    }
    outermost_endpoint
        .or(outermost_frame)
        .or(nearest_consumer)
        .unwrap_or_else(|| "unknown".to_string())
}

fn convert_jaeger_span(
    span: &JaegerSpan,
    trace_id: &str,
    process_metadata: &HashMap<&str, ProcessMetadata>,
    span_index: &HashMap<&str, &JaegerSpan>,
    grouping_attributes: Option<&[Arc<str>]>,
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
    let (io_kind, target) = if let Some(stmt) =
        find_tag(tags, "db.statement").or_else(|| find_tag(tags, "db.query.text"))
    {
        (super::TagIoKind::Sql, stmt)
    } else {
        if find_tag(tags, "span.kind").as_deref() == Some("server") {
            return None;
        }
        // A SERVER URL describes the inbound request, not an outbound call.
        // Unspecified legacy spans remain eligible for HTTP classification.
        (
            super::TagIoKind::HttpOut,
            find_tag(tags, "http.url").or_else(|| find_tag(tags, "url.full"))?,
        )
    };
    let operation = match io_kind {
        super::TagIoKind::Sql => db_system.unwrap_or("sql").to_string(),
        super::TagIoKind::HttpOut => http_method(tags),
    };

    // Service name from the per-trace Arc cache, cloned (O(1)) per span.
    let process = process_metadata.get(span.process_id.as_str());
    let service: Arc<str> = process.map_or_else(
        || Arc::from(crate::event::UNKNOWN_SERVICE),
        |m| Arc::clone(&m.0),
    );

    // Process tags win over span tags: they describe the emitter, a span
    // tag with the same name is a per-request override.
    let grouping = crate::ingest::collect_grouping(grouping_attributes, |key| {
        process
            .and_then(|m| find_tag(m.1, key).map(Arc::from))
            .or_else(|| find_tag(tags, key).map(Arc::from))
    });

    let parent_span_id = child_of(span).map(ToString::to_string);

    let status_code = match io_kind {
        super::TagIoKind::HttpOut => http_status_code(tags),
        super::TagIoKind::Sql => None,
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
    let endpoint = resolve_source_endpoint(
        match io_kind {
            super::TagIoKind::Sql => own_sql_http_endpoint(span),
            super::TagIoKind::HttpOut => own_inbound_http_endpoint(span),
        },
        tag_code_frame(tags),
        span,
        process_metadata,
        span_index,
    );
    let method = find_tag(tags, "code.function").unwrap_or_else(|| span.operation_name.clone());

    let mut event = SpanEvent {
        timestamp: micros_to_iso8601(span.start_time),
        trace_id: trace_id.to_string(),
        span_id: span.span_id.clone(),
        // Jaeger models span links as references, not read here.
        link_trace_id: None,
        parent_span_id,
        service,
        grouping,
        // Jaeger process tags do not carry cloud region. Users wanting
        // multi-region scoring with Jaeger ingestion should set
        // [green.service_regions] in the config to map services to regions.
        cloud_region: None,
        event_type: io_kind.event_type(),
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
        // information. An empty list disables the scope-based framework
        // detection path. Namespace heuristics still fire.
        instrumentation_scopes: Vec::new(),
    };
    crate::event::sanitize_span_event(&mut event);
    Some(event)
}

/// HTTP verb of an outbound span: legacy, then stable semconv, then the
/// Micrometer Observation `method` tag (only reached on a span with a URL).
fn http_method(tags: &[JaegerTag]) -> String {
    find_tag(tags, "http.method")
        .or_else(|| find_tag(tags, "http.request.method"))
        .or_else(|| find_tag(tags, "method"))
        .unwrap_or_else(|| "GET".to_string())
}

/// HTTP status of an outbound span, same precedence as [`http_method`].
fn http_status_code(tags: &[JaegerTag]) -> Option<u16> {
    find_tag(tags, "http.status_code")
        .or_else(|| find_tag(tags, "http.response.status_code"))
        .or_else(|| find_tag(tags, "status"))
        .and_then(|s| s.parse().ok())
}

fn find_tag(tags: &[JaegerTag], key: &str) -> Option<String> {
    tags.iter().find(|t| t.key == key).map(|t| match &t.value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests;
