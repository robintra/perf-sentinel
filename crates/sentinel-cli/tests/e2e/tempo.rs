//! `tempo` subcommand: exit-code contract for local argument-validation
//! and fetch failures, which don't need a live Tempo backend to trigger.

#![cfg(feature = "tempo")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

/// The id the stub search hands back and the stub trace carries. Hex
/// only, so the fetch's own trace-id validation accepts it.
const STUB_TRACE_ID: [u8; 16] = [0xa1; 16];
const STUB_TRACE_ID_HEX: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

/// Append a protobuf varint.
fn push_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = u8::try_from(n & 0x7f).expect("seven bits fit a byte");
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Append a length-delimited protobuf field (wire type 2).
fn push_bytes_field(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
    push_varint(out, (u64::from(field) << 3) | 2);
    push_varint(out, u64::try_from(payload.len()).expect("body fits u64"));
    out.extend_from_slice(payload);
}

/// Append a fixed64 protobuf field (wire type 1).
fn push_fixed64(out: &mut Vec<u8>, field: u32, value: u64) {
    push_varint(out, (u64::from(field) << 3) | 1);
    out.extend_from_slice(&value.to_le_bytes());
}

/// Append a varint protobuf field (wire type 0), which is how an enum travels.
fn push_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    push_varint(out, u64::from(field) << 3);
    push_varint(out, value);
}

/// `SpanKind` as OTLP numbers them. Fidelity rather than coverage: a span
/// carrying `db.system` is kept whatever its kind, so marking these correctly
/// changes no assertion today. It stops the stub from serving UNSPECIFIED
/// spans no backend sends, and it is what a reader copying this shape needs.
const SPAN_KIND_SERVER: u64 = 2;
const SPAN_KIND_CLIENT: u64 = 3;

/// One OTLP string attribute: `KeyValue { key, AnyValue { string_value } }`.
fn otlp_attr(key: &str, value: &str) -> Vec<u8> {
    let mut any = Vec::new();
    push_bytes_field(&mut any, 1, value.as_bytes());
    let mut kv = Vec::new();
    push_bytes_field(&mut kv, 1, key.as_bytes());
    push_bytes_field(&mut kv, 2, &any);
    kv
}

/// One OTLP `Span`. Span ids are a repeated byte so a single seed names
/// each of them.
fn otlp_span(
    span_id: u8,
    parent: Option<u8>,
    name: &str,
    kind: u64,
    start_ns: u64,
    end_ns: u64,
    attrs: &[Vec<u8>],
) -> Vec<u8> {
    let mut span = Vec::new();
    push_bytes_field(&mut span, 1, &STUB_TRACE_ID);
    push_bytes_field(&mut span, 2, &[span_id; 8]);
    if let Some(parent) = parent {
        push_bytes_field(&mut span, 4, &[parent; 8]);
    }
    push_bytes_field(&mut span, 5, name.as_bytes());
    push_varint_field(&mut span, 6, kind);
    push_fixed64(&mut span, 7, start_ns);
    push_fixed64(&mut span, 8, end_ns);
    for attr in attrs {
        push_bytes_field(&mut span, 9, attr);
    }
    span
}

