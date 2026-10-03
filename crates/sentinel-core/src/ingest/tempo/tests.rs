use super::*;

#[tokio::test]
async fn an_overrun_body_names_the_cap_and_anything_else_stays_generic() {
    // Through the real path: LengthLimitError is non-exhaustive and
    // cannot be built, only provoked.
    let body = http_body_util::Full::new(bytes::Bytes::from(vec![0u8; 100]));
    let limited = http_body_util::Limited::new(body, 10);
    let error = http_body_util::BodyExt::collect(limited)
        .await
        .expect_err("a 100 byte body over a 10 byte cap must fail");
    let overrun = classify_body_error(&*error, 4096, "lower --max-traces");
    assert!(matches!(
        overrun,
        TempoError::BodyTooLarge { limit: 4096, .. }
    ));
    // The wording must name the cap as a limit this client imposes, so
    // operators do not look for it in Tempo. The search remedy must not
    // leak onto a single-trace fetch where the flag cannot shrink anything.
    let text = overrun.to_string();
    assert!(text.contains("4096"), "{text}");
    assert!(text.contains("not of the backend"), "{text}");
    assert!(text.contains("lower --max-traces"), "{text}");
    let trace_text =
        classify_body_error(&*error, 4096, crate::ingest::TRACE_OVERRUN_REMEDY).to_string();
    assert!(trace_text.contains("cannot shrink it"), "{trace_text}");

    let other = classify_body_error(&std::io::Error::other("socket closed"), 4096, "unused");
    assert!(matches!(other, TempoError::BodyRead(_)));
}

/// String `KeyValue`, the shape every OTLP fixture in this module needs.
fn kv(key: &str, value: &str) -> opentelemetry_proto::tonic::common::v1::KeyValue {
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
        ..Default::default()
    }
}
use core::assert_matches;

// --- Search response parsing ---

#[test]
fn parse_search_response() {
    let json = r#"{"traces":[{"traceID":"abc123"},{"traceID":"def456"}]}"#;
    let response: SearchResponse = serde_json::from_str(json).unwrap();
    assert_eq!(response.traces.len(), 2);
    assert_eq!(response.traces[0].trace_id, "abc123");
    assert_eq!(response.traces[1].trace_id, "def456");
}

#[test]
fn parse_search_response_empty() {
    let json = r#"{"traces":[]}"#;
    let response: SearchResponse = serde_json::from_str(json).unwrap();
    assert!(response.traces.is_empty());
}

#[test]
fn parse_search_response_missing_traces() {
    let json = r"{}";
    let response: SearchResponse = serde_json::from_str(json).unwrap();
    assert!(response.traces.is_empty());
}

// --- Protobuf decode round-trip ---

#[test]
fn protobuf_decode_empty_request() {
    let request = ExportTraceServiceRequest {
        resource_spans: vec![],
    };
    let mut buf = Vec::new();
    request.encode(&mut buf).unwrap();

    let decoded = ExportTraceServiceRequest::decode(bytes::Bytes::from(buf)).unwrap();
    let events = crate::ingest::otlp::convert_otlp_request(&decoded);
    assert!(events.is_empty());
}

// ---------------------------------------------------------------
// Integration tests with a mock Tempo HTTP server
// ---------------------------------------------------------------
//
// The mock server helpers live in `crate::test_helpers` and are
// shared with scaphandre, cloud_energy, and electricity_maps
// tests. The mock serves one response per accepted connection,
// which matches the one-shot nature of each Tempo API call.

use crate::test_helpers::{
    http_200_bytes, http_200_text, http_status, spawn_capture_server, spawn_one_shot_server,
};

/// Wrap the shared `http_200_text` with the JSON content type.
fn http_200_json(body: &str) -> Vec<u8> {
    http_200_text("application/json", body)
}

/// Wrap the shared `http_200_bytes` with the protobuf content type.
fn http_200_proto(body: &[u8]) -> Vec<u8> {
    http_200_bytes("application/protobuf", body)
}

// --- ingest_from_tempo endpoint validation ---

