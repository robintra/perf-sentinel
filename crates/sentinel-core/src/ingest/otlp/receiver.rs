//! OTLP network receivers: the gRPC `TraceService` and the axum HTTP router.

use std::sync::Arc;

use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use tonic::{Request, Response, Status, async_trait};

#[cfg(feature = "daemon")]
use super::source_endpoint_updates;
use super::{MetricsSink, convert_otlp_request_counted_with_grouping};
use crate::event::SpanEvent;
use crate::report::metrics::OtlpRejectReason;

// ── gRPC service implementation ─────────────────────────────────────

/// Bounded wait when enqueueing a converted batch on the ingest channel.
/// Short bursts absorb silently. Sustained saturation surfaces as a fast
/// retryable rejection that moves the `channel_full` counter. A plain
/// `send().await` only errors on a closed channel, so saturation would
/// otherwise park senders until the router request timeout with no
/// rejection ever counted.
const INGEST_ENQUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Where a decoded OTLP request goes, shared by both transports.
///
/// `Events` is the daemon path: convert to [`SpanEvent`]s and feed the
/// pipeline. `Raw` is the capture path: hand the request over untouched so
/// [`crate::capture`] can write it back as an OTLP/JSON trace file. Capture
/// must not go through the conversion, which drops non-I/O spans and masks
/// fields, or the file would no longer describe what the application sent.
#[derive(Clone)]
pub enum OtlpSink {
    Events(tokio::sync::mpsc::Sender<Vec<SpanEvent>>),
    Raw(tokio::sync::mpsc::Sender<ExportTraceServiceRequest>),
}

#[derive(Clone)]
enum ReceiverSink {
    Standard(OtlpSink),
    #[cfg(feature = "daemon")]
    Daemon(tokio::sync::mpsc::Sender<crate::daemon::IngestBatch>),
}

/// Why a request could not be handed over, payload dropped so both sink arms
/// answer with one type. `Full` is retryable, `Closed` means shutdown.
pub(crate) enum SinkRejection {
    Full,
    Closed,
}

impl<T> From<tokio::sync::mpsc::error::SendTimeoutError<T>> for SinkRejection {
    fn from(e: tokio::sync::mpsc::error::SendTimeoutError<T>) -> Self {
        match e {
            tokio::sync::mpsc::error::SendTimeoutError::Timeout(_) => Self::Full,
            tokio::sync::mpsc::error::SendTimeoutError::Closed(_) => Self::Closed,
        }
    }
}

impl OtlpSink {
    /// Enqueue one request, converting first on the `Events` arm. An empty
    /// conversion is a success with nothing sent.
    async fn accept(
        &self,
        request: ExportTraceServiceRequest,
        metrics: Option<&Arc<dyn MetricsSink>>,
        grouping_attributes: Option<&[Arc<str>]>,
    ) -> Result<(), SinkRejection> {
        match self {
            Self::Events(tx) => {
                let (events, stats) =
                    convert_otlp_request_counted_with_grouping(&request, grouping_attributes);
                if let Some(m) = metrics {
                    m.record_otlp_spans(stats);
                }
                if events.is_empty() {
                    return Ok(());
                }
                Ok(tx.send_timeout(events, INGEST_ENQUEUE_TIMEOUT).await?)
            }
            Self::Raw(tx) => Ok(tx.send_timeout(request, INGEST_ENQUEUE_TIMEOUT).await?),
        }
    }
}

impl ReceiverSink {
    async fn accept(
        &self,
        request: ExportTraceServiceRequest,
        metrics: Option<&Arc<dyn MetricsSink>>,
        grouping_attributes: Option<&[Arc<str>]>,
    ) -> Result<(), SinkRejection> {
        match self {
            Self::Standard(sink) => sink.accept(request, metrics, grouping_attributes).await,
            #[cfg(feature = "daemon")]
            Self::Daemon(tx) => {
                let source_endpoint_updates = source_endpoint_updates(&request);
                let (events, stats) =
                    convert_otlp_request_counted_with_grouping(&request, grouping_attributes);
                if let Some(m) = metrics {
                    m.record_otlp_spans(stats);
                }
                if events.is_empty() && source_endpoint_updates.is_empty() {
                    return Ok(());
                }
                Ok(tx
                    .send_timeout(
                        crate::daemon::IngestBatch {
                            events,
                            source_endpoint_updates,
                        },
                        INGEST_ENQUEUE_TIMEOUT,
                    )
                    .await?)
            }
        }
    }
}

