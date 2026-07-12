//! OTLP ingestion: maps OpenTelemetry spans to `SpanEvent`.
//!
//! Supports both gRPC (tonic `TraceService`) and HTTP (axum handler) ingestion.
//! Uses the `opentelemetry-proto` crate for protobuf definitions.

use std::collections::{HashMap, HashSet};
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
    pub filtered_merged_db_span: u64,
}

impl SpanConversionStats {
    fn count_filtered(&mut self, reason: OtlpSpanFilterReason) {
        match reason {
            OtlpSpanFilterReason::NotIo => self.filtered_not_io += 1,
            OtlpSpanFilterReason::MissingDbStatement => self.filtered_missing_db_statement += 1,
            OtlpSpanFilterReason::MissingHttpUrl => self.filtered_missing_http_url += 1,
            OtlpSpanFilterReason::NonSqlDatastore => self.filtered_non_sql_datastore += 1,
            OtlpSpanFilterReason::MergedDbSpan => self.filtered_merged_db_span += 1,
        }
    }

    /// The filtered tallies keyed by their reason, the single place
    /// that zips the named fields back to the enum (consumed by the
    /// metrics sink). Kept next to [`Self::count_filtered`] so the two
    /// directions of the mapping cannot drift apart.
    #[must_use]
    pub fn filtered_counts(&self) -> [(OtlpSpanFilterReason, u64); 5] {
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
            (
                OtlpSpanFilterReason::MergedDbSpan,
                self.filtered_merged_db_span,
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
///
/// `Clone` is a plain memcpy of `Option<&str>`/`Option<i64>` fields, used
/// to reuse the per-resource classification cache in `convert_span`.
#[derive(Clone, Default)]
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
    // RPC semconv (gRPC, Dubbo, ...): no statement or URL, so these are the
    // only keys that identify the callee. See classify_io_event.
    rpc_system: Option<&'a str>,
    rpc_service: Option<&'a str>,
    rpc_method: Option<&'a str>,
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
            "rpc.system" => out.rpc_system = any_value_as_str(value),
            "rpc.service" => out.rpc_service = any_value_as_str(value),
            "rpc.method" => out.rpc_method = any_value_as_str(value),
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

/// Whether the span carries any HTTP signal (legacy or stable semconv).
/// Gates both the dd-trace statement fallback and the stitch orphan
/// classification: a span with HTTP keys is never treated as pure SQL.
fn has_http_signal(c: &ClassifiedAttrs<'_>) -> bool {
    c.http_url.is_some()
        || c.url_full.is_some()
        || c.http_method.is_some()
        || c.http_request_method.is_some()
}

/// Resolve the SQL statement a span carries: legacy `db.statement`, stable
/// `db.query.text`, then the dd-trace `dd.span.Resource` fallback (see
/// `classify_io_event` for the fail-closed gating rationale). Shared by
/// `classify_io_event` and the stitch pre-pass so the two can never
/// disagree on what counts as a statement.
fn resolve_sql_statement<'a>(c: &ClassifiedAttrs<'a>, db_system: Option<&str>) -> Option<&'a str> {
    c.db_statement.or(c.db_query_text).or_else(|| {
        c.dd_resource
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|_| {
                !has_http_signal(c) && db_system.is_some_and(crate::ingest::is_sql_db_system)
            })
    })
}

// ── Split DB span stitching ─────────────────────────────────────────

/// `(trace_id, span_id)` key of the stitch pre-pass maps.
type SpanKey<'a> = (&'a [u8], &'a [u8]);

fn span_key(span: &Span) -> SpanKey<'_> {
    (span.trace_id.as_slice(), span.span_id.as_slice())
}

/// Outcome of the stitch pre-pass for one span, keyed by [`SpanKey`].
enum StitchDecision<'a> {
    /// Span merged into another span's event: skip it, counted as
    /// `merged_db_span`.
    Suppress,
    /// Statement-less duration span adopting this statement from a
    /// related donor span.
    Adopt(&'a str),
}

/// A statement-bearing SQL span usable as a statement source.
struct StitchDonor<'a> {
    span: &'a Span,
    statement: &'a str,
}

/// Bounded look-back for an unconsumed sibling donor, so batch-prepared
/// statements pair off while crafted payloads stay linear.
const SIBLING_DONOR_LOOKBACK: usize = 8;

