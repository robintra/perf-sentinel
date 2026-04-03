//! OTLP ingestion: maps OpenTelemetry spans to `SpanEvent`.
//!
//! Supports both gRPC (tonic `TraceService`) and HTTP (axum handler) ingestion.
//! Uses the `opentelemetry-proto` crate for protobuf definitions.

use std::collections::HashMap;

use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{KeyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::Span;

use crate::event::{EventSource, EventType, SpanEvent};

// ── Conversion helpers ──────────────────────────────────────────────

/// Convert bytes to a lowercase hex string using a lookup table.
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        buf.push(HEX[(b >> 4) as usize]);
        buf.push(HEX[(b & 0x0f) as usize]);
    }
    // SAFETY: all pushed bytes come from HEX which only contains ASCII hex digits (0-9, a-f).
    // The output is guaranteed valid UTF-8 because every byte is in the ASCII range.
    unsafe { String::from_utf8_unchecked(buf) }
}

use crate::time::nanos_to_iso8601;

/// Extract a string attribute value from OTLP `KeyValue` pairs.
fn get_str_attribute<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attrs.iter().find(|kv| kv.key == key).and_then(|kv| {
        kv.value.as_ref().and_then(|v| match &v.value {
            Some(any_value::Value::StringValue(s)) => Some(s.as_str()),
            _ => None,
        })
    })
}

