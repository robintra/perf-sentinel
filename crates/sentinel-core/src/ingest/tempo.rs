//! Tempo trace ingestion: query Grafana Tempo's HTTP API for traces.
//!
//! Supports two modes:
//! - By trace ID: fetch a single trace via `GET /api/traces/{traceID}`
//! - By service + lookback: search for trace IDs via `GET /api/search`,
//!   then fetch each trace
//!
//! Trace data is returned as OTLP protobuf, decoded via `prost`, and
//! converted to `SpanEvent` using the existing `convert_otlp_request`.

use std::time::Duration;

use crate::event::SpanEvent;
use crate::http_client::{self, HttpClient};
use crate::ingest::otlp::convert_otlp_request;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;

// ---------------------------------------------------------------
// Error type
// ---------------------------------------------------------------

/// Errors from Tempo API interactions.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TempoError {
    #[error("invalid Tempo endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("invalid lookback duration: {0}")]
    InvalidLookback(String),

    #[error("HTTP transport error: {0}")]
    Transport(String),

    #[error("Tempo returned HTTP {status} for {url}")]
    HttpStatus { status: u16, url: String },

    #[error("request timed out")]
    Timeout,

    #[error("failed to read response body: {0}")]
    BodyRead(String),

    #[error("failed to decode protobuf response: {0}")]
    ProtobufDecode(String),

    #[error("failed to parse JSON response: {0}")]
    JsonParse(String),

    #[error("trace not found: {0}")]
    TraceNotFound(String),

    #[error("no traces found for the given search criteria")]
    NoTracesFound,
}

// ---------------------------------------------------------------
// Lookback duration parser
// ---------------------------------------------------------------

/// Parse a human-readable duration string like `"1h"`, `"30m"`, `"24h"`, `"2h30m"`.
///
/// # Errors
///
/// Returns `TempoError::InvalidLookback` for malformed inputs.
pub fn parse_lookback(s: &str) -> Result<Duration, TempoError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(TempoError::InvalidLookback("empty string".to_string()));
    }

    let mut total_secs: u64 = 0;
    let mut num_buf = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            if num_buf.is_empty() {
                return Err(TempoError::InvalidLookback(format!(
                    "unexpected '{ch}' without a preceding number"
                )));
            }
            let n: u64 = num_buf
                .parse()
                .map_err(|_| TempoError::InvalidLookback(format!("invalid number: {num_buf}")))?;
            num_buf.clear();
            match ch {
                'h' => total_secs += n * 3600,
                'm' => total_secs += n * 60,
                's' => total_secs += n,
                _ => {
                    return Err(TempoError::InvalidLookback(format!(
                        "unknown unit '{ch}', expected h/m/s"
                    )));
                }
            }
        }
    }

    // Trailing number without unit is rejected
    if !num_buf.is_empty() {
        return Err(TempoError::InvalidLookback(format!(
            "number '{num_buf}' without a unit suffix (h/m/s)"
        )));
    }

    if total_secs == 0 {
        return Err(TempoError::InvalidLookback(
            "duration must be greater than zero".to_string(),
        ));
    }

    Ok(Duration::from_secs(total_secs))
}

/// Minimal percent-encoding for URI query parameter values.
/// Encodes `&`, `=`, `#`, `+`, space, and non-ASCII bytes.
fn percent_encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'&' | b'=' | b'#' | b'+' | b' ' | b'%' | 0x00..=0x1F | 0x7F..=0xFF => {
                out.push('%');
                out.push(char::from(b"0123456789ABCDEF"[(b >> 4) as usize]));
                out.push(char::from(b"0123456789ABCDEF"[(b & 0x0f) as usize]));
            }
            _ => out.push(char::from(b)),
        }
    }
    out
}

// ---------------------------------------------------------------
// Tempo search response types
// ---------------------------------------------------------------

#[derive(serde::Deserialize)]
struct SearchResponse {
    #[serde(default)]
    traces: Vec<TraceMeta>,
}

#[derive(serde::Deserialize)]
struct TraceMeta {
    #[serde(rename = "traceID")]
    trace_id: String,
}

// ---------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------

/// Maximum body size for search responses (1 MiB).
///
/// Search responses only return trace-ID summaries, not span payloads,
/// so 1 MiB is generous even for a `limit=500` query.
const MAX_SEARCH_BODY_BYTES: usize = 1024 * 1024;