/// The `ExportTraceServiceRequest` the stub serves for `/api/traces/{id}`:
/// one routed root over six sibling SELECTs, the n+1 shape the jaeger
/// fixture carries. Hand-encoded because the CLI test crate has no prost
/// dependency to build it from the generated types.
fn otlp_trace_body() -> Vec<u8> {
    const ROOT_NS: u64 = 1_720_621_921_000_000_000;

    let mut scope_spans = Vec::new();
    push_bytes_field(
        &mut scope_spans,
        2,
        &otlp_span(
            0x0a,
            None,
            "OrderService::create_order",
            SPAN_KIND_SERVER,
            ROOT_NS,
            ROOT_NS + 50_000_000,
            &[
                otlp_attr("http.route", "POST /api/orders/42/submit"),
                otlp_attr("code.function", "OrderService::create_order"),
            ],
        ),
    );
    for i in 1..=6u64 {
        let start = ROOT_NS + i * 1_000_000;
        push_bytes_field(
            &mut scope_spans,
            2,
            &otlp_span(
                u8::try_from(i).expect("child index fits a byte"),
                Some(0x0a),
                "db.query",
                SPAN_KIND_CLIENT,
                start,
                start + 800_000,
                &[
                    otlp_attr(
                        "db.statement",
                        &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    ),
                    otlp_attr("db.system", "postgresql"),
                ],
            ),
        );
    }

    let mut resource = Vec::new();
    push_bytes_field(&mut resource, 1, &otlp_attr("service.name", "order-svc"));

    let mut resource_spans = Vec::new();
    push_bytes_field(&mut resource_spans, 1, &resource);
    push_bytes_field(&mut resource_spans, 2, &scope_spans);

    let mut request = Vec::new();
    push_bytes_field(&mut request, 1, &resource_spans);
    request
}

/// Serve the two hops a tempo search makes, then stop: `/api/search`
/// answers JSON trace ids, `/api/traces/{id}` answers OTLP protobuf.
/// Every response closes its connection, so one accept loop covers the
/// pair whether or not the client reuses the socket. Returns the port.
fn spawn_tempo_stub(search_body: String, trace_body: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                return;
            }
            // Drain the headers, otherwise the client can see a reset
            // before it reads the response.
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let is_trace = request_line.contains("/api/traces/");
            let body: &[u8] = if is_trace {
                &trace_body
            } else {
                search_body.as_bytes()
            };
            let content_type = if is_trace {
                "application/protobuf"
            } else {
                "application/json"
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: {content_type}\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
            if is_trace {
                return;
            }
        }
    });

    port
}

#[test]
fn cli_tempo_missing_trace_id_and_service_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["tempo", "--endpoint", "http://127.0.0.1:1"])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--trace-id or --service"),
        "stderr should name the missing flag, got: {stderr}"
    );
    // Argument validation, never a quality-gate breach. See docs/CI.md
    // "Exit codes".
    assert_eq!(
        output.status.code(),
        Some(75),
        "bad invocation must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_invalid_lookback_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--lookback",
            "not-a-duration",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(75),
        "unparsable --lookback must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_fetch_failure_exits_tooling_error() {
    // Port 1 is a privileged port nothing listens on; the fetch fails
    // fast with a connection error, no live Tempo backend needed.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error fetching traces from Tempo"),
        "stderr should name the fetch failure, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_absolute_window_conflicts_with_lookback() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--lookback",
            "2h",
            "--from",
            "2026-08-20T15:00:00Z",
            "--to",
            "2026-08-20T16:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--lookback") && stderr.contains("cannot be used with"),
        "stderr should name the conflict, got: {stderr}"
    );
}

#[test]
fn cli_tempo_absolute_window_conflicts_with_trace_id() {
    // A trace ID resolves to exactly one trace, so a window would be read
    // and silently dropped rather than applied.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--trace-id",
            "abc123def456",
            "--from",
            "2026-08-20T15:00:00Z",
            "--to",
            "2026-08-20T16:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "stderr should name the conflict, got: {stderr}"
    );
}

#[test]
fn cli_tempo_from_requires_to() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--from",
            "2026-08-20T15:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--to"),
        "stderr should name the missing half of the pair, got: {stderr}"
    );
}

