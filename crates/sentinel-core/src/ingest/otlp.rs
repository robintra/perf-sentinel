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

use crate::event::{EventSource, EventType, SpanEvent};
use crate::report::metrics::OtlpRejectReason;

/// Sink for the rejection counters this module emits.
///
/// `ingest::otlp` produced runtime telemetry on every rejection path
/// (unsupported media type, decode failure, channel full). Before
/// 0.6.0 these calls reached straight into `report::metrics::MetricsState`,
/// which leaked the downstream metrics implementation upstream and made
/// `ingest` impossible to use without paying for the Prometheus registry.
///
/// This trait is the abstraction. `MetricsState` implements it (in
/// `report::metrics`) so daemon callers keep the same wiring; alternative
/// builds (e.g. a future fork that wants OpenTelemetry metrics, or
/// tests that want a counting fake) can plug their own sink without
/// touching `ingest`.
///
/// `Send + Sync` are required because the gRPC and HTTP paths share
/// the sink across tokio tasks via `Arc<dyn MetricsSink>`.
pub trait MetricsSink: Send + Sync {
    /// Record one rejected OTLP request, labeled by reason.
    fn record_otlp_reject(&self, reason: OtlpRejectReason);
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
    fn code_attrs(&self) -> CodeAttrs<'a> {
        let function_name = self.code_function_name.or(self.code_function);
        let filepath = self.code_file_path.or(self.code_filepath);
        let lineno = self.code_line_number.or(self.code_lineno);
        let namespace = self.code_namespace.or_else(|| {
            self.code_function_name
                .and_then(|fq| fq.rsplit_once('.').map(|(ns, _)| ns))
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
    let namespace = namespace_explicit
        .or_else(|| function_name_stable.and_then(|fq| fq.rsplit_once('.').map(|(ns, _)| ns)));
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
/// Build a span index for parent lookup within a single resource (capped at 100k spans).
fn build_span_index(
    resource_spans: &opentelemetry_proto::tonic::trace::v1::ResourceSpans,
) -> HashMap<&[u8], &Span> {
    let mut index: HashMap<&[u8], &Span> = HashMap::new();
    let mut count = 0usize;
    'outer: for scope_spans in &resource_spans.scope_spans {
        for span in &scope_spans.spans {
            index.insert(&span.span_id, span);
            count += 1;
            if count >= 100_000 {
                tracing::warn!(
                    "OTLP span index capped at 100k entries, parent lookup may be degraded for remaining spans"
                );
                break 'outer;
            }
        }
    }
    index
}

/// Build a `span_id -> instrumentation scope name` index alongside the
/// span index. Same 100k cap as `build_span_index`, entries beyond the
/// cap simply lose scope attribution.
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
            if count >= 100_000 {
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
    let mut events = Vec::new();

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
                if let Some(event) = convert_span(
                    span,
                    &service_arc,
                    resource_cloud_region.as_ref(),
                    &span_index,
                    &scope_index,
                ) {
                    events.push(event);
                }
            }
        }
    }

    events
}

/// Convert a single OTLP span to a `SpanEvent`, if it is an I/O operation.
fn convert_span(
    span: &Span,
    service_arc: &Arc<str>,
    resource_cloud_region: Option<&Arc<str>>,
    span_index: &HashMap<&[u8], &Span>,
    scope_index: &HashMap<&[u8], &str>,
) -> Option<SpanEvent> {
    let classified = classify_span_attrs(&span.attributes);

    // Determine event type: SQL if db.statement/db.query.text present, HTTP if http.url/url.full present.
    // Supports both legacy (pre-1.21) and stable (1.21+) OTel semantic conventions.
    let (event_type, target, operation) =
        if let Some(statement) = classified.db_statement.or(classified.db_query_text) {
            // db.system (e.g. "postgresql"), not the SQL verb. The verb is
            // extracted from target by energy_coefficient() in the scoring stage.
            let op = classified.db_system.unwrap_or("sql").to_string();
            (EventType::Sql, statement.to_string(), op)
        } else if let Some(url) = classified.http_url.or(classified.url_full) {
            let method = classified
                .http_method
                .or(classified.http_request_method)
                .unwrap_or("GET")
                .to_string();
            (EventType::HttpOut, url.to_string(), method)
        } else {
            // Not an I/O span, skip
            return None;
        };

    // Timestamps and duration
    let start_nanos = span.start_time_unix_nano;
    let end_nanos = span.end_time_unix_nano;
    let timestamp = nanos_to_iso8601(start_nanos);
    if end_nanos < start_nanos {
        tracing::trace!("Span has end_time < start_time (clock skew?), duration forced to 0");
    }
    let duration_us = end_nanos.saturating_sub(start_nanos) / 1000;

    // IDs
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
    Some(event)
}

// ── gRPC service implementation ─────────────────────────────────────

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