/// Maximum body size for a full trace fetch (64 MiB).
///
/// Tempo traces can legitimately carry hundreds or thousands of spans
/// in a single OTLP protobuf; the 8 MiB cap used for Prometheus /metrics
/// and Electricity Maps JSON is not enough. 64 MiB is large enough to
/// cover production workloads while still bounding the worst case at a
/// level that fits comfortably in daemon RSS (the `<20 MB loaded`
/// target only applies to steady-state, not a one-shot `tempo` CLI
/// invocation that exits after the fetch).
const MAX_TRACE_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Timeout for the Tempo search endpoint (`/api/search`). Search responses
/// are small (an index lookup that returns a list of trace IDs) and should
/// complete in well under a second on a healthy query-frontend, so a tight
/// timeout fails fast on a broken endpoint.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for the Tempo single-trace endpoint (`/api/traces/{id}`). Trace
/// bodies can be large (a full OTLP dump of every span for the trace, which
/// on a wide fan-out request can be many MiB) and the query-frontend has to
/// gather spans from the ingesters + long-term storage. Five seconds was
/// empirically too tight on a `tempo-distributed` deployment queried over
/// a WAN with 24 h lookback: tens of traces per 100-trace batch hit the
/// timeout. Thirty seconds is the same order of magnitude Grafana uses for
/// its Tempo datasource query timeout default.
const FETCH_TRACE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on in-flight `fetch_trace` requests when processing a search-then-fetch
/// flow. The previous sequential loop paid the full round-trip latency N
/// times; parallelism collapses that to roughly `N × RTT / FETCH_CONCURRENCY`.
/// Sixteen is empirically a sweet spot for Tempo query-frontends: high
/// enough to saturate a remote connection (observed 10-20s for 100 traces
/// over a WAN link, vs. 2m30s sequential) and low enough to avoid being
/// rate-limited or overwhelming a single frontend replica.
const FETCH_CONCURRENCY: usize = 16;

/// Fetch raw bytes from a Tempo endpoint with size limit and timeout.
///
/// Shared implementation behind `fetch_bytes` (protobuf) and `fetch_json`.
/// Builds the request, applies the timeout, checks the HTTP status, and
/// reads the limited body. When `map_404` is true, 404 responses return
/// `TempoError::TraceNotFound` instead of the generic `HttpStatus`.
async fn fetch_raw(
    client: &HttpClient,
    uri: hyper::Uri,
    accept: &'static str,
    max_bytes: usize,
    map_404: bool,
    timeout: Duration,
) -> Result<bytes::Bytes, TempoError> {
    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(&uri)
        .header("Accept", accept)
        .header("User-Agent", "perf-sentinel")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| TempoError::Transport(e.to_string()))?;

    let resp = tokio::time::timeout(timeout, client.request(req))
        .await
        .map_err(|_| TempoError::Timeout)?
        .map_err(|e| TempoError::Transport(e.to_string()))?;

    let status = resp.status().as_u16();
    if map_404 && status == 404 {
        return Err(TempoError::TraceNotFound(http_client::redact_endpoint(
            &uri,
        )));
    }
    if status != 200 {
        // Include the redacted URL so operators pointing at a wrong Tempo
        // component (e.g. `tempo-querier` instead of `tempo-query-frontend`
        // in a microservices deployment) see which endpoint 404'd without
        // having to re-derive it from the CLI flags.
        return Err(TempoError::HttpStatus {
            status,
            url: http_client::redact_endpoint(&uri),
        });
    }

    let limited = http_body_util::Limited::new(resp.into_body(), max_bytes);
    let body = http_body_util::BodyExt::collect(limited)
        .await
        .map_err(|e| TempoError::BodyRead(e.to_string()))?
        .to_bytes();

    Ok(body)
}

/// Fetch raw bytes from a Tempo endpoint (OTLP protobuf). 404 maps to
/// `TraceNotFound` for graceful handling in search+fetch flows.
async fn fetch_bytes(
    client: &HttpClient,
    uri: hyper::Uri,
    max_bytes: usize,
) -> Result<bytes::Bytes, TempoError> {
    fetch_raw(
        client,
        uri,
        "application/protobuf",
        max_bytes,
        true,
        FETCH_TRACE_TIMEOUT,
    )
    .await
}

