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

use super::ANCESTOR_WALK_MAX_DEPTH as CODE_ATTRS_MAX_DEPTH;

/// Hard cap on one service's span index, and on the per-block scope index.
/// Bounds memory and avoids quadratic walks on pathological payloads. Spans
/// beyond the cap lose parent/scope attribution but are still converted into
/// events.
const MAX_SPANS_PER_SERVICE: usize = 100_000;

/// Byte length of a valid `OTel` trace id. The proto bounds nothing.
const TRACE_ID_LEN: usize = 16;

/// Parent-lookup index per `service.name`, built once for a whole request.
type ServiceSpanIndexes<'a> = HashMap<&'a str, HashMap<&'a [u8], &'a Span>>;

/// Link-bearing CONSUMER spans grouped by parent, per `service.name`.
///
/// A parent can hold several, so unlike [`ServiceSpanIndexes`] the value is a
/// list: picking one at index time would decide by payload order.
type ServiceConsumerIndexes<'a> = HashMap<&'a str, HashMap<&'a [u8], Vec<&'a Span>>>;

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
    // RPC semconv (gRPC, Dubbo, ...): no statement or URL, so these are the
    // only keys that identify the callee. See classify_io_event.
    rpc_system: Option<&'a str>,
    rpc_service: Option<&'a str>,
    rpc_method: Option<&'a str>,
    // Messaging semconv (Kafka, RabbitMQ, Pulsar, SQS, NATS, JMS): like RPC,
    // a publish carries neither a statement nor a URL. See classify_io_event.
    messaging_system: Option<&'a str>,
    messaging_destination_name: Option<&'a str>,
    // Pre-1.21 spelling, still emitted by older agents.
    messaging_destination: Option<&'a str>,
    messaging_body_size: Option<i64>,
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
            self.code_function_name
                .and_then(super::namespace_from_qualified_name)
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
            "messaging.system" => out.messaging_system = any_value_as_str(value),
            "messaging.destination.name" => {
                out.messaging_destination_name = any_value_as_str(value);
            }
            "messaging.destination" => out.messaging_destination = any_value_as_str(value),
            "messaging.message.body.size" => out.messaging_body_size = any_value_as_int(value),
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
    let namespace = namespace_explicit
        .or_else(|| function_name_stable.and_then(super::namespace_from_qualified_name));
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

/// Producer trace this span's work was triggered by, or `None`.
///
/// Reads the first span link of the CONSUMER span that triggered this work,
/// the edge `OTel` uses when the consumer starts its own trace. Gated on
/// CONSUMER because batch span processors and follows-from relations emit
/// links too, and those are not causality. The length check is not cosmetic:
/// the proto bounds nothing and this runs once per descendant span.
///
/// Two topologies, because `OTel` instrumentations disagree on where the
/// `receive` span goes.
/// See `docs/design/06-INGESTION-AND-DAEMON.md`.
fn resolve_producer_link<'a>(
    span: &'a Span,
    span_index: &HashMap<&'a [u8], &'a Span>,
    consumers_by_parent: &HashMap<&'a [u8], Vec<&'a Span>>,
) -> Option<Arc<str>> {
    let valid =
        |id: &[u8]| id.len() == TRACE_ID_LEN && id != span.trace_id && id.iter().any(|&b| b != 0);
    let link_of = |s: &Span| {
        s.links
            .first()
            .filter(|l| valid(&l.trace_id))
            .map(|l| Arc::from(bytes_to_hex(&l.trace_id).as_str()))
    };
    // A `receive` sibling only explains work that started after it, so a
    // scheduled flush or a health check that was already running under the
    // same parent inherits nothing. `started` is the node whose subtree is
    // being attributed, not the leaf: a handler that began before the
    // message arrived shields its children, however late their own I/O runs.
    let sibling_link = |parent_id: &[u8], started: u64| {
        if parent_id.is_empty() || parent_id.iter().all(|&b| b == 0) {
            return None;
        }
        consumers_by_parent
            .get(parent_id)?
            .iter()
            .filter(|c| {
                c.trace_id == span.trace_id
                    && c.span_id != span.span_id
                    && started >= c.start_time_unix_nano
            })
            // The nearest preceding one: with several messages under a
            // consumer loop, the work belongs to the last that arrived
            // before it. Ties keep the highest span id so a re-serialised
            // payload still answers the same.
            .max_by_key(|c| (c.start_time_unix_nano, &c.span_id))
            .and_then(|c| link_of(c))
    };
    // The OTel Java and .NET Kafka instrumentations emit `receive` as a
    // sibling of the work it triggered, so it is never on the ancestor
    // chain. The handler often sits between the two, hence one sibling
    // lookup per level rather than only at the span itself.
    if let Some(found) = sibling_link(&span.parent_span_id, span.start_time_unix_nano) {
        return Some(found);
    }
    let mut found = None;
    walk_same_trace_ancestors(span, span_index, |ancestor| {
        // Keep walking when this consumer has no usable link: some emit a
        // link-less `process` span under the `receive` span that holds it.
        if ancestor.kind == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Consumer as i32 {
            found = link_of(ancestor);
        }
        if found.is_none() {
            found = sibling_link(&ancestor.parent_span_id, ancestor.start_time_unix_nano);
        }
        found.is_some()
    });
    found
}