#[test]
fn cli_tempo_inverted_absolute_window_exits_tooling_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--from",
            "2026-08-20T16:00:00Z",
            "--to",
            "2026-08-20T15:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--from 2026-08-20T16:00:00Z --to 2026-08-20T15:00:00Z"),
        "stderr should echo the window the operator typed, got: {stderr}"
    );
    // The window is refused before any request is issued, so this is an
    // invocation error like an unparsable --lookback, not a fetch failure.
    assert_eq!(
        output.status.code(),
        Some(75),
        "an inverted window must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_absolute_window_reaches_the_fetch() {
    // Nothing listens on port 1, so getting as far as a connection error
    // proves the window parsed and the request was actually issued.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "order-svc",
            "--from",
            "2026-08-20T15:00:00Z",
            "--to",
            "2026-08-20T16:00:00Z",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error fetching traces from Tempo"),
        "an absolute window must reach the fetch, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not 1"
    );
}

#[test]
fn cli_tempo_rejects_max_traces_past_the_shared_ceiling() {
    // Parse-time, no server involved: the bound is the client's, shared
    // with jaeger-query, and must refuse before any request is issued.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "999999",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(output.status.code(), Some(2), "clap usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1..=10000"),
        "the refusal names the range, got: {stderr}"
    );
}

#[test]
fn cli_tempo_rejects_zero_max_traces() {
    // The bottom of the same range: a search for zero traces is a
    // typo, refused before the subcommand runs.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "0",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(output.status.code(), Some(2), "clap usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--max-traces") && stderr.contains("1..=10000"),
        "the refusal names the flag and the range, got: {stderr}"
    );
}

#[test]
fn cli_tempo_rejects_max_traces_one_past_the_ceiling() {
    // 10001 is the first refused value, the boundary a far-out figure
    // like 999999 never pins down.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "10001",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert_eq!(output.status.code(), Some(2), "clap usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--max-traces") && stderr.contains("1..=10000"),
        "the refusal names the flag and the range, got: {stderr}"
    );
}

#[test]
fn cli_tempo_max_traces_at_the_ceiling_reaches_the_fetch() {
    // 10000 is the last accepted value. Nothing listens on port 1, so
    // failing on the connection proves the validator let it through.
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            "http://127.0.0.1:1",
            "--service",
            "svc",
            "--max-traces",
            "10000",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error fetching traces from Tempo"),
        "an in-range --max-traces must reach the fetch, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(75),
        "a fetch/network failure must exit EXIT_TOOLING_ERROR (75), not a clap usage error"
    );
}

#[test]
fn cli_tempo_json_carries_the_findings_spans() {
    // The jaeger-query mirror of this seam, over tempo's two hops: the
    // search hands back an id, the trace fetch answers OTLP protobuf,
    // and the JSON still has to carry the spans of the traces its
    // findings point at, since it travels without its input.
    let search = format!(r#"{{"traces":[{{"traceID":"{STUB_TRACE_ID_HEX}"}}]}}"#);
    let port = spawn_tempo_stub(search, otlp_trace_body());

    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args([
            "tempo",
            "--endpoint",
            &format!("http://127.0.0.1:{port}"),
            "--service",
            "order-svc",
            "--no-acknowledgments",
            "--format",
            "json",
        ])
        .output()
        .expect("failed to execute perf-sentinel");

    assert!(
        output.status.success(),
        "the stubbed two-hop fetch must analyze cleanly, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout parses as JSON");
    assert!(
        !report["findings"]
            .as_array()
            .expect("findings array")
            .is_empty(),
        "the stub trace must yield a finding for a trace to be embedded"
    );
    let embedded = report["embedded_traces"]
        .as_array()
        .expect("embedded_traces array present");
    assert_eq!(
        embedded.len(),
        1,
        "the one trace the findings point at travels with the report"
    );
    assert_eq!(embedded[0]["trace_id"], STUB_TRACE_ID_HEX);
    assert!(
        !embedded[0]["spans"]
            .as_array()
            .expect("spans array")
            .is_empty(),
        "an embedded trace without spans draws nothing"
    );
}

#[test]
fn cli_tempo_help_mentions_sort() {
    let output = Command::new(env!("CARGO_BIN_EXE_perf-sentinel"))
        .args(["tempo", "--help"])
        .output()
        .expect("failed to execute perf-sentinel");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--sort"), "help mentions --sort");
}
