//! OTLP ingestion: maps OpenTelemetry spans to `SpanEvent`.
//!
//! Supports both gRPC (tonic `TraceService`) and HTTP (axum handler) ingestion.
//! Uses the `opentelemetry-proto` crate for protobuf definitions.

use std::collections::HashMap;
use std::sync::Arc;

use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{KeyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::Span;
use tonic::{Request, Response, Status, async_trait};

use crate::event::{EventSource, EventType, SpanEvent};
use crate::report::metrics::{OtlpRejectReason, OtlpSpanFilterReason};

/// Sink for the rejection counters this module emits, decoupling
/// `ingest` from the concrete metrics implementation. `MetricsState`
/// implements it in `report::metrics`; alternative sinks (counting
/// fakes in tests, other metrics stacks) plug in without touching
/// `ingest`. Decoupling rationale in
/// `docs/design/06-INGESTION-AND-DAEMON.md` § "The `MetricsSink` trait".
///
/// `Send + Sync` because the gRPC and HTTP paths share the sink across
/// tokio tasks via `Arc<dyn MetricsSink>`.
pub trait MetricsSink: Send + Sync {
    /// Record one rejected OTLP request, labeled by reason.
    fn record_otlp_reject(&self, reason: OtlpRejectReason);

    /// Record one request's span conversion tally (received vs filtered).
    fn record_otlp_spans(&self, stats: SpanConversionStats);

    /// Whether cgroup memory has crossed the configured high-water mark,
    /// so the handlers should reject ingest to bound RSS. Defaults to
    /// `false`: the guard is opt-in and only the daemon `MetricsState`
    /// wires a real signal, batch/test sinks stay unaffected.
    fn ingest_over_memory_limit(&self) -> bool {
        false
    }
}

/// Per-request span conversion tally.
///
/// `received` counts every span in the request; the `filtered_*` fields
/// count spans skipped by [`convert_span`] because they are not
/// analyzable I/O operations (one field per [`OtlpSpanFilterReason`]
/// variant). Retained spans = `received` minus the filtered sum.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpanConversionStats {
    pub received: u64,
    pub filtered_not_io: u64,
    pub filtered_missing_db_statement: u64,
    pub filtered_missing_http_url: u64,
    pub filtered_non_sql_datastore: u64,
}

impl SpanConversionStats {
    fn count_filtered(&mut self, reason: OtlpSpanFilterReason) {
        match reason {
            OtlpSpanFilterReason::NotIo => self.filtered_not_io += 1,
            OtlpSpanFilterReason::MissingDbStatement => self.filtered_missing_db_statement += 1,
            OtlpSpanFilterReason::MissingHttpUrl => self.filtered_missing_http_url += 1,
            OtlpSpanFilterReason::NonSqlDatastore => self.filtered_non_sql_datastore += 1,
        }
    }

    /// The filtered tallies keyed by their reason, the single place
    /// that zips the named fields back to the enum (consumed by the
    /// metrics sink). Kept next to [`Self::count_filtered`] so the two
    /// directions of the mapping cannot drift apart.
    #[must_use]
    pub fn filtered_counts(&self) -> [(OtlpSpanFilterReason, u64); 4] {
        [
            (OtlpSpanFilterReason::NotIo, self.filtered_not_io),
            (
                OtlpSpanFilterReason::MissingDbStatement,
                self.filtered_missing_db_statement,
            ),
            (
                OtlpSpanFilterReason::MissingHttpUrl,
                self.filtered_missing_http_url,
            ),
            (
                OtlpSpanFilterReason::NonSqlDatastore,
                self.filtered_non_sql_datastore,
            ),
        ]
    }
}

// ── Conversion helpers ──────────────────────────────────────────────

/// Convert bytes to a lowercase hex string using a lookup table.
///
/// Builds the String directly via byte append (all written bytes are
/// ASCII hex, so `unsafe { String::from_utf8_unchecked }` would be
/// sound but is avoided; we use safe `from_utf8` which optimizes
/// cleanly since the buffer is pre-validated by construction).
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

use crate::time::nanos_to_iso8601;