/// Link-bearing CONSUMER spans indexed by their parent, per service.
///
/// Built only when the request carries such a span, so a bus-less fleet pays
/// one `any()` pass and nothing else. Every consumer under a parent is kept,
/// because which one triggered a given span is a question only that span's
/// start time can answer, and the exporter's ordering must not decide it.
///
/// A root consumer lands under the empty key, or under an all-zero one from
/// exporters that spell a root that way. The sibling lookup rejects both, so
/// neither can pair two roots together. They are indexed anyway so an empty
/// index means "this service has no linked consumer at all", the condition
/// that skips the ancestor walk too.
fn build_consumer_link_indexes(request: &ExportTraceServiceRequest) -> ServiceConsumerIndexes<'_> {
    let mut per_service: ServiceConsumerIndexes<'_> = HashMap::new();
    // Spans kept per service, not per block: a batch processor splits one
    // service across several `ResourceSpans`, and a per-block counter would
    // let each block spend the whole budget again.
    let mut kept_per_service: HashMap<&str, usize> = HashMap::new();
    for resource_spans in &request.resource_spans {
        let service = resource_service_name(resource_spans);
        index_linked_consumers(
            resource_spans,
            per_service.entry(service).or_default(),
            kept_per_service.entry(service).or_default(),
        );
    }
    per_service
}

/// Whether the request carries any link-bearing CONSUMER span at all.
///
/// Short-circuits on the first one, so a fleet on a bus pays almost nothing
/// here and a bus-less one pays a single traversal instead of building and
/// allocating an index per service it would never read.
fn any_linked_consumer(request: &ExportTraceServiceRequest) -> bool {
    request.resource_spans.iter().any(|resource_spans| {
        resource_spans.scope_spans.iter().any(|scope_spans| {
            scope_spans.spans.iter().any(|span| {
                span.kind == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Consumer as i32
                    && !span.links.is_empty()
            })
        })
    })
}

/// Index one block's link-bearing CONSUMER spans into a service's `index`.
///
/// `kept` carries across blocks so the cap bounds the service. Past the cap
/// it is advanced one further as a latch, so a service split across many
/// blocks warns once instead of once per block.
fn index_linked_consumers<'a>(
    resource_spans: &'a opentelemetry_proto::tonic::trace::v1::ResourceSpans,
    index: &mut HashMap<&'a [u8], Vec<&'a Span>>,
    kept: &mut usize,
) {
    let linked_consumers = resource_spans
        .scope_spans
        .iter()
        .flat_map(|scope_spans| &scope_spans.spans)
        .filter(|span| {
            !span.links.is_empty()
                && span.kind
                    == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Consumer as i32
        });
    for span in linked_consumers {
        if *kept >= MAX_SPANS_PER_SERVICE {
            if *kept == MAX_SPANS_PER_SERVICE {
                tracing::warn!(
                    "OTLP consumer-link index capped at {} entries for one service, producer links may be missing for its remaining spans",
                    MAX_SPANS_PER_SERVICE
                );
                *kept += 1;
            }
            return;
        }
        index.entry(&span.parent_span_id).or_default().push(span);
        *kept += 1;
    }
}