/// Extract an integer attribute value from OTLP `KeyValue` pairs.
fn get_int_attribute(attrs: &[KeyValue], key: &str) -> Option<i64> {
    attrs.iter().find(|kv| kv.key == key).and_then(|kv| {
        kv.value.as_ref().and_then(|v| match &v.value {
            Some(any_value::Value::IntValue(i)) => Some(*i),
            _ => None,
        })
    })
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
#[must_use]
pub fn convert_otlp_request(request: &ExportTraceServiceRequest) -> Vec<SpanEvent> {
    let mut events = Vec::new();

    for resource_spans in &request.resource_spans {
        // Extract service.name from resource attributes
        let service_name = resource_spans
            .resource
            .as_ref()
            .and_then(|r| get_str_attribute(&r.attributes, "service.name"))
            .unwrap_or("unknown")
            .to_string();

        // Build a span index for parent lookup within this resource
        let mut span_index: HashMap<&[u8], &Span> = HashMap::new();
        for scope_spans in &resource_spans.scope_spans {
            for span in &scope_spans.spans {
                span_index.insert(&span.span_id, span);
            }
        }

        // Process each span
        for scope_spans in &resource_spans.scope_spans {
            for span in &scope_spans.spans {
                if let Some(event) = convert_span(span, &service_name, &span_index) {
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
    service_name: &str,
    span_index: &HashMap<&[u8], &Span>,
) -> Option<SpanEvent> {
    let attrs = &span.attributes;

    // Determine event type: SQL if db.statement/db.query.text present, HTTP if http.url/url.full present.
    // Supports both legacy (pre-1.21) and stable (1.21+) OTel semantic conventions.
    let (event_type, target, operation) = if let Some(statement) =
        get_str_attribute(attrs, "db.statement")
            .or_else(|| get_str_attribute(attrs, "db.query.text"))
    {
        let op = get_str_attribute(attrs, "db.system")
            .unwrap_or("sql")
            .to_string();
        (EventType::Sql, statement.to_string(), op)
    } else if let Some(url) =
        get_str_attribute(attrs, "http.url").or_else(|| get_str_attribute(attrs, "url.full"))
    {
        let method = get_str_attribute(attrs, "http.method")
            .or_else(|| get_str_attribute(attrs, "http.request.method"))
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
        get_int_attribute(attrs, "http.status_code")
            .or_else(|| get_int_attribute(attrs, "http.response.status_code"))
            .and_then(|c| u16::try_from(c).ok())
    } else {
        None
    };

    // Parent span lookup for source endpoint/method
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

    Some(SpanEvent {
        timestamp,
        trace_id,
        span_id,
        parent_span_id,
        service: service_name.to_string(),
        event_type,
        operation,
        target,
        duration_us,
        source: EventSource {
            endpoint: source_endpoint,
            method: source_method,
        },
        status_code,
    })
}

// ── gRPC service implementation ─────────────────────────────────────

/// OTLP gRPC trace service that converts spans and sends them through a channel.
pub struct OtlpGrpcService {
    sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
}

impl OtlpGrpcService {
    #[must_use]
    pub const fn new(sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>) -> Self {
        Self { sender }
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
        if !events.is_empty() {
            self.sender
                .send(events)
                .await
                .map_err(|_| tonic::Status::internal("event channel closed"))?;
        }
        Ok(tonic::Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

// ── HTTP handler (axum) ─────────────────────────────────────────────

/// Build an axum router for OTLP HTTP ingestion.
///
/// Accepts `POST /v1/traces` with protobuf-encoded `ExportTraceServiceRequest`.
pub fn otlp_http_router(
    sender: tokio::sync::mpsc::Sender<Vec<SpanEvent>>,
    max_payload_size: usize,
) -> axum::Router {
    use axum::{Router, extract::State, http::StatusCode, routing::post};

    async fn handle_traces(
        State(sender): State<tokio::sync::mpsc::Sender<Vec<SpanEvent>>>,
        body: axum::body::Bytes,
    ) -> StatusCode {
        let request: ExportTraceServiceRequest = match prost::Message::decode(body.as_ref()) {
            Ok(req) => req,
            Err(_) => return StatusCode::BAD_REQUEST,
        };
        let events = convert_otlp_request(&request);
        if !events.is_empty() && sender.send(events).await.is_err() {
            tracing::warn!("OTLP HTTP: event channel full or closed, dropping events");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        StatusCode::OK
    }

    Router::new()
        .route("/v1/traces", post(handle_traces))
        .with_state(sender)
        .layer(axum::extract::DefaultBodyLimit::max(max_payload_size))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::AnyValue;
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans};

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
                make_kv("code.function", "GameService::start_game"),
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
            "SELECT * FROM player WHERE game_id = 42",
            1_720_621_921_000_000_000, // 2024-07-10T14:32:01.000Z
            1_720_621_921_001_200_000, // +1.2ms
        );
        let req = make_request("game", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Sql);
        assert_eq!(events[0].operation, "postgresql");
        assert_eq!(events[0].target, "SELECT * FROM player WHERE game_id = 42");
        assert_eq!(events[0].service, "game");
        assert_eq!(events[0].duration_us, 1200);
        assert!(events[0].status_code.is_none());
    }

    #[test]
    fn http_span_maps_correctly() {
        let span = make_http_span(
            &[1; 16],
            &[3; 8],
            &[],
            "http://account-svc:5000/api/account/123",
            "GET",
            200,
            1_720_621_921_000_000_000,
            1_720_621_921_015_000_000, // +15ms
        );
        let req = make_request("game", vec![span]);
        let events = convert_otlp_request(&req);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::HttpOut);
        assert_eq!(events[0].operation, "GET");
        assert_eq!(events[0].target, "http://account-svc:5000/api/account/123");
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
        let req = make_request("game", vec![span]);
        assert!(convert_otlp_request(&req).is_empty());
    }

    #[test]
    fn parent_span_provides_source_endpoint() {
        let parent = make_parent_span(&[10; 8], "POST /api/game/{id}/start");
        let child = make_sql_span(
            &[1; 16],
            &[20; 8],
            &[10; 8], // parent_span_id
            "SELECT * FROM player WHERE game_id = 42",
            1_720_621_921_000_000_000,
            1_720_621_921_001_200_000,
        );
        let req = make_request("game", vec![parent, child]);
        let events = convert_otlp_request(&req);

        // Only the child (SQL) should produce an event
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source.endpoint, "POST /api/game/{id}/start");
        assert_eq!(events[0].source.method, "GameService::start_game");
    }

    #[test]
    fn missing_parent_falls_back() {
        let child = make_sql_span(
            &[1; 16],
            &[20; 8],
            &[99; 8], // parent not in batch
            "SELECT * FROM player WHERE game_id = 42",
            1_720_621_921_000_000_000,
            1_720_621_921_001_200_000,
        );
        let req = make_request("game", vec![child]);
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
        assert_eq!(events[0].service, "my-service");
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
}