/// Extract the string variant of an OTLP `AnyValue`.
#[inline]
fn any_value_as_str(value: Option<&any_value::Value>) -> Option<&str> {
    match value {
        Some(any_value::Value::StringValue(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Extract the integer variant of an OTLP `AnyValue`.
#[inline]
fn any_value_as_int(value: Option<&any_value::Value>) -> Option<i64> {
    match value {
        Some(any_value::Value::IntValue(i)) => Some(*i),
        _ => None,
    }
}

/// Lookup a string attribute by key (one linear scan).
///
/// Used at the resource level (`service.name`, resource-level
/// `cloud.region`) and inside the parent walk for `source.endpoint`.
/// Spans go through the single-pass `classify_span_attrs` instead.
fn get_str_attribute<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| any_value_as_str(kv.value.as_ref().and_then(|v| v.value.as_ref())))
}

// ── code.* attribute extraction ─────────────────────────────────────

/// Maximum parent-span hops when walking for `code.*` attributes.
///
/// Java auto-instrumented stacks can chain HTTP server, Filter,
/// `DispatcherServlet`, Controller, Service, Repository, Hibernate, JDBC,
/// which is 8 levels. Real OpenTelemetry SDKs typically span fewer layers
/// (Spring AOP proxies stay invisible).
const CODE_ATTRS_MAX_DEPTH: usize = 8;

/// Hard cap on the per-resource span index used for parent lookup and scope
/// attribution. Bounds memory and avoids quadratic walks on pathological
/// payloads. Spans beyond the cap lose parent/scope attribution but are still
/// converted into events.
const MAX_SPANS_PER_RESOURCE: usize = 100_000;

/// Code-frame attributes read from a single span's attribute set.
///
/// Borrows from the span attributes, so the lifetime is tied to the
/// `resource_spans` buffer (same lifetime as the values stored in
/// `span_index`). All fields are independently optional because
/// OpenTelemetry agents do not always emit the full set.
#[derive(Default, Clone, Copy)]
struct CodeAttrs<'a> {
    function_name: Option<&'a str>,
    filepath: Option<&'a str>,
    lineno: Option<i64>,
    namespace: Option<&'a str>,
}

impl CodeAttrs<'_> {
    #[inline]
    fn has_any(&self) -> bool {
        self.function_name.is_some()
            || self.filepath.is_some()
            || self.lineno.is_some()
            || self.namespace.is_some()
    }
}

/// All span attributes consumed by `convert_span`, classified in a single
/// linear pass over the attribute list.
///
/// Stable and legacy names for the same logical field are kept distinct:
/// the namespace derivation must only consume the stable `code.function.name`
/// (the legacy `code.function` is documented as a bare function name).
#[derive(Default)]
struct ClassifiedAttrs<'a> {
    db_statement: Option<&'a str>,
    db_query_text: Option<&'a str>,
    db_system: Option<&'a str>,
    // Stable OTel 1.27+ semconv key for the DB system. db.system is the older
    // experimental spelling. The current datadogreceiver emits this one.
    db_system_name: Option<&'a str>,
    // Datadog dd-trace fallbacks (see classify_io_event for the rationale).
    dd_resource: Option<&'a str>,
    db_type: Option<&'a str>,
    http_url: Option<&'a str>,
    url_full: Option<&'a str>,
    http_method: Option<&'a str>,
    http_request_method: Option<&'a str>,
    http_status_code: Option<i64>,
    http_response_status_code: Option<i64>,
    http_response_body_size: Option<i64>,
    http_response_content_length: Option<i64>,
    cloud_region: Option<&'a str>,
    code_function_name: Option<&'a str>,
    code_function: Option<&'a str>,
    code_file_path: Option<&'a str>,
    code_filepath: Option<&'a str>,
    code_line_number: Option<i64>,
    code_lineno: Option<i64>,
    code_namespace: Option<&'a str>,
}

impl<'a> ClassifiedAttrs<'a> {
    /// Effective DB system, in precedence order: the stable `OTel`
    /// `db.system.name`, the older `db.system`, then the dd-trace `db.type`
    /// meta key passed through by the datadogreceiver. Drives both the non-SQL
    /// datastore filter and the SQL operation label. Blank values are skipped
    /// per field (lazily), so an empty or whitespace `db.system.name` does not
    /// shadow a valid `db.type`.
    fn effective_db_system(&self) -> Option<&'a str> {
        self.db_system_name
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.db_system.filter(|s| !s.trim().is_empty()))
            .or_else(|| self.db_type.filter(|s| !s.trim().is_empty()))
    }

    fn code_attrs(&self) -> CodeAttrs<'a> {
        let function_name = self.code_function_name.or(self.code_function);
        let filepath = self.code_file_path.or(self.code_filepath);
        let lineno = self.code_line_number.or(self.code_lineno);
        let namespace = self.code_namespace.or_else(|| {
            self.code_function_name.and_then(|fq| {
                // PHP `\` fallback fires only when no `.` is present: PHP
                // namespaces (`Doctrine\DBAL\Driver\Connection::query`) have no
                // dots, dot-based languages always do, and Rust `::`-only names
                // have neither, so other languages are unchanged.
                fq.rsplit_once('.')
                    .or_else(|| fq.rsplit_once('\\'))
                    .map(|(ns, _)| ns)
            })
        });
        CodeAttrs {
            function_name,
            filepath,
            lineno,
            namespace,
        }
    }
}