/// Inbound HTTP endpoint carried by a single ancestor span, or `None`.
///
/// `http.route` is a server-side route template by semconv, so it counts on
/// any kind. `http.url` and `url.full` are also what an instrumented HTTP
/// *client* records, so a CLIENT span is skipped: otherwise a DB span nested
/// under an outbound call would be attributed to the third party the caller
/// reached rather than to the route being served. Kinds left unspecified stay
/// eligible, which is what manual and legacy instrumentation emits.
fn inbound_http_endpoint(span: &Span) -> Option<&str> {
    let usable = |s: &&str| !s.trim().is_empty();
    get_str_attribute(&span.attributes, "http.route")
        .filter(usable)
        .or_else(|| {
            if span.kind == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Client as i32 {
                return None;
            }
            get_str_attribute(&span.attributes, "http.url")
                .or_else(|| get_str_attribute(&span.attributes, "url.full"))
                .filter(usable)
        })
}

/// Resolve `source.endpoint`: nearest inbound HTTP route up the parent
/// chain, then the outermost `code.*` frame for entry points that have
/// none (scheduled jobs, message consumers), then `"unknown"`.
///
/// One walk serves both. The frame kept is the outermost usable one, not
/// the nearest: on a layered stack the nearest is the DAO every caller
/// shares, which collides in the ack signature exactly as `"unknown"` did.
/// A route always wins, since only an entry point carries one.
fn resolve_source_endpoint<'a>(
    leaf: CodeAttrs<'a>,
    parent_span_id: &[u8],
    span_index: &HashMap<&[u8], &'a Span>,
) -> String {
    let mut outermost_frame =
        crate::ingest::code_frame_endpoint(leaf.namespace, leaf.function_name);
    let mut current_parent_id = parent_span_id;
    let mut depth = 0;
    while !current_parent_id.is_empty() {
        let Some(parent) = span_index.get(current_parent_id) else {
            break;
        };
        if let Some(route) = inbound_http_endpoint(parent) {
            return route.to_string();
        }
        let attrs = read_code_attrs(&parent.attributes);
        if let Some(frame) =
            crate::ingest::code_frame_endpoint(attrs.namespace, attrs.function_name)
        {
            outermost_frame = Some(frame);
        }
        if depth >= CODE_ATTRS_MAX_DEPTH {
            break;
        }
        current_parent_id = parent.parent_span_id.as_slice();
        depth += 1;
    }
    outermost_frame.unwrap_or_else(|| "unknown".to_string())
}

// ── Main conversion function ────────────────────────────────────────

/// Build a span index for parent lookup across the whole request (capped at
/// [`MAX_SPANS_PER_SERVICE`] spans).
///
/// One index per service, spanning every `ResourceSpans` block that service
/// owns: the batch processor splits one trace across blocks, and a per-block
/// index lost the endpoint at the boundary. Services stay apart so a leaf
/// cannot adopt a caller's frame or route, and each gets its own
/// [`MAX_SPANS_PER_SERVICE`] budget.
fn build_span_indexes(request: &ExportTraceServiceRequest) -> ServiceSpanIndexes<'_> {
    let mut per_service: ServiceSpanIndexes<'_> = HashMap::new();
    for resource_spans in &request.resource_spans {
        let index = per_service
            .entry(resource_service_name(resource_spans))
            .or_default();
        for scope_spans in &resource_spans.scope_spans {
            for span in &scope_spans.spans {
                // A span with no id would be indexed under the empty key, which
                // is exactly the `parent_span_id` every root span carries.
                if span.span_id.is_empty() {
                    continue;
                }
                if index.len() >= MAX_SPANS_PER_SERVICE {
                    tracing::warn!(
                        "OTLP span index capped at {} entries for one service, parent lookup may be degraded for its remaining spans",
                        MAX_SPANS_PER_SERVICE
                    );
                    break;
                }
                index.insert(&span.span_id, span);
            }
        }
    }
    per_service
}