/// OTLP gRPC trace service that converts spans and sends them through a channel.
pub struct OtlpGrpcService {
    sink: ReceiverSink,
    metrics: Option<Arc<dyn MetricsSink>>,
    grouping_attributes: Option<Arc<[Arc<str>]>>,
}

impl OtlpGrpcService {
    #[must_use]
    pub fn new(
        sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
        metrics: Option<Arc<dyn MetricsSink>>,
    ) -> Self {
        Self {
            sink: ReceiverSink::Standard(OtlpSink::Events(sender)),
            metrics,
            grouping_attributes: None,
        }
    }

    /// Same service, using the operator-configured grouping attributes.
    #[must_use]
    pub fn new_with_grouping(
        sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
        metrics: Option<Arc<dyn MetricsSink>>,
        grouping_attributes: Vec<Arc<str>>,
    ) -> Self {
        Self {
            sink: ReceiverSink::Standard(OtlpSink::Events(sender)),
            metrics,
            grouping_attributes: Some(grouping_attributes.into()),
        }
    }

    /// Same service, feeding a capture sink instead of the pipeline.
    #[must_use]
    pub fn new_raw(
        sender: tokio::sync::mpsc::Sender<ExportTraceServiceRequest>,
        metrics: Option<Arc<dyn MetricsSink>>,
    ) -> Self {
        Self {
            sink: ReceiverSink::Standard(OtlpSink::Raw(sender)),
            metrics,
            grouping_attributes: None,
        }
    }

