//! Shared test helpers for sentinel-core unit tests.

use crate::correlate::Trace;
use crate::event::{EventSource, EventType, SpanEvent};
use crate::normalize;

pub fn make_sql_event(trace_id: &str, span_id: &str, target: &str, ts: &str) -> SpanEvent {
    make_sql_event_with_duration(trace_id, span_id, target, ts, 800)
}

pub fn make_http_event(trace_id: &str, span_id: &str, target: &str, ts: &str) -> SpanEvent {
    make_http_event_with_duration(trace_id, span_id, target, ts, 12000)
}

pub fn make_sql_event_with_duration(
    trace_id: &str,
    span_id: &str,
    target: &str,
    ts: &str,
    duration_us: u64,
) -> SpanEvent {
    SpanEvent {
        timestamp: ts.to_string(),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        service: "order-svc".to_string(),
        cloud_region: None,
        event_type: EventType::Sql,
        operation: "SELECT".to_string(),
        target: target.to_string(),
        duration_us,
        source: EventSource {
            endpoint: "POST /api/orders/42/submit".to_string(),
            method: "OrderService::create_order".to_string(),
        },
        status_code: None,
        response_size_bytes: None,
        code_function: None,
        code_filepath: None,
        code_lineno: None,
        code_namespace: None,
    }
}

pub fn make_http_event_with_duration(
    trace_id: &str,
    span_id: &str,
    target: &str,
    ts: &str,
    duration_us: u64,
) -> SpanEvent {
    SpanEvent {
        timestamp: ts.to_string(),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        service: "order-svc".to_string(),
        cloud_region: None,
        event_type: EventType::HttpOut,
        operation: "GET".to_string(),
        target: target.to_string(),
        duration_us,
        source: EventSource {
            endpoint: "POST /api/orders/42/submit".to_string(),
            method: "OrderService::create_order".to_string(),
        },
        status_code: Some(200),
        response_size_bytes: None,
        code_function: None,
        code_filepath: None,
        code_lineno: None,
        code_namespace: None,
    }
}

pub fn make_http_event_with_size(
    trace_id: &str,
    span_id: &str,
    target: &str,
    ts: &str,
    response_size_bytes: Option<u64>,
) -> SpanEvent {
    let mut event = make_http_event(trace_id, span_id, target, ts);
    event.response_size_bytes = response_size_bytes;
    event
}

/// Build 3 identical SQL events that trigger a redundant finding
/// (same template AND same params, i.e. exact duplicates).
pub fn make_redundant_events() -> Vec<SpanEvent> {
    (1..=3_i32)
        .map(|i| {
            make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                "SELECT * FROM order_item WHERE order_id = 42",
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            )
        })
        .collect()
}

/// Build `count` SQL events with the same template and different
/// `order_id` params, spaced `stride_ms` milliseconds apart starting
/// from `14:32:01`. Used to construct N+1-style test fixtures with
/// arbitrary cardinality and timing.
pub fn make_sql_series_events_with_stride(count: i32, stride_ms: i32) -> Vec<SpanEvent> {
    (1..=count)
        .map(|i| {
            make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * stride_ms),
            )
        })
        .collect()
}

/// Build `count` SQL events with a default 50ms stride. Shortcut for
/// tests that don't need a specific stride.
pub fn make_sql_series_events(count: i32) -> Vec<SpanEvent> {
    make_sql_series_events_with_stride(count, 50)
}

/// Build 6 SQL events that trigger an N+1 finding (same template,
/// different `order_id` params, within the default 500ms window).
/// Reused across pipeline, score, and `quality_gate` tests.
pub fn make_n_plus_one_events() -> Vec<SpanEvent> {
    make_sql_series_events(6)
}

/// Build a minimal `Finding` with the given type and severity.
/// All other fields use sensible defaults. Tests that need specific
/// values (e.g. a different template or `trace_id`) can mutate the
/// returned struct.
pub fn make_finding(
    finding_type: crate::detect::FindingType,
    severity: crate::detect::Severity,
) -> crate::detect::Finding {
    crate::detect::Finding {
        finding_type,
        severity,
        trace_id: "trace-1".to_string(),
        service: "order-svc".to_string(),
        source_endpoint: "POST /api/orders/42/submit".to_string(),
        pattern: crate::detect::Pattern {
            template: "SELECT * FROM t WHERE id = ?".to_string(),
            occurrences: 6,
            window_ms: 200,
            distinct_params: 6,
        },
        suggestion: "batch".to_string(),
        first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
        green_impact: Some(crate::detect::GreenImpact {
            estimated_extra_io_ops: 5,
            io_intensity_score: 6.0,
            io_intensity_band: crate::report::interpret::InterpretationLevel::for_iis(6.0),
        }),
        confidence: crate::detect::Confidence::default(),
        code_location: None,
    }
}

pub fn make_trace(events: Vec<SpanEvent>) -> Trace {
    assert!(!events.is_empty(), "make_trace requires at least one event");
    let trace_id = events[0].trace_id.clone();
    let spans = normalize::normalize_all(events);
    Trace { trace_id, spans }
}

// ---------------------------------------------------------------
// Mock HTTP server helpers for scraper and API-client tests
// ---------------------------------------------------------------
//
// Every scraper/ingest module that does network I/O (scaphandre,
// cloud_energy, electricity_maps, tempo) needs to test its HTTP
// round-trips against a local mock. These helpers give them a single
// implementation of the "bind ephemeral TCP port, accept one
// connection, write a canned response, half-close" dance.
//
// The server is hand-rolled (raw `tokio::net::TcpListener` + raw
// HTTP/1.1 response bytes) to avoid pulling in wiremock/httptest as
// dev-deps. That's also why the helpers live here rather than in each
// module's `tests.rs`: Qodana was flagging the 4-way duplicate.

/// Bind an ephemeral TCP port on `127.0.0.1`, spawn a one-shot server
/// that writes `response_body` verbatim on the first accepted
/// connection, and return `(endpoint_url, server_join_handle)`.
///
/// Callers must `.await` the returned `JoinHandle` after driving the
/// client side so the assertions inside the server task (`unwrap()`
/// etc.) propagate their failures.
///
/// `response_body` is `Vec<u8>` so binary protocols (OTLP protobuf)
/// can use the same helper as text/JSON.
pub async fn spawn_one_shot_server(
    response_body: Vec<u8>,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Drain the request headers so the client doesn't see a reset.
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let _ = socket.write_all(&response_body).await;
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });
    (endpoint, handle)
}

/// Build an HTTP/1.1 200 OK response with a text body and the given
/// `Content-Type`. Used for JSON and Prometheus scrape responses.
#[must_use]
pub fn http_200_text(content_type: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// Build an HTTP/1.1 200 OK response with a binary body. Used for
/// OTLP protobuf responses (Tempo fetch).
#[must_use]
pub fn http_200_bytes(content_type: &str, body: &[u8]) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    let mut out = headers.into_bytes();
    out.extend_from_slice(body);
    out
}

/// Build an HTTP/1.1 status-only response (empty body). Used for 4xx
/// and 5xx error-path tests.
#[must_use]
pub fn http_status(code: u16, reason: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n"
    )
    .into_bytes()
}