/// Visit the same-trace ancestors of `span`, nearest first, up to
/// `CODE_ATTRS_MAX_DEPTH` hops; stop early when `visit` returns `true`.
/// A malformed parent cycle that loops back to `span` itself ends the
/// walk, so a span is never its own ancestor.
fn walk_same_trace_ancestors<'a>(
    span: &'a Span,
    span_index: &HashMap<&'a [u8], &'a Span>,
    mut visit: impl FnMut(&'a Span) -> bool,
) {
    let mut current = span;
    for _ in 0..CODE_ATTRS_MAX_DEPTH {
        if current.parent_span_id.is_empty() {
            return;
        }
        let Some(&parent) = span_index.get(current.parent_span_id.as_slice()) else {
            return;
        };
        if parent.trace_id != span.trace_id || parent.span_id == span.span_id {
            return;
        }
        if visit(parent) {
            return;
        }
        current = parent;
    }
}

/// Duration halves of a split query are execute/query spans; statement-less
/// connect, commit, or transaction spans must keep today's filtering
/// instead of adopting a neighbor query's statement.
fn looks_like_query_execution(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("execute") || name.contains("query")
}

/// One [`ClassifiedAttrs`] per span in iteration order, capped at
/// [`MAX_SPANS_PER_RESOURCE`] (spans beyond the cap classify inline at
/// conversion). Shared by the stitch pre-pass and `convert_span` so each
/// attribute list is scanned once per request.
fn classify_resource_spans(
    resource_spans: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> Vec<ClassifiedAttrs<'_>> {
    let total: usize = resource_spans
        .scope_spans
        .iter()
        .map(|s| s.spans.len())
        .sum();
    let mut out = Vec::with_capacity(total.min(MAX_SPANS_PER_RESOURCE));
    'outer: for scope_spans in &resource_spans.scope_spans {
        for span in &scope_spans.spans {
            if out.len() >= MAX_SPANS_PER_RESOURCE {
                break 'outer;
            }
            out.push(classify_span_attrs(&span.attributes));
        }
    }
    out
}

/// Classify the resource's SQL spans into donors (statement-bearing) and
/// orphans (allowlisted SQL `db.*` system, execute/query span name, no
/// statement and no HTTP/RPC signal). Spans with empty ids are excluded
/// (their [`SpanKey`]s would collide). `classified` is the capped
/// per-resource cache: spans beyond it never participate and convert
/// exactly as before.
fn collect_stitch_participants<'a>(
    resource_spans: &'a opentelemetry_proto::tonic::trace::v1::ResourceSpans,
    classified: &[ClassifiedAttrs<'a>],
) -> (Vec<StitchDonor<'a>>, Vec<&'a Span>) {
    let mut donors = Vec::new();
    let mut orphans = Vec::new();
    let mut idx = 0usize;
    'outer: for scope_spans in &resource_spans.scope_spans {
        for span in &scope_spans.spans {
            let Some(c) = classified.get(idx) else {
                break 'outer;
            };
            idx += 1;
            if span.trace_id.is_empty() || span.span_id.is_empty() {
                continue;
            }
            let db_system = c
                .effective_db_system()
                .map(crate::ingest::canonical_db_system);
            if !db_system.is_some_and(crate::ingest::is_sql_db_system) {
                continue;
            }
            if let Some(statement) = resolve_sql_statement(c, db_system) {
                donors.push(StitchDonor { span, statement });
            } else if !has_http_signal(c)
                && c.rpc_system.is_none()
                && looks_like_query_execution(&span.name)
            {
                orphans.push(span);
            }
        }
    }
    (donors, orphans)
}

/// Rule 1: collapse layered duplicate statements onto the outermost span.
/// Mutual ancestry (a malformed parent cycle, which has no outermost span)
/// suppresses neither donor, keeping pre-stitch behavior.
fn suppress_layered_duplicates<'a>(
    donors: &[StitchDonor<'a>],
    donor_by_id: &HashMap<SpanKey<'a>, usize>,
    span_index: &HashMap<&'a [u8], &'a Span>,
) -> Vec<bool> {
    let mut suppressed = vec![false; donors.len()];
    for (i, donor) in donors.iter().enumerate() {
        let mut suppressor = None;
        walk_same_trace_ancestors(donor.span, span_index, |ancestor| {
            suppressor = donor_by_id
                .get(&span_key(ancestor))
                .copied()
                .filter(|&j| donors[j].statement == donor.statement);
            suppressor.is_some()
        });
        if let Some(j) = suppressor {
            let mut mutual = false;
            walk_same_trace_ancestors(donors[j].span, span_index, |ancestor| {
                mutual = ancestor.span_id == donor.span.span_id;
                mutual
            });
            suppressed[i] = !mutual;
        }
    }
    suppressed
}