    #[cfg(feature = "daemon")]
    pub(crate) fn new_daemon_with_grouping(
        sender: tokio::sync::mpsc::Sender<crate::daemon::IngestBatch>,
        metrics: Option<Arc<dyn MetricsSink>>,
        grouping_attributes: Vec<Arc<str>>,
    ) -> Self {
        Self {
            sink: ReceiverSink::Daemon(sender),
            metrics,
            grouping_attributes: Some(grouping_attributes.into()),
        }
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
        // Memory-pressure admission control, repeated at the handler level:
        // the daemon wraps this service in a tonic interceptor that rejects
        // before the message is decoded (see
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
        if let Err(e) = self
            .sink
            .accept(
                request.into_inner(),
                self.metrics.as_ref(),
                self.grouping_attributes.as_deref(),
            )
            .await
        {
            if let Some(m) = self.metrics.as_ref() {
                m.record_otlp_reject(OtlpRejectReason::ChannelFull);
            }
            // Saturation must map to a status the OTLP spec lists as
            // retryable (UNAVAILABLE). INTERNAL is non-retryable and
            // would make compliant exporters drop the batch for good.
            // A closed channel means shutdown: INTERNAL is accurate.
            return Err(match e {
                SinkRejection::Full => Status::unavailable("ingest queue full, retry"),
                SinkRejection::Closed => Status::internal("event channel closed"),
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
/// Cloned on every request by axum's `State` extractor. The sender and
/// metrics handle are both cheap to clone (mpsc Sender is an Arc, the
/// metrics Option carries an Arc).
#[derive(Clone)]
struct OtlpHttpState {
    sink: ReceiverSink,
    metrics: Option<Arc<dyn MetricsSink>>,
    grouping_attributes: Option<Arc<[Arc<str>]>>,
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
    otlp_http_router_with_sink_and_grouping(
        ReceiverSink::Standard(OtlpSink::Events(sender)),
        max_payload_size,
        metrics,
        None,
    )
}

/// Build an OTLP HTTP router using the operator-configured grouping attributes.
pub fn otlp_http_router_with_grouping(
    sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
    max_payload_size: usize,
    metrics: Option<Arc<dyn MetricsSink>>,
    grouping_attributes: Vec<Arc<str>>,
) -> axum::Router {
    otlp_http_router_with_sink_and_grouping(
        ReceiverSink::Standard(OtlpSink::Events(sender)),
        max_payload_size,
        metrics,
        Some(grouping_attributes.into()),
    )
}

/// Same router, against an explicit sink. `OtlpSink::Raw` is what
/// [`crate::capture`] mounts so requests reach the trace file unconverted.
pub fn otlp_http_router_with_sink(
    sink: OtlpSink,
    max_payload_size: usize,
    metrics: Option<Arc<dyn MetricsSink>>,
) -> axum::Router {
    otlp_http_router_with_sink_and_grouping(
        ReceiverSink::Standard(sink),
        max_payload_size,
        metrics,
        None,
    )
}

#[cfg(feature = "daemon")]
pub(crate) fn otlp_http_router_for_daemon(
    sender: tokio::sync::mpsc::Sender<crate::daemon::IngestBatch>,
    max_payload_size: usize,
    metrics: Option<Arc<dyn MetricsSink>>,
    grouping_attributes: Vec<Arc<str>>,
) -> axum::Router {
    otlp_http_router_with_sink_and_grouping(
        ReceiverSink::Daemon(sender),
        max_payload_size,
        metrics,
        Some(grouping_attributes.into()),
    )
}

fn otlp_http_router_with_sink_and_grouping(
    sink: ReceiverSink,
    max_payload_size: usize,
    metrics: Option<Arc<dyn MetricsSink>>,
    grouping_attributes: Option<Arc<[Arc<str>]>>,
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
        // Memory-pressure admission control, repeated at the handler level:
        // the outermost `memory_pressure_guard` middleware already rejects
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
        if state
            .sink
            .accept(
                request,
                state.metrics.as_ref(),
                state.grouping_attributes.as_deref(),
            )
            .await
            .is_err()
        {
            tracing::warn!("OTLP HTTP: ingest channel full or closed, dropping request");
            reject(OtlpRejectReason::ChannelFull);
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        StatusCode::OK
    }

    // Hard cap on concurrently processed OTLP HTTP requests, bounding
    // decode CPU and buffered-body memory under a saturation flood.
    // Without it the kubelet liveness probe on /health starves behind
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

    let state = OtlpHttpState {
        sink,
        metrics,
        grouping_attributes,
    };
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

/// Mount an [`OtlpGrpcService`] with the encodings and the decode cap every
/// gRPC listener shares. Divergence here breaks one transport silently: a
/// listener without gzip drops every batch from a default Collector. The cap
/// applies to the decompressed message.
#[cfg(feature = "daemon")]
#[must_use]
pub fn trace_service(
    service: OtlpGrpcService,
    max_payload: usize,
) -> opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer<
    OtlpGrpcService,
> {
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer;
    TraceServiceServer::new(service)
        .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
        .accept_compressed(tonic::codec::CompressionEncoding::Deflate)
        .max_decoding_message_size(max_payload)
}

/// A tonic server with the resource caps every gRPC listener shares: a
/// request timeout, an HTTP/2 stream cap doubling as the per-connection
/// concurrency limit, and a process-wide in-flight request limit. Only
/// the numbers differ between capture and the daemon, so the shape stays
/// here rather than drifting apart in two call sites.
#[cfg(feature = "daemon")]
#[must_use]
pub(crate) fn hardened_grpc_server(
    max_concurrent_streams: u32,
    max_concurrent_requests: usize,
) -> tonic::transport::Server<
    tower::layer::util::Stack<
        tower::limit::GlobalConcurrencyLimitLayer,
        tower::layer::util::Identity,
    >,
> {
    tonic::transport::Server::builder()
        .timeout(std::time::Duration::from_mins(1))
        .max_concurrent_streams(Some(max_concurrent_streams))
        .concurrency_limit_per_connection(max_concurrent_streams as usize)
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(
            max_concurrent_requests,
        ))
}