#[tokio::test]
async fn ingest_from_tempo_rejects_non_http_scheme() {
    let err = ingest_from_tempo(
        "ftp://tempo.local",
        Some("foo-svc"),
        None,
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await
    .expect_err("non-http must be rejected");
    match err {
        TempoError::InvalidEndpoint(msg) => assert!(msg.contains("http://")),
        other => panic!("expected InvalidEndpoint, got {other:?}"),
    }
}

#[tokio::test]
async fn ingest_from_tempo_rejects_credentials_in_endpoint() {
    let err = ingest_from_tempo(
        "http://user:pass@tempo.local",
        None,
        Some("abc"),
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await
    .expect_err("credentials must be rejected");
    match err {
        TempoError::InvalidEndpoint(msg) => assert!(msg.contains("credentials")),
        other => panic!("expected InvalidEndpoint, got {other:?}"),
    }
}

#[tokio::test]
async fn ingest_from_tempo_rejects_missing_service_and_trace_id() {
    // Neither trace_id nor service supplied, must error.
    let err = ingest_from_tempo(
        "http://tempo.local",
        None,
        None,
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await
    .expect_err("missing both must be rejected");
    match err {
        TempoError::InvalidEndpoint(msg) => {
            assert!(msg.contains("trace-id") || msg.contains("service"));
        }
        other => panic!("expected InvalidEndpoint, got {other:?}"),
    }
}

#[tokio::test]
async fn ingest_from_tempo_accepts_percent_encoded_at_in_query_string() {
    // Regression guard: the endpoint validator must only reject `@`
    // in the authority section, not in the path or query. A URI
    // like `http://tempo.local/api/traces?owner=foo%40example.com`
    // contains a literal `@` in the query string, after the
    // authority, and should be accepted. The validator strips
    // the scheme, then looks at the slice BEFORE the first `/` or
    // `?`, so the authority is `tempo.local` and the `%40` lives
    // in the query-string-only part.
    //
    // This test uses an unreachable endpoint. We don't care
    // whether the fetch succeeds, only that the validator does
    // NOT synchronously return `InvalidEndpoint`. Any other error
    // (transport, timeout, etc.) is acceptable.
    let result = ingest_from_tempo(
        "http://127.0.0.1:1/api/traces?owner=foo%40example.com",
        None,
        Some("abc123"),
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await;
    match result {
        Err(TempoError::InvalidEndpoint(msg)) if msg.contains("credentials") => {
            panic!("validator must not reject `@` in the query string");
        }
        _ => {} // transport / timeout / anything else is fine
    }
}

// --- fetch_trace: hex validation and happy path ---

#[tokio::test]
async fn fetch_trace_rejects_non_hex_trace_id() {
    let client = http_client::build_client();
    let err = fetch_trace(&client, "http://tempo.local", "not-hex-id!", None)
        .await
        .expect_err("non-hex must be rejected");
    match err {
        TempoError::InvalidEndpoint(msg) => assert!(msg.contains("non-hex")),
        other => panic!("expected InvalidEndpoint, got {other:?}"),
    }
}

#[tokio::test]
async fn fetch_trace_decodes_empty_otlp_request() {
    // Send an empty but valid OTLP protobuf. fetch_trace should
    // decode it into an empty Vec<SpanEvent> without error.
    let request = ExportTraceServiceRequest {
        resource_spans: vec![],
    };
    let mut buf = Vec::new();
    request.encode(&mut buf).unwrap();

    let (endpoint, server) = spawn_one_shot_server(http_200_proto(&buf)).await;
    let client = http_client::build_client();
    let events = fetch_trace(&client, &endpoint, "abc123def456", None)
        .await
        .expect("valid OTLP must decode");
    assert!(events.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_trace_uses_configured_grouping_attributes() {
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![kv("service.name", "svc"), kv("tenant.id", "acme")],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: vec![1; 16],
                    span_id: vec![2; 8],
                    name: "db.query".to_string(),
                    start_time_unix_nano: 1,
                    end_time_unix_nano: 2,
                    attributes: vec![kv("db.statement", "SELECT 1")],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let (endpoint, server) = spawn_one_shot_server(http_200_proto(&request.encode_to_vec())).await;
    let client = http_client::build_client();
    let grouping = [Arc::from("tenant.id")];

    let (events, received) =
        fetch_trace_with_grouping(&client, &endpoint, "abc123def456", None, Some(&grouping))
            .await
            .expect("valid OTLP must decode");
    assert!(received > 0, "the span tally must reach the caller");
    assert_eq!(events[0].grouping[0].key.as_ref(), "tenant.id");
    assert_eq!(events[0].grouping[0].value.as_ref(), "acme");
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_trace_surfaces_404_as_trace_not_found() {
    let (endpoint, server) = spawn_one_shot_server(http_status(404, "Not Found")).await;
    let client = http_client::build_client();
    let err = fetch_trace(&client, &endpoint, "abc123", None)
        .await
        .expect_err("404 must surface as TraceNotFound");
    assert_matches!(err, TempoError::TraceNotFound(_));
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_trace_surfaces_500_as_http_status() {
    let (endpoint, server) = spawn_one_shot_server(http_status(500, "Internal")).await;
    let client = http_client::build_client();
    let err = fetch_trace(&client, &endpoint, "abc123", None)
        .await
        .expect_err("500 must surface as HttpStatus");
    match err {
        TempoError::HttpStatus { status: 500, .. } => {}
        other => panic!("expected HttpStatus {{ status: 500, .. }}, got {other:?}"),
    }
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_trace_rejects_malformed_protobuf() {
    let garbage = http_200_proto(b"\xff\xff\xff\xff\xff\xff\xff\xff");
    let (endpoint, server) = spawn_one_shot_server(garbage).await;

    let client = http_client::build_client();
    let err = fetch_trace(&client, &endpoint, "abc123", None)
        .await
        .expect_err("malformed protobuf must surface as ProtobufDecode");
    assert_matches!(err, TempoError::ProtobufDecode(_));
    server.await.unwrap();
}

// --- Body-cap overruns on both paths ---
//
// `MAX_SEARCH_BODY_BYTES` (16 MiB) and `MAX_TRACE_BODY_BYTES`
// (64 MiB) are compiled in, so these go through the private fetch
// helpers with a tiny cap instead of serving the real one. The
// constants themselves are guarded by the `const _: () = assert!`
// above. Only these two tests check that an overrun on the wire
// reaches the caller as `BodyTooLarge` rather than a parse or read
// error, and that each path binds its own remedy.

#[tokio::test]
async fn a_search_body_over_the_cap_carries_the_search_remedy() {
    let (endpoint, server) = spawn_one_shot_server(http_200_json(&"x".repeat(256))).await;
    let client = http_client::build_client();
    let uri: hyper::Uri = format!("{endpoint}/api/search").parse().unwrap();

    let err = fetch_json(&client, uri, 64, None)
        .await
        .expect_err("a 256 byte body over a 64 byte cap must fail");
    match err {
        TempoError::BodyTooLarge { limit: 64, remedy } => {
            assert_eq!(remedy, crate::ingest::SEARCH_OVERRUN_REMEDY);
        }
        other => panic!("expected BodyTooLarge on the search path, got {other:?}"),
    }
    server.await.unwrap();
}

#[tokio::test]
async fn a_trace_body_over_the_cap_carries_the_trace_remedy() {
    // Same overrun on the other path. Inverting the two remedies
    // would tell an operator to lower `--max-traces` for a single
    // trace the flag cannot shrink.
    let (endpoint, server) = spawn_one_shot_server(http_200_proto(&[0u8; 256])).await;
    let client = http_client::build_client();
    let uri: hyper::Uri = format!("{endpoint}/api/traces/abc123").parse().unwrap();

    let err = fetch_bytes(&client, uri, 64, None)
        .await
        .expect_err("a 256 byte body over a 64 byte cap must fail");
    match err {
        TempoError::BodyTooLarge { limit: 64, remedy } => {
            assert_eq!(remedy, crate::ingest::TRACE_OVERRUN_REMEDY);
        }
        other => panic!("expected BodyTooLarge on the per-trace path, got {other:?}"),
    }
    server.await.unwrap();
}

// --- search_traces ---

#[tokio::test]
async fn search_traces_happy_path_returns_ids() {
    let body = r#"{"traces":[{"traceID":"aaa111"},{"traceID":"bbb222"}]}"#;
    let (endpoint, server) = spawn_one_shot_server(http_200_json(body)).await;
    let client = http_client::build_client();
    let ids = search_traces(
        &client,
        &endpoint,
        "foo-svc",
        SearchWindow::Lookback(Duration::from_mins(5)),
        10,
        None,
    )
    .await
    .expect("search must succeed");
    assert_eq!(ids, vec!["aaa111".to_string(), "bbb222".to_string()]);
    server.await.unwrap();
}

#[tokio::test]
async fn search_traces_empty_result_surfaces_no_traces_found() {
    let body = r#"{"traces":[]}"#;
    let (endpoint, server) = spawn_one_shot_server(http_200_json(body)).await;
    let client = http_client::build_client();
    let err = search_traces(
        &client,
        &endpoint,
        "foo-svc",
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await
    .expect_err("empty search result must be NoTracesFound");
    assert_matches!(err, TempoError::NoTracesFound);
    server.await.unwrap();
}

#[tokio::test]
async fn search_traces_malformed_json_surfaces_json_parse() {
    let (endpoint, server) = spawn_one_shot_server(http_200_json("not json")).await;
    let client = http_client::build_client();
    let err = search_traces(
        &client,
        &endpoint,
        "foo-svc",
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await
    .expect_err("malformed JSON must be JsonParse");
    assert_matches!(err, TempoError::JsonParse(_));
    server.await.unwrap();
}

#[tokio::test]
async fn search_traces_http_500_surfaces_http_status() {
    let (endpoint, server) = spawn_one_shot_server(http_status(500, "Internal")).await;
    let client = http_client::build_client();
    let err = search_traces(
        &client,
        &endpoint,
        "foo-svc",
        SearchWindow::Lookback(Duration::from_mins(1)),
        10,
        None,
    )
    .await
    .expect_err("500 must surface as HttpStatus");
    match err {
        TempoError::HttpStatus { status: 500, .. } => {}
        other => panic!("expected HttpStatus {{ status: 500, .. }}, got {other:?}"),
    }
    server.await.unwrap();
}

// --- ingest_from_tempo: end-to-end search+fetch flow ---

/// Verifies that `--auth-header` propagates on BOTH the search and
/// the per-trace fetch connections. Parallel to
/// `jaeger_query::tests::search_sends_auth_header_on_wire` but
/// covers the two-step tempo flow, including the `JoinSet` fanout
/// clone that the jaeger-query single-request path does not
/// exercise.
#[tokio::test]
async fn ingest_from_tempo_sends_auth_header_on_both_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let search_body = r#"{"traces":[{"traceID":"abcdef"}]}"#;
    let search_resp = http_200_json(search_body);
    let mut proto_buf = Vec::new();
    ExportTraceServiceRequest {
        resource_spans: vec![],
    }
    .encode(&mut proto_buf)
    .expect("encode protobuf");
    let trace_resp = http_200_proto(&proto_buf);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let endpoint = format!("http://{addr}");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    let server = tokio::spawn(async move {
        // Connection 1: /api/search
        let (mut socket, _) = listener.accept().await.expect("accept 1");
        let mut rbuf = vec![0u8; 4096];
        let n = socket.read(&mut rbuf).await.expect("read 1");
        rbuf.truncate(n);
        tx.send(rbuf).await.expect("send 1");
        socket.write_all(&search_resp).await.expect("write 1");
        let _ = socket.shutdown().await;
        drop(socket);

        // Connection 2: /api/traces/<id>
        let (mut socket, _) = listener.accept().await.expect("accept 2");
        let mut rbuf = vec![0u8; 4096];
        let n = socket.read(&mut rbuf).await.expect("read 2");
        rbuf.truncate(n);
        tx.send(rbuf).await.expect("send 2");
        socket.write_all(&trace_resp).await.expect("write 2");
        let _ = socket.shutdown().await;
    });

    // The empty-trace OTLP makes the aggregated run return
    // NoTracesFound. The test ignores it and asserts on wire content,
    // not on the final event list.
    let _ = ingest_from_tempo(
        &endpoint,
        Some("foo-svc"),
        None,
        SearchWindow::Lookback(Duration::from_mins(5)),
        5,
        Some("Authorization: Bearer topsecret"),
    )
    .await;

    for label in ["search", "fetch"] {
        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).expect("utf8");
        assert!(
            text.to_lowercase()
                .contains("authorization: bearer topsecret"),
            "auth header missing from {label} request, got:\n{text}"
        );
    }
    server.await.expect("server join");
}

#[tokio::test]
async fn ingest_from_tempo_search_then_fetch_aggregates_events() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // The mock must handle MULTIPLE connections in sequence:
    //   1. /api/search returns one trace ID
    //   2. /api/traces/<id> returns an empty OTLP protobuf
    let search_body = r#"{"traces":[{"traceID":"abcdef"}]}"#;
    let search_resp = http_200_json(search_body);
    let mut proto_buf = Vec::new();
    ExportTraceServiceRequest {
        resource_spans: vec![],
    }
    .encode(&mut proto_buf)
    .unwrap();
    let trace_resp = http_200_proto(&proto_buf);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}");

    let server = tokio::spawn(async move {
        // Connection 1: search
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut rbuf = [0u8; 4096];
        let _ = socket.read(&mut rbuf).await;
        let _ = socket.write_all(&search_resp).await;
        let _ = socket.shutdown().await;
        drop(socket);

        // Connection 2: fetch trace
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = socket.read(&mut rbuf).await;
        let _ = socket.write_all(&trace_resp).await;
        let _ = socket.shutdown().await;
    });

    let err = ingest_from_tempo(
        &endpoint,
        Some("foo-svc"),
        None,
        SearchWindow::Lookback(Duration::from_mins(5)),
        5,
        None,
    )
    .await
    .expect_err("empty trace must surface as NoTracesFound after loop");
    // The trace was fetched successfully but contained zero spans,
    // so the aggregated result is empty and the run ends with NoTracesFound.
    assert_matches!(err, TempoError::NoTracesFound);
    server.await.unwrap();
}

// --- Error display ---

#[test]
fn tempo_error_display_messages_are_informative() {
    let e1 = TempoError::InvalidEndpoint("bad".to_string());
    let e2 = TempoError::Transport("oops".to_string());
    let e3 = TempoError::BodyRead("body".to_string());
    let e4 = TempoError::HttpStatus {
        status: 418,
        url: "http://tempo.example/api/search".to_string(),
    };
    let e5 = TempoError::Timeout;
    let e6 = TempoError::JsonParse("json".to_string());
    let e7 = TempoError::ProtobufDecode("proto".to_string());
    let e8 = TempoError::TraceNotFound("http://x".to_string());
    let e9 = TempoError::NoTracesFound;
    let e10 = TempoError::Interrupted;
    assert!(format!("{e1}").contains("endpoint"));
    assert!(format!("{e2}").contains("transport") || format!("{e2}").contains("Transport"));
    assert!(format!("{e3}").contains("body"));
    assert!(format!("{e4}").contains("418"));
    assert!(format!("{e5}").contains("timed out"));
    assert!(format!("{e6}").contains("JSON"));
    assert!(format!("{e7}").contains("protobuf") || format!("{e7}").contains("Protobuf"));
    assert!(format!("{e8}").contains("not found") || format!("{e8}").contains("Not found"));
    assert!(format!("{e9}").contains("no traces") || format!("{e9}").contains("No traces"));
    assert!(format!("{e10}").contains("interrupted") || format!("{e10}").contains("Interrupted"));
}

// --- Fetch-error classification ---

/// The Tempo path decodes protobuf then converts. Threading the
/// configured grouping attributes through must not change which spans
/// become events, only what identity they carry.
#[test]
fn a_protobuf_trace_converts_to_events_with_and_without_grouping() {
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    let span = Span {
        trace_id: vec![1; 16],
        span_id: vec![2; 8],
        name: "SELECT".to_string(),
        kind: 3, // CLIENT
        start_time_unix_nano: 1_000_000,
        end_time_unix_nano: 2_000_000,
        attributes: vec![
            kv("db.system", "postgresql"),
            kv("db.statement", "SELECT * FROM orders WHERE id = 1"),
            kv("tenant.id", "acme"),
        ],
        ..Default::default()
    };
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![
                    kv("service.name", "order-svc"),
                    kv("k8s.namespace.name", "prod-eu"),
                ],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans: vec![span],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    // Round-trip through protobuf exactly like `fetch_trace` does.
    let encoded = request.encode_to_vec();
    let decoded = ExportTraceServiceRequest::decode(encoded.as_slice()).unwrap();

    let (plain, _) = convert_otlp_request_counted_with_grouping(&decoded, None);
    assert_eq!(plain.len(), 1, "conversion must not drop the span");

    let keys: Vec<Arc<str>> = vec![Arc::from("tenant.id")];
    let (grouped, _) = convert_otlp_request_counted_with_grouping(&decoded, Some(&keys));
    assert_eq!(
        grouped.len(),
        1,
        "configuring grouping must not change which spans convert"
    );
    assert_eq!(grouped[0].grouping_value(), Some("acme"));
}

/// Mirrors a real lab trace: a single SERVER span for
/// `GET /actuator/prometheus`, valid protobuf, zero convertible spans.
/// Reporting it as `NoTracesFound` sends the operator hunting a missing
/// trace that Tempo returned.
#[tokio::test]
async fn a_trace_with_no_io_span_is_not_reported_as_a_missing_trace() {
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![kv("service.name", "shop")],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: vec![3; 16],
                    span_id: vec![4; 8],
                    name: "GET /actuator/prometheus".to_string(),
                    kind: 2, // SERVER: an inbound hop, never an outbound call
                    start_time_unix_nano: 1_000_000,
                    end_time_unix_nano: 2_000_000,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let (endpoint, server) = spawn_one_shot_server(http_200_proto(&request.encode_to_vec())).await;

    let err = ingest_from_tempo(
        &endpoint,
        None,
        Some("33ad5d1d22faaf57f938f4a7083791c"),
        SearchWindow::Lookback(Duration::from_hours(1)),
        10,
        None,
    )
    .await
    .expect_err("a trace with no I/O span must not look like a missing trace");

    match err {
        TempoError::NoConvertibleSpans { traces: 1 } => {}
        other => panic!("expected NoConvertibleSpans {{ traces: 1 }}, got {other:?}"),
    }
    // The message must name what to look at, not just what failed.
    let rendered = err.to_string();
    assert!(rendered.contains("db.system"), "{rendered}");
    assert!(rendered.contains("SERVER"), "{rendered}");
    server.await.unwrap();
}

/// Sanity-check that every hard-failure variant of `TempoError` lands in a
/// distinct, stable bucket. Acts as a drift guard: if someone adds a new
/// variant later and forgets to extend `classify_fetch_error`, the new
/// variant silently ends up as `"other"` in the summary counts. This
/// test either catches it (when the new variant deserves its own bucket)
/// or documents that the variant belongs in the catch-all.
#[test]
fn classify_fetch_error_buckets_every_hard_failure_variant() {
    assert_eq!(classify_fetch_error(&TempoError::Timeout), "timeout");
    assert_eq!(
        classify_fetch_error(&TempoError::Transport("x".into())),
        "transport"
    );
    assert_eq!(
        classify_fetch_error(&TempoError::HttpStatus {
            status: 500,
            url: "u".into()
        }),
        "http_status"
    );
    assert_eq!(
        classify_fetch_error(&TempoError::ProtobufDecode("p".into())),
        "protobuf_decode"
    );
    assert_eq!(
        classify_fetch_error(&TempoError::BodyRead("b".into())),
        "body_read"
    );
    assert_eq!(
        classify_fetch_error(&TempoError::JsonParse("j".into())),
        "json_parse"
    );
    assert_eq!(
        classify_fetch_error(&TempoError::BodyTooLarge {
            limit: 1,
            remedy: "r"
        }),
        "body_too_large"
    );
    // Variants that should never reach the per-trace classifier in
    // practice (they surface earlier in the pipeline) fall through to
    // the catch-all bucket rather than crashing.
    assert_eq!(
        classify_fetch_error(&TempoError::InvalidEndpoint("x".into())),
        "other"
    );
    assert_eq!(classify_fetch_error(&TempoError::NoTracesFound), "other");
}

/// End-to-end check of the drain loop with mixed per-trace outcomes:
/// one fetch returns HTTP 500, one returns an empty OTLP protobuf, and
/// one returns HTTP 404 (mapped to `TraceNotFound`). The search step is
/// successful, so `ingest_from_tempo` reaches the parallel fetch stage
/// and hits every error-handling branch of `drain_fetch_set` in a single
/// run. The aggregated outcome is empty (only the 200-empty-body trace
/// contributed 0 spans), so the outer function reports `NoTracesFound`.
/// It exercises the classification code path end-to-end without
/// asserting on log levels.
#[tokio::test]
async fn ingest_from_tempo_drains_mixed_per_trace_outcomes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let search_body =
        r#"{"traces":[{"traceID":"aaa111"},{"traceID":"bbb222"},{"traceID":"ccc333"}]}"#;
    let search_resp = http_200_json(search_body);

    let mut empty_proto = Vec::new();
    ExportTraceServiceRequest {
        resource_spans: vec![],
    }
    .encode(&mut empty_proto)
    .unwrap();
    let ok_empty_resp = http_200_proto(&empty_proto);
    let http_500 = http_status(500, "Internal");
    let http_404 = http_status(404, "Not Found");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("http://{addr}");

    let server = tokio::spawn(async move {
        // Connection 1: /api/search returns 3 trace IDs.
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut rbuf = [0u8; 4096];
        let _ = sock.read(&mut rbuf).await;
        let _ = sock.write_all(&search_resp).await;
        let _ = sock.shutdown().await;
        drop(sock);

        // Connections 2-4: the three /api/traces/{id} fetches, order
        // non-deterministic because the drain loop is parallel. Match on
        // the request line to route each connection to the intended
        // response (500 for bbb222, 404 for ccc333, 200-empty for the
        // remaining one). A single unmatched trace ID is fine too. The
        // classifier still reports it as a 200-empty success.
        for _ in 0..3 {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut rbuf = [0u8; 4096];
            let n = sock.read(&mut rbuf).await.unwrap_or(0);
            let req = std::str::from_utf8(&rbuf[..n]).unwrap_or("");
            let resp: &[u8] = if req.contains("/api/traces/bbb222") {
                &http_500
            } else if req.contains("/api/traces/ccc333") {
                &http_404
            } else {
                &ok_empty_resp
            };
            let _ = sock.write_all(resp).await;
            let _ = sock.shutdown().await;
        }
    });

    let err = ingest_from_tempo(
        &endpoint,
        Some("foo-svc"),
        None,
        SearchWindow::Lookback(Duration::from_mins(5)),
        10,
        None,
    )
    .await
    .expect_err("mixed-outcome run with only empty successes must surface NoTracesFound");
    assert!(
        matches!(err, TempoError::NoTracesFound),
        "expected NoTracesFound, got {err:?}"
    );
    server.await.unwrap();
}

/// Tempo counts in whole seconds, so the millisecond bounds round
/// outwards: the start down, the end up. Both bounds here carry a
/// millisecond remainder, otherwise `div_ceil` and a plain division
/// would be indistinguishable and the rule would go untested.
#[tokio::test]
async fn an_absolute_window_rounds_outwards_to_whole_seconds() {
    let (endpoint, mut captured, server) =
        spawn_capture_server(http_200_text("application/json", r#"{"traces":[]}"#)).await;
    let client = http_client::build_client();
    let _ = search_traces(
        &client,
        &endpoint,
        "order-svc",
        SearchWindow::Absolute {
            start_ms: 1_787_838_000_600,
            end_ms: 1_787_839_200_400,
        },
        10,
        None,
    )
    .await;
    let request = captured.recv().await.expect("captured request");
    let request = String::from_utf8_lossy(&request);
    // Delimited on both sides: an undelimited prefix would also match a
    // value carrying an extra factor of a thousand, which is the one
    // mistake these bounds are here to catch.
    assert!(request.contains("&start=1787838000&"), "got: {request}");
    assert!(request.contains("&end=1787839201&"), "got: {request}");
    server.await.expect("server join");
}