/// Rule 2 split: a layered orphan defers to its outermost same-trace orphan
/// ancestor (returned as `deferred` with the carrier's key); the rest carry
/// their own stitched event.
fn split_layered_orphans<'a>(
    orphans: &[&'a Span],
    span_index: &HashMap<&'a [u8], &'a Span>,
) -> (Vec<(&'a Span, SpanKey<'a>)>, Vec<&'a Span>) {
    let orphan_keys: HashSet<SpanKey<'a>> = orphans.iter().map(|o| span_key(o)).collect();
    let mut deferred = Vec::new();
    let mut carriers = Vec::new();
    for &orphan in orphans {
        let mut carrier_key = None;
        walk_same_trace_ancestors(orphan, span_index, |ancestor| {
            if orphan_keys.contains(&span_key(ancestor)) {
                carrier_key = Some(span_key(ancestor));
            }
            false
        });
        match carrier_key {
            Some(key) => deferred.push((orphan, key)),
            None => carriers.push(orphan),
        }
    }
    (deferred, carriers)
}

/// Sibling donor candidates for one carrier: the latest preceding donor
/// (span-order first on ties), plus the nearest unconsumed one within a
/// bounded look-back. Following siblings are never candidates (a prepare
/// span starts before its execute).
fn push_sibling_candidates(
    donors: &[StitchDonor<'_>],
    consumed: &[bool],
    siblings: &[usize],
    orphan_start: u64,
    candidates: &mut Vec<usize>,
) {
    let at_or_before =
        siblings.partition_point(|&i| donors[i].span.start_time_unix_nano <= orphan_start);
    if at_or_before == 0 {
        return;
    }
    let best_start = donors[siblings[at_or_before - 1]].span.start_time_unix_nano;
    let run_start = siblings[..at_or_before]
        .partition_point(|&i| donors[i].span.start_time_unix_nano < best_start);
    candidates.push(siblings[run_start]);
    for &i in siblings[..at_or_before]
        .iter()
        .rev()
        .take(SIBLING_DONOR_LOOKBACK)
    {
        if !consumed[i] {
            candidates.push(i);
            return;
        }
    }
}

/// Nearest related donor for an orphan starting at `orphan_start`, among
/// preceding donors (a prepare span starts before its execute), preferring
/// unconsumed ones so batch-prepared statements pair off instead of piling
/// onto the latest prepare. Fallback: smallest absolute gap, which by
/// construction only descendant donors can win (following siblings are
/// never candidates). Ties resolve to the first candidate, so pairing is
/// deterministic.
fn nearest_donor(
    donors: &[StitchDonor<'_>],
    consumed: &[bool],
    candidates: &[usize],
    orphan_start: u64,
) -> Option<usize> {
    let mut best: Option<(usize, u64, bool)> = None;
    for &i in candidates {
        let start = donors[i].span.start_time_unix_nano;
        if start > orphan_start {
            continue;
        }
        let free = !consumed[i];
        let better = match best {
            None => true,
            Some((_, b_start, b_free)) => (free && !b_free) || (free == b_free && start > b_start),
        };
        if better {
            best = Some((i, start, free));
        }
    }
    best.map(|(i, _, _)| i).or_else(|| {
        candidates
            .iter()
            .copied()
            .min_by_key(|&i| donors[i].span.start_time_unix_nano.abs_diff(orphan_start))
    })
}

/// Stitch SQL queries that layered instrumentation split across spans.
///
/// The PHP `OTel` contrib packages (Doctrine + PDO) emit, per query, spans
/// carrying the real duration but no `db.statement` (`Doctrine::execute`,
/// `PDOStatement::execute`) alongside ~0 ms spans carrying the statement
/// (the prepare spans), the latter duplicated once per layer. Without
/// stitching the duration spans drop as `missing_db_statement`, so every
/// SQL event lasts ~0 ms (slow detection can never fire) and the duplicate
/// statement spans fake redundancy.
///
/// Three rules over the SQL spans of one resource (per `ResourceSpans`
/// block; allowlisted SQL engines only, other engines keep today's
/// behavior):
/// 1. A donor (statement-bearing span) whose same-trace ancestor is a donor
///    with the identical statement is suppressed: layered duplicate.
///    Siblings are never collapsed (single-layer emitters like Laravel/PDO
///    legitimately emit prepare and execute as siblings), and mutual
///    ancestry (a malformed parent cycle, which has no outermost span)
///    suppresses neither.
/// 2. An orphan whose same-trace ancestor is an orphan defers to the
///    outermost one: only the outermost carries the stitched event.
/// 3. Each remaining orphan adopts the statement of the nearest related
///    donor (sibling or ancestor/descendant, same trace, see
///    [`nearest_donor`]). Donors are reusable (prepare once, execute N
///    times yields N events); a donor consumed at least once is suppressed.
///
/// Fail-open: an orphan with no related preceding donor (for example a
/// prepare/execute pair split across collector batches) gets no decision
/// and still counts `missing_db_statement`.
///
/// Known limit: pairing is a nearest-start heuristic. Interleaved
/// same-parent queries can swap params and durations between events, and
/// batch-prepared statements can attribute an execution to the wrong
/// template; nothing is dropped or double-emitted. Real emitters are
/// per-query sequential.
fn compute_stitch_decisions<'a>(
    resource_spans: &'a opentelemetry_proto::tonic::trace::v1::ResourceSpans,
    span_index: &HashMap<&'a [u8], &'a Span>,
    classified: &[ClassifiedAttrs<'a>],
) -> HashMap<SpanKey<'a>, StitchDecision<'a>> {
    let (donors, orphans) = collect_stitch_participants(resource_spans, classified);
    if donors.is_empty() {
        return HashMap::new();
    }

    let donor_by_id: HashMap<SpanKey<'a>, usize> = donors
        .iter()
        .enumerate()
        .map(|(i, d)| (span_key(d.span), i))
        .collect();
    let donor_suppressed = suppress_layered_duplicates(&donors, &donor_by_id, span_index);

    // Rule 2: deferred spans are only suppressed if the carrier actually
    // stitches, so the no-donor case stays byte-identical to today.
    let (deferred, carriers) = split_layered_orphans(&orphans, span_index);

    let mut decisions: HashMap<SpanKey<'a>, StitchDecision<'a>> = HashMap::new();
    let mut donor_consumed = vec![false; donors.len()];
    let mut stitched: HashSet<SpanKey<'a>> = HashSet::new();

    if !carriers.is_empty() {
        // Bucket surviving donors by parent (sibling lookup) and by ancestor
        // (descendant lookup): O(n) buckets, no quadratic sibling scans.
        let mut donors_by_parent: HashMap<SpanKey<'a>, Vec<usize>> = HashMap::new();
        let mut donors_by_ancestor: HashMap<SpanKey<'a>, Vec<usize>> = HashMap::new();
        for (i, donor) in donors.iter().enumerate() {
            if donor_suppressed[i] {
                continue;
            }
            if !donor.span.parent_span_id.is_empty() {
                donors_by_parent
                    .entry((
                        donor.span.trace_id.as_slice(),
                        donor.span.parent_span_id.as_slice(),
                    ))
                    .or_default()
                    .push(i);
            }
            walk_same_trace_ancestors(donor.span, span_index, |ancestor| {
                donors_by_ancestor
                    .entry(span_key(ancestor))
                    .or_default()
                    .push(i);
                false
            });
        }
        // Sorted by (start, span order): carriers binary-search their
        // relevant siblings, a full bucket scan would be quadratic.
        for bucket in donors_by_parent.values_mut() {
            bucket.sort_unstable_by_key(|&i| (donors[i].span.start_time_unix_nano, i));
        }

        // Rule 3: each carrier adopts the nearest related donor's statement.
        let mut candidates: Vec<usize> = Vec::new();
        for orphan in carriers {
            candidates.clear();
            let orphan_start = orphan.start_time_unix_nano;
            if !orphan.parent_span_id.is_empty()
                && let Some(siblings) = donors_by_parent
                    .get(&(orphan.trace_id.as_slice(), orphan.parent_span_id.as_slice()))
            {
                push_sibling_candidates(
                    &donors,
                    &donor_consumed,
                    siblings,
                    orphan_start,
                    &mut candidates,
                );
            }
            walk_same_trace_ancestors(orphan, span_index, |ancestor| {
                if let Some(&i) = donor_by_id.get(&span_key(ancestor))
                    && !donor_suppressed[i]
                {
                    candidates.push(i);
                }
                false
            });
            if let Some(descendants) = donors_by_ancestor.get(&span_key(orphan)) {
                candidates.extend(descendants.iter().copied());
            }
            if let Some(i) = nearest_donor(&donors, &donor_consumed, &candidates, orphan_start) {
                decisions.insert(span_key(orphan), StitchDecision::Adopt(donors[i].statement));
                donor_consumed[i] = true;
                stitched.insert(span_key(orphan));
            }
        }
    }

    for (i, donor) in donors.iter().enumerate() {
        if donor_suppressed[i] || donor_consumed[i] {
            decisions.insert(span_key(donor.span), StitchDecision::Suppress);
        }
    }
    for (span, carrier_key) in deferred {
        if stitched.contains(&carrier_key) {
            decisions.insert(span_key(span), StitchDecision::Suppress);
        }
    }
    decisions
}

/// Convert an OTLP `ExportTraceServiceRequest` into `SpanEvent`s.
///
/// Per resource: a first pass builds a span index for parent lookup (needed
/// to resolve `source.endpoint` from parent attributes), a stitch pre-pass
/// re-joins SQL queries that layered instrumentation split across spans
/// (see [`compute_stitch_decisions`]), and the final pass converts I/O
/// spans into events.
///
/// Spans that resolve none of a statement (legacy `db.statement`, stable
/// `db.query.text`, or the dd-trace `dd.span.Resource` fallback), an
/// outbound URL (legacy `http.url`, stable `url.full`), or an RPC callee
/// (`rpc.system` with `rpc.service`/`rpc.method` or the span name) are
/// skipped; see `classify_io_event`. Parent span lookup is done within the
/// same request; if the parent is not found, `source.endpoint` defaults to
/// `"unknown"`.
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
        let classified = classify_resource_spans(resource_spans);
        let stitch = compute_stitch_decisions(resource_spans, &span_index, &classified);

        let mut span_idx = 0usize;
        for scope_spans in &resource_spans.scope_spans {
            for span in &scope_spans.spans {
                stats.received += 1;
                let cached_attrs = classified.get(span_idx);
                span_idx += 1;
                let stitched_statement = match stitch.get(&span_key(span)) {
                    Some(StitchDecision::Suppress) => {
                        stats.count_filtered(OtlpSpanFilterReason::MergedDbSpan);
                        continue;
                    }
                    Some(StitchDecision::Adopt(statement)) => Some(*statement),
                    None => None,
                };
                match convert_span(
                    span,
                    &service_arc,
                    resource_cloud_region.as_ref(),
                    &span_index,
                    &scope_index,
                    stitched_statement,
                    cached_attrs,
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
/// `(event_type, target, operation)`. `None` when it carries no statement,
/// no URL, and no RPC client method. `kind` is the OTLP `SpanKind`, used to
/// admit only CLIENT-side RPC spans. Supports both legacy (pre-1.21) and
/// stable (1.21+) `OTel` semantic conventions.
fn classify_io_event(
    c: &ClassifiedAttrs<'_>,
    db_system: Option<&str>,
    span_name: &str,
    kind: i32,
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
    if let Some(statement) = resolve_sql_statement(c, db_system) {
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
    } else if let Some(system) = c.rpc_system {
        // RPC (gRPC, Dubbo, ...): no statement or URL, but rpc.service +
        // rpc.method identify the callee. Only the CLIENT span is the
        // outbound call: rpc.* is set on the inbound SERVER handler span too
        // (OTel semconv), and admitting those would double-count every hop
        // and invent self-directed edges in the topology detectors. Modeled
        // as EventType::HttpOut so the topology + occurrence detectors see it
        // and it reuses the HTTP normalize/sanitize path. Target is
        // "service/method", falling back to the span name (the gRPC
        // "package.Service/Method" convention) when either key is absent or
        // blank.
        let is_client =
            kind == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Client as i32;
        if !is_client {
            return None;
        }
        let svc = c.rpc_service.filter(|s| !s.is_empty());
        let method = c.rpc_method.filter(|s| !s.is_empty());
        let target = match (svc, method) {
            (Some(svc), Some(method)) => format!("{svc}/{method}"),
            _ => span_name.to_string(),
        };
        if target.is_empty() {
            return None;
        }
        Some((EventType::HttpOut, target, system.to_string()))
    } else {
        None
    }
}

/// Convert a single OTLP span to a `SpanEvent`, if it is an I/O operation.
///
/// Non-I/O spans return the filter reason so the caller can tally them.
fn convert_span<'a>(
    span: &'a Span,
    service_arc: &Arc<str>,
    resource_cloud_region: Option<&Arc<str>>,
    span_index: &HashMap<&[u8], &Span>,
    scope_index: &HashMap<&[u8], &str>,
    stitched_statement: Option<&'a str>,
    cached_attrs: Option<&ClassifiedAttrs<'a>>,
) -> Result<SpanEvent, OtlpSpanFilterReason> {
    // Cache miss only for spans beyond MAX_SPANS_PER_RESOURCE.
    let mut classified = cached_attrs
        .cloned()
        .unwrap_or_else(|| classify_span_attrs(&span.attributes));
    // Statement adopted from a donor span (`compute_stitch_decisions`);
    // injected here so the whole SQL tail runs unchanged.
    if stitched_statement.is_some() {
        classified.db_statement = stitched_statement;
    }
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

    let Some((event_type, target, operation)) =
        classify_io_event(&classified, db_system, &span.name, span.kind)
    else {
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
        // Memory-pressure admission control, handler-level belt: the
        // daemon wraps this service in a tonic interceptor that rejects
        // before the message is even decoded (see
        // `daemon::listeners::spawn_grpc_listener`), so this branch only
        // fires for direct callers (unit tests, embedders). UNAVAILABLE
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

    // True if the Content-Type is (optionally parameterized) protobuf, e.g.
    // `application/x-protobuf` or `application/x-protobuf; charset=...`.
    fn is_protobuf_content_type(headers: &HeaderMap) -> bool {
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| {
                ct.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/x-protobuf")
            })
    }

    async fn handle_traces(
        State(state): State<OtlpHttpState>,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> StatusCode {
        // Memory-pressure admission control, handler-level belt: the
        // outermost `memory_pressure_guard` middleware already rejects
        // before the body is buffered or decompressed, so this branch
        // only fires for direct handler callers (unit tests, embedders
        // that skip the router layers). 503 is the retryable status
        // compliant exporters back off on.
        if let Some(m) = state.metrics.as_ref()
            && m.ingest_over_memory_limit()
        {
            m.record_otlp_reject(OtlpRejectReason::MemoryPressure);
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        // Record a rejection reason when metrics are wired (daemon mode),
        // a no-op in batch/test contexts. Shared by the reject sites below.
        let reject = |reason: OtlpRejectReason| {
            if let Some(m) = state.metrics.as_ref() {
                m.record_otlp_reject(reason);
            }
        };
        // OTLP/HTTP spec: only `application/x-protobuf` is accepted by
        // perf-sentinel (we do not implement the JSON-encoded variant).
        // Reject upfront so we do not waste CPU running `prost::decode`
        // on obviously mistyped requests (curl without a Content-Type,
        // JSON clients misconfigured at the OTel Collector, etc.).
        if !is_protobuf_content_type(&headers) {
            reject(OtlpRejectReason::UnsupportedMediaType);
            return StatusCode::UNSUPPORTED_MEDIA_TYPE;
        }
        let Ok(request) = <ExportTraceServiceRequest as prost::Message>::decode(body.as_ref())
        else {
            reject(OtlpRejectReason::ParseError);
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
            reject(OtlpRejectReason::ChannelFull);
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

    // Outermost admission gate: rejects while the memory guard is
    // tripped BEFORE the request body is read, so a saturation flood
    // cannot materialize up to max_payload_size per request into RSS
    // (the in-handler check only runs after `Bytes` buffered the
    // decompressed body).
    async fn memory_pressure_guard(
        State(state): State<OtlpHttpState>,
        request: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        if let Some(m) = state.metrics.as_ref()
            && m.ingest_over_memory_limit()
        {
            m.record_otlp_reject(OtlpRejectReason::MemoryPressure);
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        next.run(request).await
    }

    let state = OtlpHttpState { sender, metrics };
    let guard_state = state.clone();
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

    // Added last = outermost = first on the way in: the memory guard
    // short-circuits before RequestBodyLimit/Decompression ever touch
    // the body.
    router.layer(axum::middleware::from_fn_with_state(
        guard_state,
        memory_pressure_guard,
    ))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