/// Single-pass classifier over span attributes.
///
/// Replaces ~14 separate linear scans (one per attribute lookup) with a
/// single iteration. At typical 30-attribute HTTP spans the saving is
/// ~13x fewer key comparisons per span.
fn classify_span_attrs(attrs: &[KeyValue]) -> ClassifiedAttrs<'_> {
    let mut out = ClassifiedAttrs::default();
    for kv in attrs {
        let value = kv.value.as_ref().and_then(|v| v.value.as_ref());
        match kv.key.as_str() {
            "db.statement" => out.db_statement = any_value_as_str(value),
            "db.query.text" => out.db_query_text = any_value_as_str(value),
            "db.system" => out.db_system = any_value_as_str(value),
            "db.system.name" => out.db_system_name = any_value_as_str(value),
            "dd.span.Resource" => out.dd_resource = any_value_as_str(value),
            "db.type" => out.db_type = any_value_as_str(value),
            "http.url" => out.http_url = any_value_as_str(value),
            "url.full" => out.url_full = any_value_as_str(value),
            "http.method" => out.http_method = any_value_as_str(value),
            "http.request.method" => out.http_request_method = any_value_as_str(value),
            "http.status_code" => out.http_status_code = any_value_as_int(value),
            "http.response.status_code" => out.http_response_status_code = any_value_as_int(value),
            "http.response.body.size" => out.http_response_body_size = any_value_as_int(value),
            "http.response_content_length" => {
                out.http_response_content_length = any_value_as_int(value);
            }
            "cloud.region" => out.cloud_region = any_value_as_str(value),
            "code.function.name" => out.code_function_name = any_value_as_str(value),
            "code.function" => out.code_function = any_value_as_str(value),
            "code.file.path" => out.code_file_path = any_value_as_str(value),
            "code.filepath" => out.code_filepath = any_value_as_str(value),
            "code.line.number" => out.code_line_number = any_value_as_int(value),
            "code.lineno" => out.code_lineno = any_value_as_int(value),
            "code.namespace" => out.code_namespace = any_value_as_str(value),
            _ => {}
        }
    }
    out
}

/// Single-pass `code.*` extractor for parent-span walks.
///
/// Same precedence rules as `ClassifiedAttrs::code_attrs`. We do not
/// classify the full attribute set on parents because only `code.*`
/// matters for ancestor frames.
fn read_code_attrs(attrs: &[KeyValue]) -> CodeAttrs<'_> {
    let mut function_name_stable = None;
    let mut function_name_legacy = None;
    let mut filepath_stable = None;
    let mut filepath_legacy = None;
    let mut lineno_stable = None;
    let mut lineno_legacy = None;
    let mut namespace_explicit = None;
    for kv in attrs {
        let value = kv.value.as_ref().and_then(|v| v.value.as_ref());
        match kv.key.as_str() {
            "code.function.name" => function_name_stable = any_value_as_str(value),
            "code.function" => function_name_legacy = any_value_as_str(value),
            "code.file.path" => filepath_stable = any_value_as_str(value),
            "code.filepath" => filepath_legacy = any_value_as_str(value),
            "code.line.number" => lineno_stable = any_value_as_int(value),
            "code.lineno" => lineno_legacy = any_value_as_int(value),
            "code.namespace" => namespace_explicit = any_value_as_str(value),
            _ => {}
        }
    }
    let namespace = namespace_explicit.or_else(|| {
        function_name_stable.and_then(|fq| {
            // PHP `\` fallback: see `code_attrs` for the precedence rationale.
            fq.rsplit_once('.')
                .or_else(|| fq.rsplit_once('\\'))
                .map(|(ns, _)| ns)
        })
    });
    CodeAttrs {
        function_name: function_name_stable.or(function_name_legacy),
        filepath: filepath_stable.or(filepath_legacy),
        lineno: lineno_stable.or(lineno_legacy),
        namespace,
    }
}