/// `service.name` from the resource attributes, or `"unknown"`. Shared by the
/// span index and the emitted events so both agree.
fn resource_service_name(
    resource_spans: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> &str {
    resource_spans
        .resource
        .as_ref()
        .and_then(|r| get_str_attribute(&r.attributes, "service.name"))
        .unwrap_or("unknown")
}

/// Build a `span_id -> instrumentation scope name` index alongside the
/// span index. Same [`MAX_SPANS_PER_SERVICE`] cap, entries beyond it simply
/// lose scope attribution.
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
            if count >= MAX_SPANS_PER_SERVICE {
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
/// [`MAX_SPANS_PER_SERVICE`] (spans beyond the cap classify inline at
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
    let mut out = Vec::with_capacity(total.min(MAX_SPANS_PER_SERVICE));
    'outer: for scope_spans in &resource_spans.scope_spans {
        for span in &scope_spans.spans {
            if out.len() >= MAX_SPANS_PER_SERVICE {
                break 'outer;
            }
            out.push(classify_span_attrs(&span.attributes));
        }
    }
    out
}

/// One span's role in the stitch pre-pass.
enum SpanRole<'a> {
    /// Statement-bearing SQL span, statement borrowed for adoption.
    Donor(&'a str),
    /// Statement-less execute/query span. `has_sql_db_system` false means the
    /// engine sits only on a child layer (PHP Doctrine), so it is admitted
    /// only with a sibling donor (checked by the caller).
    Orphan { has_sql_db_system: bool },
    /// Not a stitch participant (empty id, non-SQL store, HTTP/RPC, or not an
    /// execute/query span).
    Skip,
}

/// Classify one span's stitch role. Statement resolution and the SQL/non-SQL
/// gating match `classify_io_event`.
fn classify_stitch_role<'a>(span: &Span, c: &ClassifiedAttrs<'a>) -> SpanRole<'a> {
    if span.trace_id.is_empty() || span.span_id.is_empty() {
        return SpanRole::Skip;
    }
    let db_system = c
        .effective_db_system()
        .map(crate::ingest::canonical_db_system);
    if db_system.is_some_and(crate::ingest::is_non_sql_db_system) {
        return SpanRole::Skip;
    }
    if let Some(statement) = resolve_sql_statement(c, db_system) {
        SpanRole::Donor(statement)
    } else if !has_http_signal(c)
        && c.rpc_system.is_none()
        && c.messaging_system.is_none()
        && looks_like_query_execution(&span.name)
    {
        SpanRole::Orphan {
            has_sql_db_system: db_system.is_some_and(crate::ingest::is_sql_db_system),
        }
    } else {
        SpanRole::Skip
    }
}