#[tonic::async_trait]
impl opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService
    for OtlpGrpcService
{
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        let events = convert_otlp_request(request.get_ref());
        if !events.is_empty() && self.sender.send(events).await.is_err() {
            if let Some(m) = self.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::ChannelFull);
            }
            return Err(tonic::Status::internal("event channel closed"));
        }
        Ok(tonic::Response::new(ExportTraceServiceResponse {
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
        let events = convert_otlp_request(&request);
        if !events.is_empty() && state.sender.send(events).await.is_err() {
            tracing::warn!("OTLP HTTP: event channel full or closed, dropping events");
            if let Some(m) = state.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::ChannelFull);
            }
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        StatusCode::OK
    }

    let state = OtlpHttpState { sender, metrics };
    let router = Router::new()
        .route("/v1/traces", post(handle_traces))
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
mod tests {
    use super::*;
    use crate::report::metrics::MetricsState;
    use opentelemetry_proto::tonic::common::v1::AnyValue;
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans};

    /// Build a metrics sink from a fresh `MetricsState`, coerced to the
    /// trait object the OTLP module expects. Co-locates the
    /// `Arc<MetricsState>` -> `Arc<dyn MetricsSink>` cast so the four
    /// HTTP-handler tests below stay readable.
    fn fresh_metrics_sink() -> (Arc<MetricsState>, Arc<dyn MetricsSink>) {
        let state = Arc::new(MetricsState::new());
        let sink: Arc<dyn MetricsSink> = state.clone();
        (state, sink)
    }

    fn make_kv(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.to_string())),
            }),
        }
    }

    fn make_int_kv(key: &str, value: i64) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::IntValue(value)),
            }),
        }
    }

    fn make_sql_span(
        trace_id: &[u8],
        span_id: &[u8],
        parent_span_id: &[u8],
        statement: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> Span {
        Span {
            trace_id: trace_id.to_vec(),
            span_id: span_id.to_vec(),
            parent_span_id: parent_span_id.to_vec(),
            name: "db.query".to_string(),
            start_time_unix_nano: start_ns,
            end_time_unix_nano: end_ns,
            attributes: vec![
                make_kv("db.statement", statement),
                make_kv("db.system", "postgresql"),
            ],
            ..Default::default()
        }
    }

    #[allow(clippy::too_many_arguments)] // test helper builds a full OTLP Span with all required fields
    fn make_http_span(
        trace_id: &[u8],
        span_id: &[u8],
        parent_span_id: &[u8],
        url: &str,
        method: &str,
        status: i64,
        start_ns: u64,
        end_ns: u64,
    ) -> Span {
        Span {
            trace_id: trace_id.to_vec(),
            span_id: span_id.to_vec(),
            parent_span_id: parent_span_id.to_vec(),
            name: "http.request".to_string(),
            start_time_unix_nano: start_ns,
            end_time_unix_nano: end_ns,
            attributes: vec![
                make_kv("http.url", url),
                make_kv("http.method", method),
                make_int_kv("http.status_code", status),
            ],
            ..Default::default()
        }
    }

    fn make_parent_span(span_id: &[u8], route: &str) -> Span {
        Span {
            trace_id: vec![1; 16],
            span_id: span_id.to_vec(),
            parent_span_id: vec![],
            name: "HandleRequest".to_string(),
            start_time_unix_nano: 0,
            end_time_unix_nano: 1_000_000_000,
            attributes: vec![
                make_kv("http.route", route),
                make_kv("code.function", "OrderService::create_order"),
            ],
            ..Default::default()
        }
    }

    fn make_request(service: &str, spans: Vec<Span>) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![make_kv("service.name", service)],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans,
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn empty_request_returns_empty() {
        let req = ExportTraceServiceRequest {
            resource_spans: vec![],
        };
        assert!(convert_otlp_request(&req).is_empty());
    }

    #[test]
    fn sql_span_maps_correctly() {
        let span = make_sql_span(
            &[1; 16],
            &[2; 8],
            &[],
            "SELECT * FROM order_item WHERE order_id = 42",
            1_720_621_921_000_000_000, // 2024-07-10T14:32:01.000Z
            1_720_621_921_001_200_000, // +1.2ms
        );
        let req = make_request("order-svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Sql);
        assert_eq!(events[0].operation, "postgresql");
        assert_eq!(
            events[0].target,
            "SELECT * FROM order_item WHERE order_id = 42"
        );
        assert_eq!(&*events[0].service, "order-svc");
        assert_eq!(events[0].duration_us, 1200);
        assert!(events[0].status_code.is_none());
    }

    #[test]
    fn http_span_maps_correctly() {
        let span = make_http_span(
            &[1; 16],
            &[3; 8],
            &[],
            "http://user-svc:5000/api/users/123",
            "GET",
            200,
            1_720_621_921_000_000_000,
            1_720_621_921_015_000_000, // +15ms
        );
        let req = make_request("order-svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::HttpOut);
        assert_eq!(events[0].operation, "GET");
        assert_eq!(events[0].target, "http://user-svc:5000/api/users/123");
        assert_eq!(events[0].status_code, Some(200));
        assert_eq!(events[0].duration_us, 15000);
    }

    #[test]
    fn non_io_span_skipped() {
        let span = Span {
            trace_id: vec![1; 16],
            span_id: vec![4; 8],
            name: "internal.processing".to_string(),
            start_time_unix_nano: 1_720_621_921_000_000_000,
            end_time_unix_nano: 1_720_619_521_000_500_000,
            attributes: vec![make_kv("custom.attr", "value")],
            ..Default::default()
        };
        let req = make_request("order-svc", vec![span]);
        assert!(convert_otlp_request(&req).is_empty());
    }

    #[test]
    fn parent_span_provides_source_endpoint() {
        let parent = make_parent_span(&[10; 8], "POST /api/orders/{id}/submit");
        let child = make_sql_span(
            &[1; 16],
            &[20; 8],
            &[10; 8], // parent_span_id
            "SELECT * FROM order_item WHERE order_id = 42",
            1_720_621_921_000_000_000,
            1_720_621_921_001_200_000,
        );
        let req = make_request("order-svc", vec![parent, child]);
        let events = convert_otlp_request(&req);

        // Only the child (SQL) should produce an event
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "POST /api/orders/{id}/submit");
        assert_eq!(events[0].source.method, "OrderService::create_order");
    }

    #[test]
    fn parent_span_http_route_takes_precedence_over_http_url() {
        // Critical for ack stability: when the parent emits both
        // http.route (template) and http.url (instantiated), the route
        // must win. Otherwise every distinct request id forks the ack
        // signature.
        let parent = Span {
            trace_id: vec![1; 16],
            span_id: vec![10; 8],
            parent_span_id: vec![],
            name: "HandleRequest".to_string(),
            start_time_unix_nano: 0,
            end_time_unix_nano: 1_000_000_000,
            attributes: vec![
                make_kv("http.route", "POST /api/orders/{id}/submit"),
                make_kv("http.url", "http://order-svc/api/orders/42/submit"),
                make_kv("code.function", "OrderService::create_order"),
            ],
            ..Default::default()
        };
        let child = make_sql_span(
            &[1; 16],
            &[20; 8],
            &[10; 8],
            "SELECT * FROM order_item WHERE order_id = 42",
            1_720_621_921_000_000_000,
            1_720_621_921_001_200_000,
        );
        let req = make_request("order-svc", vec![parent, child]);
        let events = convert_otlp_request(&req);

        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql child event present");
        assert_eq!(sql.source.endpoint, "POST /api/orders/{id}/submit");
    }

    #[test]
    fn parent_span_http_url_used_only_when_route_absent() {
        // Documented fallback: instrumentation that omits http.route
        // (legacy SDK, manual instrumentation) loses signature stability
        // but still produces a usable endpoint string.
        let parent = Span {
            trace_id: vec![1; 16],
            span_id: vec![10; 8],
            parent_span_id: vec![],
            name: "HandleRequest".to_string(),
            start_time_unix_nano: 0,
            end_time_unix_nano: 1_000_000_000,
            attributes: vec![make_kv("http.url", "http://order-svc/api/orders/42/submit")],
            ..Default::default()
        };
        let child = make_sql_span(&[1; 16], &[20; 8], &[10; 8], "SELECT 1", 0, 1_000_000);
        let req = make_request("order-svc", vec![parent, child]);
        let events = convert_otlp_request(&req);

        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql child event present");
        assert_eq!(sql.source.endpoint, "http://order-svc/api/orders/42/submit");
    }

    #[test]
    fn parent_span_url_full_used_when_neither_route_nor_url_present() {
        // OTel stable v1.21+ replaces http.url with url.full. Last-resort
        // fallback once http.route and http.url are both absent.
        let parent = Span {
            trace_id: vec![1; 16],
            span_id: vec![10; 8],
            parent_span_id: vec![],
            name: "HandleRequest".to_string(),
            start_time_unix_nano: 0,
            end_time_unix_nano: 1_000_000_000,
            attributes: vec![make_kv("url.full", "http://order-svc/api/orders/42")],
            ..Default::default()
        };
        let child = make_sql_span(&[1; 16], &[20; 8], &[10; 8], "SELECT 1", 0, 1_000_000);
        let req = make_request("order-svc", vec![parent, child]);
        let events = convert_otlp_request(&req);

        let sql = events
            .iter()
            .find(|e| e.event_type == EventType::Sql)
            .expect("sql child event present");
        assert_eq!(sql.source.endpoint, "http://order-svc/api/orders/42");
    }

    #[test]
    fn missing_parent_falls_back() {
        let child = make_sql_span(
            &[1; 16],
            &[20; 8],
            &[99; 8], // parent not in batch
            "SELECT * FROM order_item WHERE order_id = 42",
            1_720_621_921_000_000_000,
            1_720_621_921_001_200_000,
        );
        let req = make_request("order-svc", vec![child]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "unknown");
        assert_eq!(events[0].source.method, "db.query");
    }

    #[test]
    fn trace_id_hex_encoding() {
        let trace_bytes: Vec<u8> = (0..16).collect();
        assert_eq!(
            bytes_to_hex(&trace_bytes),
            "000102030405060708090a0b0c0d0e0f"
        );
    }

    #[test]
    fn timestamp_nanos_to_iso8601() {
        // 2024-07-10T14:32:01.123Z UTC
        let nanos: u64 = 1_720_621_921_123_000_000;
        let iso = nanos_to_iso8601(nanos);
        assert_eq!(iso, "2024-07-10T14:32:01.123Z");
    }

    #[test]
    fn timestamp_epoch_zero() {
        assert_eq!(nanos_to_iso8601(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn duration_calculation() {
        let span = make_sql_span(
            &[1; 16],
            &[2; 8],
            &[],
            "SELECT 1",
            1_000_000_000, // 1 second
            1_002_500_000, // +2.5ms = 2500us
        );
        let req = make_request("test", vec![span]);
        let events = convert_otlp_request(&req);
        assert_eq!(events[0].duration_us, 2500);
    }

    #[test]
    fn status_code_extraction() {
        let span = make_http_span(
            &[1; 16],
            &[3; 8],
            &[],
            "http://svc/api/health",
            "GET",
            404,
            1_000_000_000,
            1_001_000_000,
        );
        let req = make_request("test", vec![span]);
        let events = convert_otlp_request(&req);
        assert_eq!(events[0].status_code, Some(404));
    }

    #[test]
    fn service_name_from_resource() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request("my-service", vec![span]);
        let events = convert_otlp_request(&req);
        assert_eq!(&*events[0].service, "my-service");
    }

    #[test]
    fn span_with_both_db_and_http_prefers_sql() {
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        let mut span = make_sql_span(
            &[1; 16],
            &[2; 8],
            &[],
            "SELECT 1",
            1_000_000_000,
            1_001_000_000,
        );
        // Add http.url attribute too
        span.attributes.push(KeyValue {
            key: "http.url".to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue("http://svc/api".to_string())),
            }),
        });
        let req = make_request("test", vec![span]);
        let events = convert_otlp_request(&req);
        // db.statement takes precedence
        assert_eq!(events[0].event_type, EventType::Sql);
    }

    #[test]
    fn clock_skew_duration_is_zero() {
        // end < start -> saturating_sub gives 0
        let span = make_sql_span(
            &[1; 16],
            &[2; 8],
            &[],
            "SELECT 1",
            2_000_000_000, // start = 2s
            1_000_000_000, // end = 1s (before start)
        );
        let req = make_request("test", vec![span]);
        let events = convert_otlp_request(&req);
        assert_eq!(events[0].duration_us, 0);
    }

    #[test]
    fn bytes_to_hex_empty() {
        assert_eq!(bytes_to_hex(&[]), "");
    }

    #[test]
    fn bytes_to_hex_all_values() {
        assert_eq!(bytes_to_hex(&[0x00, 0xff, 0xab]), "00ffab");
    }

    #[test]
    fn nanos_to_iso8601_leap_year() {
        // 2024-02-29T00:00:00.000Z (2024 is a leap year)
        let nanos: u64 = 1_709_164_800_000_000_000;
        let iso = nanos_to_iso8601(nanos);
        assert_eq!(iso, "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn empty_trace_id_produces_empty_hex() {
        assert_eq!(bytes_to_hex(&[]), "");
    }

    #[test]
    fn short_span_id_produces_short_hex() {
        assert_eq!(bytes_to_hex(&[0xab]), "ab");
    }

    #[test]
    fn missing_service_name_defaults_to_unknown() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![], // no service.name
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![span],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let events = convert_otlp_request(&req);
        assert_eq!(&*events[0].service, "unknown");
    }

    #[test]
    fn no_resource_defaults_to_unknown_service() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: None,
                scope_spans: vec![ScopeSpans {
                    spans: vec![span],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let events = convert_otlp_request(&req);
        assert_eq!(&*events[0].service, "unknown");
    }

    // ----- cloud.region extraction tests -----

    fn make_request_with_resource_attrs(
        attrs: Vec<KeyValue>,
        spans: Vec<Span>,
    ) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: attrs,
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans,
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn cloud_region_extracted_from_resource_attributes() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request_with_resource_attrs(
            vec![
                make_kv("service.name", "order-svc"),
                make_kv("cloud.region", "eu-west-3"),
            ],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cloud_region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn cloud_region_falls_back_to_span_attribute() {
        // Resource has no cloud.region; span itself carries it.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        span.attributes.push(make_kv("cloud.region", "us-east-1"));
        let req = make_request_with_resource_attrs(
            vec![make_kv("service.name", "order-svc")],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cloud_region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn cloud_region_resource_wins_over_span() {
        // When both are present, the resource value takes precedence
        // (canonical OTel location, single source per service).
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        span.attributes.push(make_kv("cloud.region", "us-east-1"));
        let req = make_request_with_resource_attrs(
            vec![
                make_kv("service.name", "order-svc"),
                make_kv("cloud.region", "eu-west-3"),
            ],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert_eq!(events[0].cloud_region.as_deref(), Some("eu-west-3"));
    }

    #[test]
    fn no_cloud_region_yields_none() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request("order-svc", vec![span]);
        let events = convert_otlp_request(&req);
        assert!(events[0].cloud_region.is_none());
    }

    // ----- cloud.region sanitization at OTLP boundary -----

    #[test]
    fn cloud_region_with_space_is_sanitized_to_none() {
        // Invalid character (space) at the resource level → silently dropped.
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request_with_resource_attrs(
            vec![
                make_kv("service.name", "order-svc"),
                make_kv("cloud.region", "eu west 3"),
            ],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert_eq!(events.len(), 1);
        assert!(
            events[0].cloud_region.is_none(),
            "region with space must be sanitized to None"
        );
    }

    #[test]
    fn oversized_cloud_region_is_sanitized_to_none() {
        // 65 chars, exceeds the 64-char cap.
        let long_region = "a".repeat(65);
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request_with_resource_attrs(
            vec![
                make_kv("service.name", "order-svc"),
                make_kv("cloud.region", &long_region),
            ],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn cloud_region_with_control_char_is_sanitized_to_none() {
        // Log-forging payload: newline + fake log line.
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request_with_resource_attrs(
            vec![
                make_kv("service.name", "order-svc"),
                make_kv("cloud.region", "eu-west-3\n2026-04-07 ERROR fake"),
            ],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert!(events[0].cloud_region.is_none());
    }

    #[test]
    fn cloud_region_span_level_fallback_also_sanitized() {
        // Invalid cloud.region at the span level (resource has none) →
        // silently dropped too.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        span.attributes.push(make_kv("cloud.region", "bad region!"));
        let req = make_request("order-svc", vec![span]);
        let events = convert_otlp_request(&req);
        assert!(events[0].cloud_region.is_none());
    }

    // ── instrumentation_scopes capture from OTLP ──────────────────

    fn scoped_request(service: &str, scoped: Vec<(&str, Vec<Span>)>) -> ExportTraceServiceRequest {
        use opentelemetry_proto::tonic::common::v1::InstrumentationScope;
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![make_kv("service.name", service)],
                    ..Default::default()
                }),
                scope_spans: scoped
                    .into_iter()
                    .map(|(name, spans)| ScopeSpans {
                        scope: Some(InstrumentationScope {
                            name: name.to_string(),
                            ..Default::default()
                        }),
                        spans,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn instrumentation_scope_captured_from_leaf_only() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        let req = scoped_request("svc", vec![("io.opentelemetry.jdbc", vec![span])]);
        let events = convert_otlp_request(&req);
        assert_eq!(events.len(), 1);
        let scopes: Vec<&str> = events[0]
            .instrumentation_scopes
            .iter()
            .map(AsRef::as_ref)
            .collect();
        assert_eq!(scopes, vec!["io.opentelemetry.jdbc"]);
    }

    #[test]
    fn instrumentation_scopes_walk_parent_chain_deduped() {
        // Lab-shaped trace: HTTP server (spring-webmvc) -> Spring
        // Data span (spring-data-3.0) -> Hibernate (hibernate-6.0)
        // -> JDBC leaf (jdbc). Walker collects unique scopes leaf
        // to root.
        let http = make_span_with_code_attrs(
            &[10; 8],
            &[],
            "GET /api/orders",
            vec![make_kv("http.route", "GET /api/orders")],
        );
        let spring_data =
            make_span_with_code_attrs(&[11; 8], &[10; 8], "OrderRepository.findById", vec![]);
        let hibernate = make_span_with_code_attrs(&[12; 8], &[11; 8], "Session.find", vec![]);
        let jdbc = make_sql_span(&[1; 16], &[13; 8], &[12; 8], "SELECT 1", 0, 1_000_000);
        let req = scoped_request(
            "svc",
            vec![
                ("io.opentelemetry.spring-webmvc-6.0", vec![http]),
                ("io.opentelemetry.spring-data-3.0", vec![spring_data]),
                ("io.opentelemetry.hibernate-6.0", vec![hibernate]),
                ("io.opentelemetry.jdbc", vec![jdbc]),
            ],
        );
        let events = convert_otlp_request(&req);
        assert_eq!(events.len(), 1, "only the JDBC span yields a SpanEvent");
        let scopes: Vec<&str> = events[0]
            .instrumentation_scopes
            .iter()
            .map(AsRef::as_ref)
            .collect();
        assert_eq!(
            scopes,
            vec![
                "io.opentelemetry.jdbc",
                "io.opentelemetry.hibernate-6.0",
                "io.opentelemetry.spring-data-3.0",
                "io.opentelemetry.spring-webmvc-6.0",
            ],
            "leaf-to-root order, deduplicated"
        );
    }

    #[test]
    fn instrumentation_scopes_empty_when_scope_absent() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);
        assert_eq!(events.len(), 1);
        assert!(events[0].instrumentation_scopes.is_empty());
    }

    #[test]
    fn cloud_region_empty_string_is_sanitized_to_none() {
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1000);
        let req = make_request_with_resource_attrs(
            vec![
                make_kv("service.name", "order-svc"),
                make_kv("cloud.region", ""),
            ],
            vec![span],
        );
        let events = convert_otlp_request(&req);
        assert!(events[0].cloud_region.is_none());
    }

    // ── code.* parent walk and stable convention support ────────────

    fn make_span_with_code_attrs(
        span_id: &[u8],
        parent_span_id: &[u8],
        name: &str,
        code_attrs: Vec<KeyValue>,
    ) -> Span {
        Span {
            trace_id: vec![1; 16],
            span_id: span_id.to_vec(),
            parent_span_id: parent_span_id.to_vec(),
            name: name.to_string(),
            start_time_unix_nano: 0,
            end_time_unix_nano: 1_000_000,
            attributes: code_attrs,
            ..Default::default()
        }
    }

    #[test]
    fn code_attrs_inherited_from_immediate_parent() {
        // HTTP server parent carries code.namespace; JDBC child has none.
        // Walker must surface the parent namespace on the child SpanEvent.
        let parent = make_span_with_code_attrs(
            &[10; 8],
            &[],
            "GET /api/orders",
            vec![
                make_kv("http.route", "GET /api/orders"),
                make_kv("code.namespace", "com.foo.OrderController"),
                make_kv("code.function", "list"),
            ],
        );
        let child = make_sql_span(
            &[1; 16],
            &[20; 8],
            &[10; 8],
            "SELECT * FROM orders",
            0,
            1_000_000,
        );
        let req = make_request("order-svc", vec![parent, child]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].code_namespace.as_deref(),
            Some("com.foo.OrderController")
        );
        assert_eq!(events[0].code_function.as_deref(), Some("list"));
    }

    #[test]
    fn code_attrs_inherited_from_grandparent() {
        // HTTP -> Service (carries code.*) -> Hibernate -> JDBC (no code.*).
        // Walker must traverse multiple levels.
        let http = make_span_with_code_attrs(
            &[10; 8],
            &[],
            "GET /api/orders",
            vec![make_kv("http.route", "GET /api/orders")],
        );
        let service = make_span_with_code_attrs(
            &[11; 8],
            &[10; 8],
            "OrderService.list",
            vec![
                make_kv("code.namespace", "com.foo.OrderService"),
                make_kv("code.function", "list"),
            ],
        );
        let hibernate = make_span_with_code_attrs(&[12; 8], &[11; 8], "Hibernate.query", vec![]);
        let jdbc = make_sql_span(&[1; 16], &[13; 8], &[12; 8], "SELECT 1", 0, 1_000_000);
        let req = make_request("order-svc", vec![http, service, hibernate, jdbc]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].code_namespace.as_deref(),
            Some("com.foo.OrderService")
        );
    }

    #[test]
    fn code_attrs_orphan_span_returns_none() {
        // Span with no parent and no code.* must yield code_namespace = None.
        let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert!(events[0].code_namespace.is_none());
        assert!(events[0].code_function.is_none());
    }

    #[test]
    fn code_attrs_max_depth_safety() {
        // Chain depth larger than CODE_ATTRS_MAX_DEPTH, none carry code.*.
        // Walker terminates at the cap and returns None without looping.
        let depth = u8::try_from(CODE_ATTRS_MAX_DEPTH * 2 + 4).unwrap();
        let mut spans = Vec::new();
        for i in 0..depth {
            let id = [i + 1; 8];
            let parent = if i == 0 { vec![] } else { vec![i; 8] };
            spans.push(make_span_with_code_attrs(
                &id,
                &parent,
                &format!("level.{i}"),
                vec![],
            ));
        }
        let leaf = make_sql_span(&[1; 16], &[100; 8], &[depth; 8], "SELECT 1", 0, 1_000_000);
        spans.push(leaf);
        let req = make_request("svc", spans);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert!(events[0].code_namespace.is_none());
    }

    #[test]
    fn code_attrs_self_takes_precedence() {
        // Span has its own code.namespace; parent has a different one.
        // The span's own attrs must win; the walker only triggers when the
        // span itself has nothing.
        let parent = make_span_with_code_attrs(
            &[10; 8],
            &[],
            "GET /api/x",
            vec![make_kv("code.namespace", "com.parent")],
        );
        let mut child = make_sql_span(&[1; 16], &[20; 8], &[10; 8], "SELECT 1", 0, 1_000_000);
        child
            .attributes
            .push(make_kv("code.namespace", "com.child"));
        let req = make_request("svc", vec![parent, child]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code_namespace.as_deref(), Some("com.child"));
    }

    // ── Stable code.* conventions (semconv v1.33.0) ─────────────────

    #[test]
    fn code_attrs_stable_conventions() {
        // Stable names only. Namespace must be derived from the FQ
        // function name by splitting on the last dot.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        span.attributes.extend(vec![
            make_kv("code.function.name", "com.foo.OrderService.findItems"),
            make_kv("code.file.path", "src/main/java/com/foo/OrderService.java"),
            make_int_kv("code.line.number", 42),
        ]);
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].code_function.as_deref(),
            Some("com.foo.OrderService.findItems")
        );
        assert_eq!(
            events[0].code_filepath.as_deref(),
            Some("src/main/java/com/foo/OrderService.java")
        );
        assert_eq!(events[0].code_lineno, Some(42));
        assert_eq!(
            events[0].code_namespace.as_deref(),
            Some("com.foo.OrderService")
        );
    }

    #[test]
    fn code_attrs_legacy_conventions_still_work() {
        // Legacy names only. No regression from the stable-name addition.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        span.attributes.extend(vec![
            make_kv("code.function", "findItems"),
            make_kv("code.namespace", "com.foo.OrderService"),
            make_kv("code.filepath", "src/OrderService.java"),
            make_int_kv("code.lineno", 99),
        ]);
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code_function.as_deref(), Some("findItems"));
        assert_eq!(
            events[0].code_namespace.as_deref(),
            Some("com.foo.OrderService")
        );
        assert_eq!(events[0].code_lineno, Some(99));
    }

    #[test]
    fn code_attrs_legacy_namespace_wins_over_derivation() {
        // Pathological agent emits both a stable FQ function name AND an
        // explicit legacy code.namespace. The explicit value must win,
        // not the derivation from rsplit_once.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        span.attributes.extend(vec![
            make_kv("code.function.name", "com.foo.X.y"),
            make_kv("code.namespace", "com.bar.X"),
        ]);
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code_namespace.as_deref(), Some("com.bar.X"));
    }

    #[test]
    fn code_attrs_legacy_function_does_not_derive_namespace() {
        // Legacy `code.function` is documented as a bare function name, even
        // when an agent technically packs a dotted value into it. We must
        // NOT derive a namespace from it; doing so would surface false
        // positives in JAVA_RULES on agents that emit `code.function = "X.y"`.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        span.attributes
            .push(make_kv("code.function", "OrderService.findItems"));
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].code_function.as_deref(),
            Some("OrderService.findItems")
        );
        assert!(events[0].code_namespace.is_none());
    }

    #[test]
    fn code_attrs_no_dot_in_fq_name() {
        // Bare function name (Rust, C, JS callbacks) has no dot to split on.
        // Namespace stays None.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        span.attributes.push(make_kv("code.function.name", "main"));
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code_function.as_deref(), Some("main"));
        assert!(events[0].code_namespace.is_none());
    }

    #[test]
    fn java_rules_match_via_derived_namespace() {
        // End-to-end: a stable-convention FQ name on a JPA repository span
        // must produce a SpanEvent whose code_namespace triggers JAVA_RULES
        // (via the JPA prefix). Verifies that namespace derivation feeds
        // the suggestion engine correctly.
        let mut span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
        span.attributes.push(make_kv(
            "code.function.name",
            "org.springframework.data.jpa.repository.support.SimpleJpaRepository.findAll",
        ));
        let req = make_request("svc", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        let ns = events[0]
            .code_namespace
            .as_deref()
            .expect("namespace derived from FQ name");
        assert_eq!(
            ns,
            "org.springframework.data.jpa.repository.support.SimpleJpaRepository"
        );
        assert!(ns.contains("org.springframework.data.jpa"));
    }

    // ── OTLP/HTTP handler with gzip decompression ───────────────────

    #[cfg(feature = "daemon")]
    mod http_handler {
        use super::*;
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use prost::Message;
        use std::io::Write;
        use tokio::sync::mpsc;
        use tower::ServiceExt;

        fn build_minimal_request_bytes() -> Vec<u8> {
            let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
            let req = make_request("svc", vec![span]);
            req.encode_to_vec()
        }

        fn gzip(body: &[u8]) -> Vec<u8> {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(body).expect("gzip encode");
            encoder.finish().expect("gzip finish")
        }

        /// Build a POST `/v1/traces` request with `Content-Type:
        /// application/json` and an empty body, used to exercise the
        /// 415 path in tests that focus on the rejection metric (the
        /// body content does not matter, only the wrong content type).
        fn unsupported_media_type_request() -> Request<Body> {
            Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(Vec::<u8>::new()))
                .expect("build request")
        }

        #[tokio::test]
        async fn otlp_http_accepts_gzip_request() {
            let (tx, mut rx) = mpsc::channel(8);
            let router = otlp_http_router(tx, 1_048_576, None);

            let body = build_minimal_request_bytes();
            let gzipped = gzip(&body);
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .header(header::CONTENT_ENCODING, "gzip")
                .body(Body::from(gzipped))
                .expect("build request");

            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::OK);

            let events = rx.recv().await.expect("event batch sent");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].target, "SELECT 1");
        }

        #[tokio::test]
        async fn otlp_http_accepts_uncompressed_request() {
            let (tx, mut rx) = mpsc::channel(8);
            let router = otlp_http_router(tx, 1_048_576, None);

            let body = build_minimal_request_bytes();
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .body(Body::from(body))
                .expect("build request");

            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::OK);

            let events = rx.recv().await.expect("event batch sent");
            assert_eq!(events.len(), 1);
        }

        #[tokio::test]
        async fn otlp_http_rejects_unsupported_encoding() {
            // Brotli is not enabled; tower-http surfaces this as 415.
            let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
            let router = otlp_http_router(tx, 1_048_576, None);

            let body = build_minimal_request_bytes();
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .header(header::CONTENT_ENCODING, "br")
                .body(Body::from(body))
                .expect("build request");

            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        #[tokio::test]
        async fn otlp_http_rejects_oversize_compressed_body() {
            // Compressed body above max_payload_size must be rejected before
            // decompression, bounding attacker decompression CPU work even
            // when operators raise the cap. Realistic clients always send
            // Content-Length, so the layer rejects pre-decompression.
            let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
            let cap = 256_usize;
            let router = otlp_http_router(tx, cap, None);

            let payload: Vec<u8> = vec![0u8; 4096];
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .header(header::CONTENT_LENGTH, payload.len())
                .body(Body::from(payload))
                .expect("build request");

            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        }

        #[tokio::test]
        async fn otlp_http_content_type_check_with_gzip() {
            // Gzip body but JSON Content-Type. The Content-Type guard runs
            // after decompression and must still reject this with 415.
            let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
            let router = otlp_http_router(tx, 1_048_576, None);

            let body = build_minimal_request_bytes();
            let gzipped = gzip(&body);
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_ENCODING, "gzip")
                .body(Body::from(gzipped))
                .expect("build request");

            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        #[tokio::test]
        async fn http_handler_records_unsupported_media_type() {
            let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
            let (metrics, sink) = fresh_metrics_sink();
            let router = otlp_http_router(tx, 1_048_576, Some(sink));

            let response = router
                .oneshot(unsupported_media_type_request())
                .await
                .expect("router runs");
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
            assert_eq!(
                metrics
                    .otlp_rejected_total
                    .with_label_values(&["unsupported_media_type"])
                    .get(),
                1
            );
        }

        #[tokio::test]
        async fn http_handler_records_parse_error() {
            let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
            let (metrics, sink) = fresh_metrics_sink();
            let router = otlp_http_router(tx, 1_048_576, Some(sink));

            // Random bytes are extremely unlikely to be a valid OTLP
            // ExportTraceServiceRequest protobuf. prost decode returns
            // an error and the handler must reject with 400.
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .body(Body::from(vec![0xff_u8, 0xff, 0xff, 0xff, 0xff, 0xff]))
                .expect("build request");
            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                metrics
                    .otlp_rejected_total
                    .with_label_values(&["parse_error"])
                    .get(),
                1
            );
        }

        #[tokio::test]
        async fn http_handler_records_channel_full() {
            // Drop the receiver so any send fails immediately. The
            // handler must reject with 503 and bump the channel_full
            // counter.
            let (tx, rx) = mpsc::channel::<Vec<SpanEvent>>(1);
            drop(rx);
            let (metrics, sink) = fresh_metrics_sink();
            let router = otlp_http_router(tx, 1_048_576, Some(sink));

            let body = build_minimal_request_bytes();
            let req = Request::builder()
                .method("POST")
                .uri("/v1/traces")
                .header(header::CONTENT_TYPE, "application/x-protobuf")
                .body(Body::from(body))
                .expect("build request");
            let response = router.oneshot(req).await.expect("router runs");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                metrics
                    .otlp_rejected_total
                    .with_label_values(&["channel_full"])
                    .get(),
                1
            );
        }

        #[tokio::test]
        async fn http_handler_no_metrics_state_does_not_panic() {
            // None metrics state must not panic at any rejection site.
            let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(8);
            let router = otlp_http_router(tx, 1_048_576, None);

            let response = router
                .oneshot(unsupported_media_type_request())
                .await
                .expect("router runs");
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        #[tokio::test]
        async fn grpc_handler_records_channel_full() {
            use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;

            let (tx, rx) = mpsc::channel::<Vec<SpanEvent>>(1);
            drop(rx);
            let metrics = Arc::new(MetricsState::new());
            let svc = OtlpGrpcService::new(tx, Some(metrics.clone()));

            let span = make_sql_span(&[1; 16], &[2; 8], &[], "SELECT 1", 0, 1_000_000);
            let req = tonic::Request::new(make_request("svc", vec![span]));
            let result = svc.export(req).await;
            assert!(result.is_err());
            assert_eq!(
                metrics
                    .otlp_rejected_total
                    .with_label_values(&["channel_full"])
                    .get(),
                1
            );
        }
    }
}