/// Walk parent span chain to find the nearest span carrying any code.* attribute.
///
/// Caller passes the leaf's already-extracted code attributes and the
/// leaf's `parent_span_id`. The walk only triggers when the leaf has
/// nothing, so the leaf attribute list is never re-scanned. Bounded by
/// `CODE_ATTRS_MAX_DEPTH` to prevent loops on malformed parent chains.
fn walk_parents_for_code_attrs<'a>(
    leaf: CodeAttrs<'a>,
    parent_span_id: &[u8],
    span_index: &HashMap<&[u8], &'a Span>,
) -> CodeAttrs<'a> {
    if leaf.has_any() || parent_span_id.is_empty() {
        return leaf;
    }
    let mut current_parent_id = parent_span_id;
    let mut depth = 0;
    loop {
        let Some(parent) = span_index.get(current_parent_id) else {
            return CodeAttrs::default();
        };
        let attrs = read_code_attrs(&parent.attributes);
        if attrs.has_any() {
            return attrs;
        }
        if parent.parent_span_id.is_empty() || depth >= CODE_ATTRS_MAX_DEPTH {
            return CodeAttrs::default();
        }
        current_parent_id = parent.parent_span_id.as_slice();
        depth += 1;
    }
}

// ── Main conversion function ────────────────────────────────────────

/// Convert an OTLP `ExportTraceServiceRequest` into `SpanEvent`s.
///
/// Uses a two-pass design per resource: the first pass builds a span index
/// for parent lookup (needed to resolve `source.endpoint` from parent
/// attributes), and the second pass converts I/O spans into events.
///
/// Spans without `db.statement` or `http.url` attributes are skipped.
/// Parent span lookup is done within the same request; if the parent is not
/// found, `source.endpoint` defaults to `"unknown"`.
/// Build a span index for parent lookup within a single resource
/// (capped at [`MAX_SPANS_PER_RESOURCE`] spans).
fn build_span_index(
    resource_spans: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> HashMap<&[u8], &Span> {
    let mut index: HashMap<&[u8], &Span> = HashMap::new();
    let mut count = 0usize;
    'outer: for scope_spans in &resource_spans.scope_spans {
        for span in &scope_spans.spans {
            index.insert(&span.span_id, span);
            count += 1;
            if count >= MAX_SPANS_PER_RESOURCE {
                tracing::warn!(
                    "OTLP span index capped at {} entries, parent lookup may be degraded for remaining spans",
                    MAX_SPANS_PER_RESOURCE
                );
                break 'outer;
            }
        }
    }
    index
}

/// Build a `span_id -> instrumentation scope name` index alongside the
/// span index. Same [`MAX_SPANS_PER_RESOURCE`] cap as `build_span_index`,
/// entries beyond the cap simply lose scope attribution.
fn build_scope_index(
    resource_spans: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> HashMap<&[u8], &str> {
    let mut index: HashMap<&[u8], &str> = HashMap::new();
    let mut count = 0usize;
    'outer: for scope_spans in &resource_spans.scope_spans {
        let scope_name = scope_spans.scope.as_ref().map_or("", |s| s.name.as_str());
        if scope_name.is_empty() {
            continue;
        }
        for span in &scope_spans.spans {
            index.insert(&span.span_id, scope_name);
            count += 1;
            if count >= MAX_SPANS_PER_RESOURCE {
                break 'outer;
            }
        }
    }
    index
}

/// Collect the leaf span's scope plus each unique ancestor scope, up to
/// `CODE_ATTRS_MAX_DEPTH`. Result is ordered leaf to root and
/// deduplicated. Empty when no scope is recorded for any span on the
/// chain.
fn collect_instrumentation_scopes(
    span: &Span,
    span_index: &HashMap<&[u8], &Span>,
    scope_index: &HashMap<&[u8], &str>,
) -> Vec<Arc<str>> {
    let mut out: Vec<Arc<str>> = Vec::new();
    let mut current = span;
    let mut depth = 0;
    loop {
        if let Some(name) = scope_index.get(current.span_id.as_slice())
            && !out.iter().any(|s| s.as_ref() == *name)
        {
            out.push(Arc::from(*name));
        }
        if current.parent_span_id.is_empty() || depth >= CODE_ATTRS_MAX_DEPTH {
            return out;
        }
        let Some(parent) = span_index.get(current.parent_span_id.as_slice()) else {
            return out;
        };
        current = *parent;
        depth += 1;
    }
}