/// Fetch JSON from a Tempo endpoint.
async fn fetch_json(
    client: &HttpClient,
    uri: hyper::Uri,
    max_bytes: usize,
) -> Result<String, TempoError> {
    let body = fetch_raw(
        client,
        uri,
        "application/json",
        max_bytes,
        false,
        SEARCH_TIMEOUT,
    )
    .await?;
    String::from_utf8(body.to_vec()).map_err(|e| TempoError::BodyRead(e.to_string()))
}

// ---------------------------------------------------------------
// Core API functions
// ---------------------------------------------------------------

/// Search Tempo for trace IDs matching a service name within a lookback window.
///
/// # Errors
///
/// Returns `TempoError` on HTTP errors, timeouts, or JSON parse failures.
pub async fn search_traces(
    client: &HttpClient,
    endpoint: &str,
    service: &str,
    lookback: Duration,
    limit: usize,
) -> Result<Vec<String>, TempoError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let end = now.as_secs();
    let start = end.saturating_sub(lookback.as_secs());

    let encoded_service = percent_encode_query_value(service);
    let uri_str = format!(
        "{endpoint}/api/search?tags=service.name%3D{encoded_service}&start={start}&end={end}&limit={limit}"
    );
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|_| TempoError::InvalidEndpoint(endpoint.to_string()))?;

    let json = fetch_json(client, uri, MAX_SEARCH_BODY_BYTES).await?;

    let response: SearchResponse =
        serde_json::from_str(&json).map_err(|e| TempoError::JsonParse(e.to_string()))?;

    let ids: Vec<String> = response.traces.into_iter().map(|t| t.trace_id).collect();
    if ids.is_empty() {
        return Err(TempoError::NoTracesFound);
    }

    Ok(ids)
}

/// Fetch a single trace from Tempo and convert to `SpanEvent`s.
///
/// Requests OTLP protobuf format and decodes via `prost::Message`.
///
/// # Errors
///
/// Returns `TempoError` on HTTP errors, timeouts, or protobuf decode failures.
pub async fn fetch_trace(
    client: &HttpClient,
    endpoint: &str,
    trace_id: &str,
) -> Result<Vec<SpanEvent>, TempoError> {
    // Validate trace_id is hex-only (OTLP spec: hex-encoded 16/32 bytes).
    if !trace_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(TempoError::InvalidEndpoint(format!(
            "trace ID '{trace_id}' contains non-hex characters"
        )));
    }
    let uri_str = format!("{endpoint}/api/traces/{trace_id}");
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|_| TempoError::InvalidEndpoint(endpoint.to_string()))?;

    let body = fetch_bytes(client, uri, MAX_TRACE_BODY_BYTES).await?;

    let request = ExportTraceServiceRequest::decode(body)
        .map_err(|e| TempoError::ProtobufDecode(e.to_string()))?;

    Ok(convert_otlp_request(&request))
}