/// Classify the resource's SQL spans into donors (statement-bearing) and
/// orphans (statement-less execute/query spans). Non-SQL datastores and
/// spans with empty ids never participate.
///
/// A donor needs a resolvable statement, not a `db.system`: the PHP `OTel`
/// contrib doctrine layer carries `db.query.text` with no `db.system` (that
/// attribute sits only on the child pdo layer), and the statement-bearing
/// `SELECT orders` span must be usable as a donor for its `Doctrine::execute`
/// sibling. An orphan with a SQL `db.system` is admitted directly; an orphan
/// with no `db.system` (again the doctrine layer) is admitted only when it
/// has a statement-bearing sibling, so ORM logical-op spans that wrap their
/// own SQL child (Ruby `ActiveRecord`) do not adopt a descendant's statement.
///
/// `classified` is the capped per-resource cache: spans beyond it never
/// participate and convert exactly as before.
fn collect_stitch_participants<'a>(
    resource_spans: &'a opentelemetry_proto::tonic::trace::v1::ResourceSpans,
    classified: &[ClassifiedAttrs<'a>],
) -> (Vec<StitchDonor<'a>>, Vec<&'a Span>) {
    let mut donors = Vec::new();
    // (span, has_sql_db_system): orphans without a db.system are kept only
    // if a sibling donor exists (resolved after the full pass).
    let mut provisional: Vec<(&'a Span, bool)> = Vec::new();
    let mut idx = 0usize;
    'outer: for scope_spans in &resource_spans.scope_spans {
        for span in &scope_spans.spans {
            let Some(c) = classified.get(idx) else {
                break 'outer;
            };
            idx += 1;
            match classify_stitch_role(span, c) {
                SpanRole::Donor(statement) => donors.push(StitchDonor { span, statement }),
                SpanRole::Orphan { has_sql_db_system } => {
                    provisional.push((span, has_sql_db_system));
                }
                SpanRole::Skip => {}
            }
        }
    }

    let donor_parents: HashSet<SpanKey<'a>> = donors
        .iter()
        .filter(|d| !d.span.parent_span_id.is_empty())
        .map(|d| (d.span.trace_id.as_slice(), d.span.parent_span_id.as_slice()))
        .collect();
    let orphans = provisional
        .into_iter()
        .filter(|&(span, is_sql)| {
            is_sql
                || donor_parents
                    .contains(&(span.trace_id.as_slice(), span.parent_span_id.as_slice()))
        })
        .map(|(span, _)| span)
        .collect();
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

/// Bucket surviving donors by parent (sibling lookup) and by same-trace
/// ancestor (descendant lookup); sibling buckets are sorted by start time so
/// carriers binary-search their relevant siblings. O(n), no quadratic scans.
fn bucket_surviving_donors<'a>(
    donors: &[StitchDonor<'a>],
    donor_suppressed: &[bool],
    span_index: &HashMap<&'a [u8], &'a Span>,
) -> (
    HashMap<SpanKey<'a>, Vec<usize>>,
    HashMap<SpanKey<'a>, Vec<usize>>,
) {
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
    for bucket in donors_by_parent.values_mut() {
        bucket.sort_unstable_by_key(|&i| (donors[i].span.start_time_unix_nano, i));
    }
    (donors_by_parent, donors_by_ancestor)
}

/// Related-donor candidates for one carrier orphan: nearest siblings (from the
/// sorted parent bucket), same-trace ancestors, then descendants.
#[allow(clippy::too_many_arguments)]
fn collect_orphan_candidates<'a>(
    orphan: &'a Span,
    donors: &[StitchDonor<'a>],
    donor_by_id: &HashMap<SpanKey<'a>, usize>,
    donor_suppressed: &[bool],
    donor_consumed: &[bool],
    donors_by_parent: &HashMap<SpanKey<'a>, Vec<usize>>,
    donors_by_ancestor: &HashMap<SpanKey<'a>, Vec<usize>>,
    span_index: &HashMap<&'a [u8], &'a Span>,
    candidates: &mut Vec<usize>,
) {
    if !orphan.parent_span_id.is_empty()
        && let Some(siblings) =
            donors_by_parent.get(&(orphan.trace_id.as_slice(), orphan.parent_span_id.as_slice()))
    {
        push_sibling_candidates(
            donors,
            donor_consumed,
            siblings,
            orphan.start_time_unix_nano,
            candidates,
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

    // Rule 3: each carrier adopts the nearest related donor's statement.
    if !carriers.is_empty() {
        let (donors_by_parent, donors_by_ancestor) =
            bucket_surviving_donors(&donors, &donor_suppressed, span_index);
        let mut candidates: Vec<usize> = Vec::new();
        for orphan in carriers {
            candidates.clear();
            collect_orphan_candidates(
                orphan,
                &donors,
                &donor_by_id,
                &donor_suppressed,
                &donor_consumed,
                &donors_by_parent,
                &donors_by_ancestor,
                span_index,
                &mut candidates,
            );
            if let Some(i) = nearest_donor(
                &donors,
                &donor_consumed,
                &candidates,
                orphan.start_time_unix_nano,
            ) {
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
/// skipped; see `classify_io_event`. Parent lookup is done within the same
/// request. `source.endpoint` resolves to the nearest inbound HTTP route up
/// the parent chain, then to the `code.*` frame for entry points that have
/// none (scheduled jobs, message consumers), and only otherwise to
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

    // Parent chains cross ResourceSpans blocks when the batch processor
    // splits a trace, but never cross services.
    let span_indexes = build_span_indexes(request);
    let empty_index = HashMap::new();
    let empty_consumers = HashMap::new();
    // One O(spans) pass, so the per-span ancestor walk is skipped entirely
    // on a fleet with no broker.
    let consumer_indexes = if any_linked_consumer(request) {
        build_consumer_link_indexes(request)
    } else {
        HashMap::new()
    };

    for resource_spans in &request.resource_spans {
        // Build the per-Resource Arc<str> once, then Arc::clone into each span.
        // A resource_spans block routinely carries hundreds of spans for the
        // same service.name, so this collapses N allocations to one.
        let service_name = resource_service_name(resource_spans);
        let service_arc: Arc<str> = Arc::from(service_name);
        let span_index = span_indexes.get(service_name).unwrap_or(&empty_index);
        let consumer_index = consumer_indexes
            .get(service_name)
            .unwrap_or(&empty_consumers);

        // cloud.region: resource-level with span-level fallback in convert_span.
        // Invalid values silently dropped (sanitization at ingest boundary).
        let resource_cloud_region: Option<Arc<str>> = resource_spans
            .resource
            .as_ref()
            .and_then(|r| get_str_attribute(&r.attributes, "cloud.region"))
            .filter(|s| crate::score::carbon::is_valid_region_id(s))
            .map(Arc::from);

        let scope_index = build_scope_index(resource_spans);
        let classified = classify_resource_spans(resource_spans);
        let stitch = compute_stitch_decisions(resource_spans, span_index, &classified);

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
                    span_index,
                    &scope_index,
                    stitched_statement,
                    cached_attrs,
                    consumer_index,
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

/// Classify an analyzable span as SQL, outbound HTTP or a message publish,
/// returning `(event_type, target, operation)`. `None` when it carries no
/// statement, no URL, no RPC client method and no messaging destination.
/// `kind` is the OTLP `SpanKind`, used to admit only CLIENT-side RPC spans
/// and PRODUCER-side messaging spans. Supports both legacy (pre-1.21) and
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
    } else if let Some(system) = c.rpc_system.filter(|_| {
        // Gated here, not in the body, so a non-CLIENT span carrying rpc.*
        // still reaches the messaging branch instead of being dropped.
        kind == opentelemetry_proto::tonic::trace::v1::span::SpanKind::Client as i32
    }) {
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
    } else if c.messaging_system.is_some() {
        classify_messaging_event(c, span_name, kind)
    } else {
        None
    }
}

/// Payload size for the carbon size tiers: the HTTP response body, or the
/// published message body. `None` for SQL.
fn payload_size_bytes(event_type: &EventType, c: &ClassifiedAttrs<'_>) -> Option<u64> {
    match event_type {
        EventType::HttpOut => c.http_response_body_size.or(c.http_response_content_length),
        EventType::Messaging => c.messaging_body_size,
        EventType::Sql => None,
    }
    .and_then(|v| u64::try_from(v).ok())
}

/// Message publish, or `None` when the span is not the producer side.
///
/// One convention covers Kafka, `RabbitMQ`, Pulsar, SQS, NATS and JMS. PRODUCER
/// only: a consumer describes work on a delivered message, and a polling one
/// would flood the occurrence detectors. Target is the destination, else the
/// span name (agents shape it `<destination> publish`).
fn classify_messaging_event(
    c: &ClassifiedAttrs<'_>,
    span_name: &str,
    kind: i32,
) -> Option<(EventType, String, String)> {
    let system = c.messaging_system.filter(|s| !s.trim().is_empty())?;
    if kind != opentelemetry_proto::tonic::trace::v1::span::SpanKind::Producer as i32 {
        return None;
    }
    // Blank per field, so an empty destination.name does not shadow the
    // legacy key.
    let target = c
        .messaging_destination_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            c.messaging_destination
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map_or_else(|| span_name.trim().to_string(), ToString::to_string);
    if target.is_empty() {
        return None;
    }
    Some((EventType::Messaging, target, system.to_string()))
}

/// Owned attribute rebuild for the spans that cannot borrow the
/// per-resource cache: stitched spans (statement adopted from a donor
/// span, injected so the whole SQL tail runs unchanged) and spans beyond
/// `MAX_SPANS_PER_SERVICE`. `None` on the borrow-the-cache hot path.
fn rebuilt_classified<'a>(
    span: &'a Span,
    cached_attrs: Option<&ClassifiedAttrs<'a>>,
    stitched_statement: Option<&'a str>,
) -> Option<ClassifiedAttrs<'a>> {
    if cached_attrs.is_some() && stitched_statement.is_none() {
        return None;
    }
    let mut rebuilt = classify_span_attrs(&span.attributes);
    if stitched_statement.is_some() {
        rebuilt.db_statement = stitched_statement;
    }
    Some(rebuilt)
}

/// Convert a single OTLP span to a `SpanEvent`, if it is an I/O operation.
///
/// Non-I/O spans return the filter reason so the caller can tally them.
#[allow(clippy::too_many_arguments)] // per-request context, each one distinct
fn convert_span<'a>(
    span: &'a Span,
    service_arc: &Arc<str>,
    resource_cloud_region: Option<&Arc<str>>,
    span_index: &HashMap<&[u8], &Span>,
    scope_index: &HashMap<&[u8], &str>,
    stitched_statement: Option<&'a str>,
    cached_attrs: Option<&ClassifiedAttrs<'a>>,
    consumer_index: &HashMap<&'a [u8], Vec<&'a Span>>,
) -> Result<SpanEvent, OtlpSpanFilterReason> {
    let owned = rebuilt_classified(span, cached_attrs, stitched_statement);
    let classified = match (&owned, cached_attrs) {
        (Some(rebuilt), _) => rebuilt,
        (None, Some(cached)) => cached,
        (None, None) => unreachable!("rebuilt_classified rebuilds on cache miss"),
    };
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
        classify_io_event(classified, db_system, &span.name, span.kind)
    else {
        return Err(span_filter_reason(classified, db_system, span.kind));
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

    let response_size_bytes = payload_size_bytes(&event_type, classified);

    // code.* attributes: leaf attrs first, walk parents only when empty.
    // OTel JDBC and HTTP-client spans rarely carry their own code.*; the
    // user frame sits on a parent.
    let code =
        walk_parents_for_code_attrs(classified.code_attrs(), &span.parent_span_id, span_index);

    // Source method comes from the direct parent, unchanged: it is display
    // metadata and does not enter the ack signature.
    let source_method = if span.parent_span_id.is_empty() {
        span.name.clone()
    } else if let Some(parent) = span_index.get(span.parent_span_id.as_slice()) {
        get_str_attribute(&parent.attributes, "code.function")
            .map_or_else(|| parent.name.clone(), ToString::to_string)
    } else {
        span.name.clone()
    };

    let source_endpoint =
        resolve_source_endpoint(classified.code_attrs(), &span.parent_span_id, span_index);

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

    let code_function: Option<Arc<str>> = code.function_name.map(Arc::from);
    let code_filepath: Option<Arc<str>> = code.filepath.map(Arc::from);
    let code_lineno = code.lineno.and_then(|v| u32::try_from(v).ok());
    let code_namespace: Option<Arc<str>> = code.namespace.map(Arc::from);

    let instrumentation_scopes = collect_instrumentation_scopes(span, span_index, scope_index);
    let link_trace_id = (!consumer_index.is_empty())
        .then(|| resolve_producer_link(span, span_index, consumer_index))
        .flatten();

    let mut event = SpanEvent {
        timestamp,
        trace_id,
        span_id,
        parent_span_id,
        link_trace_id,
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