#[must_use]
pub fn convert_otlp_request(request: &ExportTraceServiceRequest) -> Vec<SpanEvent> {
    convert_otlp_request_counted(request).0
}

/// [`convert_otlp_request`] with a per-request conversion tally.
///
/// The daemon listeners use this variant so the received vs filtered
/// span counters move even when a whole request converts to zero
/// events (the request itself still succeeds, by design).
#[must_use]
pub fn convert_otlp_request_counted(
    request: &ExportTraceServiceRequest,
) -> (Vec<SpanEvent>, SpanConversionStats) {
    let mut events = Vec::new();
    let mut stats = SpanConversionStats::default();

    for resource_spans in &request.resource_spans {
        // Build the per-Resource Arc<str> once, then Arc::clone into each span.
        // A resource_spans block routinely carries hundreds of spans for the
        // same service.name, so this collapses N allocations to one.
        let service_arc: Arc<str> = Arc::from(
            resource_spans
                .resource
                .as_ref()
                .and_then(|r| get_str_attribute(&r.attributes, "service.name"))
                .unwrap_or("unknown"),
        );

        // cloud.region: resource-level with span-level fallback in convert_span.
        // Invalid values silently dropped (sanitization at ingest boundary).
        let resource_cloud_region: Option<Arc<str>> = resource_spans
            .resource
            .as_ref()
            .and_then(|r| get_str_attribute(&r.attributes, "cloud.region"))
            .filter(|s| crate::score::carbon::is_valid_region_id(s))
            .map(Arc::from);

        let span_index = build_span_index(resource_spans);
        let scope_index = build_scope_index(resource_spans);

        for scope_spans in &resource_spans.scope_spans {
            for span in &scope_spans.spans {
                stats.received += 1;
                match convert_span(
                    span,
                    &service_arc,
                    resource_cloud_region.as_ref(),
                    &span_index,
                    &scope_index,
                ) {
                    Ok(event) => events.push(event),
                    Err(reason) => stats.count_filtered(reason),
                }
            }
        }
    }

    (events, stats)
}

/// Classify why a span was skipped: distinguishes "internal span" from
/// "I/O span missing the attribute that carries its statement or url".
fn span_filter_reason(
    classified: &ClassifiedAttrs<'_>,
    db_system: Option<&str>,
    kind: i32,
) -> OtlpSpanFilterReason {
    // Stable OTel semconv puts `url.full` on CLIENT spans only; SERVER
    // spans legitimately carry just `http.request.method` + `url.path`.
    // A server span without a full URL is inbound work, not a stripped
    // outbound call, so it must count as `not_io`, not as an
    // instrumentation gap.
    let server = kind == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Server as i32;
    // Any DB span (non-SQL stores were already dropped) that resolved no
    // statement is an instrumentation gap. Keyed on presence, not on the SQL
    // allowlist, so a statement-less span on a SQL engine outside the allowlist
    // (Snowflake, Trino, ...) is still reported instead of silently masked.
    // db_system is the canonicalized effective system.
    if db_system.is_some() {
        OtlpSpanFilterReason::MissingDbStatement
    } else if !server
        && classified
            .http_method
            .or(classified.http_request_method)
            .is_some()
    {
        OtlpSpanFilterReason::MissingHttpUrl
    } else {
        OtlpSpanFilterReason::NotIo
    }
}