/// Ingest traces from Tempo: either a single trace by ID or a search-then-fetch flow.
///
/// # Errors
///
/// Returns `TempoError` on API failures.
pub async fn ingest_from_tempo(
    endpoint: &str,
    service: Option<&str>,
    trace_id: Option<&str>,
    lookback: Duration,
    max_traces: usize,
) -> Result<Vec<SpanEvent>, TempoError> {
    // Validate endpoint: must start with http:// or https://, no
    // credentials in the authority section. We deliberately check for
    // `@` only in the authority (scheme://authority/path?query), not
    // in the path or query string, so paths like `/api/traces?owner=foo%40example.com`
    // are accepted. The `validate_http_authority` helper in config.rs
    // uses the same strip-then-split-on-`/` technique.
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(TempoError::InvalidEndpoint(format!(
            "endpoint must start with http:// or https://, got '{endpoint}'"
        )));
    }
    let after_scheme = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or("");
    // The authority ends at the first `/` (start of path) or `?`
    // (start of query). Everything before is host[:port] with optional
    // userinfo. We reject `@` only in that slice.
    let authority_end = after_scheme.find(['/', '?']).unwrap_or(after_scheme.len());
    if after_scheme[..authority_end].contains('@') {
        return Err(TempoError::InvalidEndpoint(
            "endpoint must not contain credentials (user:pass@host)".to_string(),
        ));
    }

    let client = http_client::build_client();

    if let Some(tid) = trace_id {
        tracing::info!(trace_id = tid, "Fetching single trace from Tempo");
        return fetch_trace(&client, endpoint, tid).await;
    }

    let svc = service.ok_or_else(|| {
        TempoError::InvalidEndpoint("either --trace-id or --service is required".to_string())
    })?;

    tracing::info!(
        service = svc,
        lookback_secs = lookback.as_secs(),
        max_traces,
        "Searching Tempo for traces"
    );

    let trace_ids = search_traces(&client, endpoint, svc, lookback, max_traces).await?;
    let total = trace_ids.len();
    tracing::info!(count = total, "Found traces, fetching...");

    // Parallelize per-trace fetches via a `JoinSet`, capped at
    // `FETCH_CONCURRENCY` in-flight requests to avoid flooding Tempo's
    // query-frontend. Mirrors the pattern used by
    // `score::cloud_energy::scraper` for per-service Prometheus CPU queries.
    // The hyper client holds an `Arc` internally, so `.clone()` is cheap;
    // the endpoint is cloned per task so each owned future is `'static` as
    // required by `spawn`.
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(FETCH_CONCURRENCY));
    let mut set: tokio::task::JoinSet<(String, Result<Vec<SpanEvent>, TempoError>)> =
        tokio::task::JoinSet::new();
    for tid in trace_ids {
        let client_clone = client.clone();
        let endpoint_owned = endpoint.to_string();
        let sem = std::sync::Arc::clone(&semaphore);
        set.spawn(async move {
            let Ok(_permit) = sem.acquire_owned().await else {
                return (
                    tid,
                    Err(TempoError::Transport("semaphore closed".to_string())),
                );
            };
            let result = fetch_trace(&client_clone, &endpoint_owned, &tid).await;
            (tid, result)
        });
    }

    let mut all_events = Vec::new();
    let mut done: usize = 0;
    while let Some(join_result) = set.join_next().await {
        done += 1;
        match join_result {
            Ok((tid, Ok(events))) => {
                tracing::debug!(
                    trace_id = %tid,
                    events = events.len(),
                    progress = format!("{done}/{total}"),
                    "Fetched trace"
                );
                all_events.extend(events);
            }
            Ok((tid, Err(TempoError::TraceNotFound(_)))) => {
                tracing::warn!(trace_id = %tid, "Trace not found, skipping");
            }
            Ok((tid, Err(e))) => {
                tracing::error!(trace_id = %tid, error = %e, "Failed to fetch trace, skipping");
            }
            Err(e) => {
                tracing::error!(error = %e, "Trace fetch task panicked or was cancelled");
            }
        }
    }

    if all_events.is_empty() {
        return Err(TempoError::NoTracesFound);
    }

    Ok(all_events)
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Lookback parser ---

    #[test]
    fn parse_lookback_hours() {
        assert_eq!(parse_lookback("1h").unwrap(), Duration::from_hours(1));
        assert_eq!(parse_lookback("24h").unwrap(), Duration::from_hours(24));
    }

    #[test]
    fn parse_lookback_minutes() {
        assert_eq!(parse_lookback("30m").unwrap(), Duration::from_mins(30));
    }

    #[test]
    fn parse_lookback_seconds() {
        assert_eq!(parse_lookback("90s").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn parse_lookback_combined() {
        assert_eq!(parse_lookback("2h30m").unwrap(), Duration::from_mins(150));
    }

    #[test]
    fn parse_lookback_rejects_empty() {
        assert!(parse_lookback("").is_err());
    }

    #[test]
    fn parse_lookback_rejects_no_unit() {
        assert!(parse_lookback("30").is_err());
    }

    #[test]
    fn parse_lookback_rejects_unknown_unit() {
        assert!(parse_lookback("5d").is_err());
    }

    #[test]
    fn parse_lookback_rejects_zero() {
        assert!(parse_lookback("0h").is_err());
    }

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
        let events = convert_otlp_request(&decoded);
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

    use crate::test_helpers::{http_200_bytes, http_200_text, http_status, spawn_one_shot_server};

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
            Duration::from_mins(1),
            10,
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
            Duration::from_mins(1),
            10,
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
        let err = ingest_from_tempo("http://tempo.local", None, None, Duration::from_mins(1), 10)
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
        // Note: this test uses an unreachable endpoint. We don't care
        // whether the fetch succeeds, only that the validator does
        // NOT synchronously return `InvalidEndpoint`. Any other error
        // (transport, timeout, etc.) is acceptable.
        let result = ingest_from_tempo(
            "http://127.0.0.1:1/api/traces?owner=foo%40example.com",
            None,
            Some("abc123"),
            Duration::from_mins(1),
            10,
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
        let err = fetch_trace(&client, "http://tempo.local", "not-hex-id!")
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
        let events = fetch_trace(&client, &endpoint, "abc123def456")
            .await
            .expect("valid OTLP must decode");
        assert!(events.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_trace_surfaces_404_as_trace_not_found() {
        let (endpoint, server) = spawn_one_shot_server(http_status(404, "Not Found")).await;
        let client = http_client::build_client();
        let err = fetch_trace(&client, &endpoint, "abc123")
            .await
            .expect_err("404 must surface as TraceNotFound");
        assert!(matches!(err, TempoError::TraceNotFound(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_trace_surfaces_500_as_http_status() {
        let (endpoint, server) = spawn_one_shot_server(http_status(500, "Internal")).await;
        let client = http_client::build_client();
        let err = fetch_trace(&client, &endpoint, "abc123")
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
        let err = fetch_trace(&client, &endpoint, "abc123")
            .await
            .expect_err("malformed protobuf must surface as ProtobufDecode");
        assert!(matches!(err, TempoError::ProtobufDecode(_)));
        server.await.unwrap();
    }

    // --- search_traces ---

    #[tokio::test]
    async fn search_traces_happy_path_returns_ids() {
        let body = r#"{"traces":[{"traceID":"aaa111"},{"traceID":"bbb222"}]}"#;
        let (endpoint, server) = spawn_one_shot_server(http_200_json(body)).await;
        let client = http_client::build_client();
        let ids = search_traces(&client, &endpoint, "foo-svc", Duration::from_mins(5), 10)
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
        let err = search_traces(&client, &endpoint, "foo-svc", Duration::from_mins(1), 10)
            .await
            .expect_err("empty search result must be NoTracesFound");
        assert!(matches!(err, TempoError::NoTracesFound));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn search_traces_malformed_json_surfaces_json_parse() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json("not json")).await;
        let client = http_client::build_client();
        let err = search_traces(&client, &endpoint, "foo-svc", Duration::from_mins(1), 10)
            .await
            .expect_err("malformed JSON must be JsonParse");
        assert!(matches!(err, TempoError::JsonParse(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn search_traces_http_500_surfaces_http_status() {
        let (endpoint, server) = spawn_one_shot_server(http_status(500, "Internal")).await;
        let client = http_client::build_client();
        let err = search_traces(&client, &endpoint, "foo-svc", Duration::from_mins(1), 10)
            .await
            .expect_err("500 must surface as HttpStatus");
        match err {
            TempoError::HttpStatus { status: 500, .. } => {}
            other => panic!("expected HttpStatus {{ status: 500, .. }}, got {other:?}"),
        }
        server.await.unwrap();
    }

    // --- ingest_from_tempo: end-to-end search+fetch flow ---

    #[tokio::test]
    async fn ingest_from_tempo_search_then_fetch_aggregates_events() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // The mock must handle MULTIPLE connections in sequence:
        //   1. /api/search → return one trace ID
        //   2. /api/traces/<id> → return an empty OTLP protobuf
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

        let err = ingest_from_tempo(&endpoint, Some("foo-svc"), None, Duration::from_mins(5), 5)
            .await
            .expect_err("empty trace must surface as NoTracesFound after loop");
        // The trace was fetched successfully but contained zero spans,
        // so the aggregated result is empty → NoTracesFound at the end.
        assert!(matches!(err, TempoError::NoTracesFound));
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
        assert!(format!("{e1}").contains("endpoint"));
        assert!(format!("{e2}").contains("transport") || format!("{e2}").contains("Transport"));
        assert!(format!("{e3}").contains("body"));
        assert!(format!("{e4}").contains("418"));
        assert!(format!("{e5}").contains("timed out"));
        assert!(format!("{e6}").contains("JSON"));
        assert!(format!("{e7}").contains("protobuf") || format!("{e7}").contains("Protobuf"));
        assert!(format!("{e8}").contains("not found") || format!("{e8}").contains("Not found"));
        assert!(format!("{e9}").contains("no traces") || format!("{e9}").contains("No traces"));
    }
}