/// Classify an analyzable span as SQL or outbound HTTP, returning
/// `(event_type, target, operation)`. `None` when it carries neither a
/// statement nor a URL. Supports both legacy (pre-1.21) and stable (1.21+)
/// `OTel` semantic conventions.
fn classify_io_event(
    c: &ClassifiedAttrs<'_>,
    db_system: Option<&str>,
) -> Option<(EventType, String, String)> {
    // OTel db.statement/db.query.text first, then the dd-trace fallback: the
    // datadogreceiver never sets db.statement and leaves the (obfuscated) SQL
    // in dd.span.Resource. That attribute is present on every dd-trace span,
    // HTTP routes included, so trust it as SQL only when the engine is a
    // recognized SQL system, the resource is non-blank, and the span carries no
    // HTTP signal (legacy or stable). Fail closed: an HTTP route or a
    // non-SQL/unknown system is never fed to the SQL tokenizer. The resource is
    // trimmed so stray collector whitespace does not fragment N+1 groups.
    // db_system is the canonicalized effective system.
    let has_http = c.http_url.is_some()
        || c.url_full.is_some()
        || c.http_method.is_some()
        || c.http_request_method.is_some();
    let statement = c.db_statement.or(c.db_query_text).or_else(|| {
        c.dd_resource
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|_| !has_http && db_system.is_some_and(crate::ingest::is_sql_db_system))
    });
    if let Some(statement) = statement {
        // db_system (e.g. "postgresql") is the engine, not the SQL verb. The
        // verb is extracted from target by energy_coefficient() when scoring.
        let op = db_system.unwrap_or("sql").to_string();
        Some((EventType::Sql, statement.to_string(), op))
    } else if let Some(url) = c.http_url.or(c.url_full) {
        let method = c
            .http_method
            .or(c.http_request_method)
            .unwrap_or("GET")
            .to_string();
        Some((EventType::HttpOut, url.to_string(), method))
    } else {
        None
    }
}

/// Convert a single OTLP span to a `SpanEvent`, if it is an I/O operation.
///
/// Non-I/O spans return the filter reason so the caller can tally them.
fn convert_span(
    span: &Span,
    service_arc: &Arc<str>,
    resource_cloud_region: Option<&Arc<str>>,
    span_index: &HashMap<&[u8], &Span>,
    scope_index: &HashMap<&[u8], &str>,
) -> Result<SpanEvent, OtlpSpanFilterReason> {
    let classified = classify_span_attrs(&span.attributes);
    // Canonical effective DB system, computed once and threaded through the
    // non-SQL drop, SQL classification, and gap-reason paths.
    let db_system = classified
        .effective_db_system()
        .map(crate::ingest::canonical_db_system);

    // Non-SQL datastore (Redis, MongoDB, ...): dropped, not modeled. Gated on
    // the canonical effective system so a statement-less or url-bearing span is
    // also dropped, and never mistaken for an instrumentation gap.
    if db_system.is_some_and(crate::ingest::is_non_sql_db_system) {
        return Err(OtlpSpanFilterReason::NonSqlDatastore);
    }

    let Some((event_type, target, operation)) = classify_io_event(&classified, db_system) else {
        return Err(span_filter_reason(&classified, db_system, span.kind));
    };

    let start_nanos = span.start_time_unix_nano;
    let end_nanos = span.end_time_unix_nano;
    let timestamp = nanos_to_iso8601(start_nanos);
    if end_nanos < start_nanos {
        tracing::trace!("Span has end_time < start_time (clock skew?), duration forced to 0");
    }
    let duration_us = end_nanos.saturating_sub(start_nanos) / 1000;

    let trace_id = bytes_to_hex(&span.trace_id);
    let span_id = bytes_to_hex(&span.span_id);

    // Status code (HTTP only, supports both legacy and stable conventions)
    let status_code = if event_type == EventType::HttpOut {
        classified
            .http_status_code
            .or(classified.http_response_status_code)
            .and_then(|c| u16::try_from(c).ok())
    } else {
        None
    };

    // Response body size (HTTP only, for carbon scoring payload tiers).
    let response_size_bytes = if event_type == EventType::HttpOut {
        classified
            .http_response_body_size
            .or(classified.http_response_content_length)
            .and_then(|v| u64::try_from(v).ok())
    } else {
        None
    };

    // Parent span lookup for source endpoint/method (single-level only,
    // independent from the code.* parent walk below).
    let (source_endpoint, source_method) = if span.parent_span_id.is_empty() {
        ("unknown".to_string(), span.name.clone())
    } else if let Some(parent) = span_index.get(span.parent_span_id.as_slice()) {
        let endpoint = get_str_attribute(&parent.attributes, "http.route")
            .or_else(|| get_str_attribute(&parent.attributes, "http.url"))
            .or_else(|| get_str_attribute(&parent.attributes, "url.full"))
            .unwrap_or("unknown")
            .to_string();
        let method = get_str_attribute(&parent.attributes, "code.function")
            .map_or_else(|| parent.name.clone(), ToString::to_string);
        (endpoint, method)
    } else {
        ("unknown".to_string(), span.name.clone())
    };

    let parent_span_id = if span.parent_span_id.is_empty() {
        None
    } else {
        Some(bytes_to_hex(&span.parent_span_id))
    };

    // cloud.region: resource → span fallback → None. The resource-level
    // Arc is shared across all spans of this resource_spans block via
    // Arc::clone; only the span-level fallback path allocates.
    let cloud_region: Option<Arc<str>> = resource_cloud_region.cloned().or_else(|| {
        classified
            .cloud_region
            .filter(|s| crate::score::carbon::is_valid_region_id(s))
            .map(Arc::from)
    });

    // code.* attributes: leaf attrs first, walk parents only when empty.
    // OTel JDBC and HTTP-client spans rarely carry their own code.*; the
    // user frame sits on a parent.
    let code =
        walk_parents_for_code_attrs(classified.code_attrs(), &span.parent_span_id, span_index);
    let code_function: Option<Arc<str>> = code.function_name.map(Arc::from);
    let code_filepath: Option<Arc<str>> = code.filepath.map(Arc::from);
    let code_lineno = code.lineno.and_then(|v| u32::try_from(v).ok());
    let code_namespace: Option<Arc<str>> = code.namespace.map(Arc::from);

    let instrumentation_scopes = collect_instrumentation_scopes(span, span_index, scope_index);

    let mut event = SpanEvent {
        timestamp,
        trace_id,
        span_id,
        parent_span_id,
        service: Arc::clone(service_arc),
        cloud_region,
        event_type,
        operation,
        target,
        duration_us,
        source: EventSource {
            endpoint: source_endpoint,
            method: source_method,
        },
        status_code,
        response_size_bytes,
        code_function,
        code_filepath,
        code_lineno,
        code_namespace,
        instrumentation_scopes,
    };
    crate::event::sanitize_span_event(&mut event);
    Ok(event)
}

// ── gRPC service implementation ─────────────────────────────────────

/// Bounded wait when enqueueing a converted batch on the ingest channel.
/// Short bursts absorb silently; sustained saturation surfaces as a fast
/// retryable rejection that moves the `channel_full` counter. A plain
/// `send().await` only errors on a closed channel, so saturation would
/// otherwise park senders until the router request timeout with no
/// rejection ever counted.
const INGEST_ENQUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// OTLP gRPC trace service that converts spans and sends them through a channel.
pub struct OtlpGrpcService {
    sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
    metrics: Option<Arc<dyn MetricsSink>>,
}

impl OtlpGrpcService {
    #[must_use]
    pub fn new(
        sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
        metrics: Option<Arc<dyn MetricsSink>>,
    ) -> Self {
        Self { sender, metrics }
    }
}

#[async_trait]
impl opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService
    for OtlpGrpcService
{
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        // Memory-pressure admission control: reject before conversion so a
        // saturation flood cannot grow RSS past the cgroup limit. UNAVAILABLE
        // is the retryable status compliant exporters back off on.
        if let Some(m) = self.metrics.as_ref()
            && m.ingest_over_memory_limit()
        {
            m.record_otlp_reject(OtlpRejectReason::MemoryPressure);
            return Err(Status::unavailable(
                "ingest paused: memory high-water, retry",
            ));
        }
        let (events, stats) = convert_otlp_request_counted(request.get_ref());
        if let Some(m) = self.metrics.as_ref() {
            m.record_otlp_spans(stats);
        }
        if !events.is_empty()
            && let Err(e) = self
                .sender
                .send_timeout(events, INGEST_ENQUEUE_TIMEOUT)
                .await
        {
            if let Some(m) = self.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::ChannelFull);
            }
            // Saturation must map to a status the OTLP spec lists as
            // retryable (UNAVAILABLE); INTERNAL is non-retryable and
            // would make compliant exporters drop the batch for good.
            // A closed channel means shutdown: INTERNAL is accurate.
            return Err(match e {
                tokio::sync::mpsc::error::SendTimeoutError::Timeout(_) => {
                    Status::unavailable("ingest queue full, retry")
                }
                tokio::sync::mpsc::error::SendTimeoutError::Closed(_) => {
                    Status::internal("event channel closed")
                }
            });
        }
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

// ── HTTP handler (axum) ─────────────────────────────────────────────

/// State shared by the OTLP HTTP handler.
///
/// Cloned on every request by axum's `State` extractor; the sender and
/// metrics handle are both cheap to clone (mpsc Sender is an Arc, the
/// metrics Option carries an Arc).
#[derive(Clone)]
struct OtlpHttpState {
    sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
    metrics: Option<Arc<dyn MetricsSink>>,
}

/// Build an axum router for OTLP HTTP ingestion.
///
/// Accepts `POST /v1/traces` with protobuf-encoded `ExportTraceServiceRequest`.
/// `metrics` is `Some` in daemon mode so the handler can increment
/// `perf_sentinel_otlp_rejected_total` at every rejection site, and
/// `None` in batch / test contexts where no Prometheus registry exists.
pub fn otlp_http_router(
    sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
    max_payload_size: usize,
    metrics: Option<Arc<dyn MetricsSink>>,
) -> axum::Router {
    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode, header},
        routing::post,
    };

    async fn handle_traces(
        State(state): State<OtlpHttpState>,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> StatusCode {
        // Memory-pressure admission control: reject before decode so a
        // saturation flood cannot grow RSS past the cgroup limit. 503 is
        // the retryable status compliant exporters back off on.
        if let Some(m) = state.metrics.as_ref()
            && m.ingest_over_memory_limit()
        {
            m.record_otlp_reject(OtlpRejectReason::MemoryPressure);
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        // OTLP/HTTP spec: only `application/x-protobuf` is accepted by
        // perf-sentinel (we do not implement the JSON-encoded variant).
        // Reject upfront so we do not waste CPU running `prost::decode`
        // on obviously mistyped requests (curl without a Content-Type,
        // JSON clients misconfigured at the OTel Collector, etc.).
        let content_type_ok = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| {
                // Match `application/x-protobuf` with optional parameters
                // like `; charset=...`. Exact-match or prefix-with-semicolon.
                let base = ct.split(';').next().unwrap_or("").trim();
                base.eq_ignore_ascii_case("application/x-protobuf")
            });
        if !content_type_ok {
            if let Some(m) = state.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::UnsupportedMediaType);
            }
            return StatusCode::UNSUPPORTED_MEDIA_TYPE;
        }
        let Ok(request) = <ExportTraceServiceRequest as prost::Message>::decode(body.as_ref())
        else {
            if let Some(m) = state.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::ParseError);
            }
            return StatusCode::BAD_REQUEST;
        };
        let (events, stats) = convert_otlp_request_counted(&request);
        if let Some(m) = state.metrics.as_ref() {
            m.record_otlp_spans(stats);
        }
        if !events.is_empty()
            && state
                .sender
                .send_timeout(events, INGEST_ENQUEUE_TIMEOUT)
                .await
                .is_err()
        {
            tracing::warn!("OTLP HTTP: event channel full or closed, dropping events");
            if let Some(m) = state.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::ChannelFull);
            }
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        StatusCode::OK
    }

    // Hard cap on concurrently processed OTLP HTTP requests, bounding
    // decode CPU and buffered-body memory under a saturation flood:
    // without it the kubelet liveness probe on /health starves behind
    // decode work and restarts the daemon before shedding gets a chance
    // (observed at ~800 traces/s on a 500m-CPU pod). Excess requests
    // wait on this in-process semaphore, bounded by the router-level
    // request timeout, which is the backpressure OTLP senders expect.
    // Scoped to this route so /health and the query API stay responsive.
    const MAX_CONCURRENT_OTLP_HTTP: usize = 32;

    let state = OtlpHttpState { sender, metrics };
    let router = Router::new()
        .route("/v1/traces", post(handle_traces))
        .route_layer(tower::limit::GlobalConcurrencyLimitLayer::new(
            MAX_CONCURRENT_OTLP_HTTP,
        ))
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(max_payload_size));

    // Layer order, request flow on the way in: RequestBodyLimit (compressed
    // wire bytes) → RequestDecompression (gzip stream) → DefaultBodyLimit
    // (decompressed bytes via the `Bytes` extractor) → handler. The
    // outer compressed cap bounds attacker decompression CPU even when
    // operators raise `max_payload_size`. tower-http does streaming
    // decompression with backpressure, so it cannot pre-allocate above
    // what `Bytes` will accept.
    #[cfg(feature = "daemon")]
    let router = router
        .layer(tower_http::decompression::RequestDecompressionLayer::new())
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            max_payload_size,
        ));

    router
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
