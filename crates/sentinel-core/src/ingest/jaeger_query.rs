//! Jaeger query API ingestion: query any backend that speaks the
//! Jaeger query HTTP API for traces. Covers Jaeger upstream and
//! Victoria Traces (which implements the same API surface).
//!
//! Unlike Tempo's `/api/search` (returns trace IDs only, each trace
//! fetched separately), Jaeger's `/api/traces` returns full traces in
//! the search response, so one HTTP round trip covers the entire
//! ingestion. The payload shape is shared with the file-mode `jaeger`
//! parser: `{"data": [{"traceID": ..., "spans": [...], "processes": {...}}]}`.

use std::time::Duration;

use crate::event::SpanEvent;
use crate::http_client::{self, HttpClient};
use crate::ingest::jaeger::{JaegerExport, convert_jaeger_export};
use crate::ingest::lookback::{self, LookbackError};

// ---------------------------------------------------------------
// Error type
// ---------------------------------------------------------------

/// Errors from Jaeger query API interactions.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JaegerQueryError {
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("invalid lookback duration: {0}")]
    InvalidLookback(String),

    #[error("HTTP transport error: {0}")]
    Transport(String),

    #[error("backend returned HTTP {status} for {url}")]
    HttpStatus { status: u16, url: String },

    #[error("request timed out")]
    Timeout,

    #[error("failed to read response body: {0}")]
    BodyRead(String),

    #[error("failed to parse JSON response: {0}")]
    JsonParse(String),

    #[error("trace not found: {0}")]
    TraceNotFound(String),

    #[error("no traces found for the given search criteria")]
    NoTracesFound,
}

impl From<LookbackError> for JaegerQueryError {
    fn from(e: LookbackError) -> Self {
        JaegerQueryError::InvalidLookback(e.to_string())
    }
}

// ---------------------------------------------------------------
// Lookback parser (thin wrapper around shared helper)
// ---------------------------------------------------------------

/// Parse a human-readable lookback duration string like `"1h"`, `"30m"`.
///
/// # Errors
///
/// Returns `JaegerQueryError::InvalidLookback` for malformed inputs.
pub fn parse_lookback(s: &str) -> Result<Duration, JaegerQueryError> {
    lookback::parse(s).map_err(Into::into)
}

// ---------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------

/// Minimal percent-encoding for URI query parameter values. Duplicated
/// from `tempo.rs` to avoid a `percent-encoding` dep for 12 lines.
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
// HTTP constants
// ---------------------------------------------------------------

/// Maximum body size for Jaeger query responses (256 MiB). Larger than
/// Tempo's per-trace cap because `/api/traces` returns full traces for
/// every hit in a single response. A `limit=500` multi-trace search
/// can legitimately push into the tens of megabytes.
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Request timeout for `/api/traces` and `/api/traces/{id}`. The
/// search endpoint may scan a non-trivial index on the backend, we
/// stay generous at 60 seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

async fn fetch_json(
    client: &HttpClient,
    uri: hyper::Uri,
    map_404: bool,
) -> Result<String, JaegerQueryError> {
    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(&uri)
        .header("Accept", "application/json")
        .header("User-Agent", "perf-sentinel")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| JaegerQueryError::Transport(e.to_string()))?;

    let resp = tokio::time::timeout(REQUEST_TIMEOUT, client.request(req))
        .await
        .map_err(|_| JaegerQueryError::Timeout)?
        .map_err(|e| JaegerQueryError::Transport(e.to_string()))?;

    let status = resp.status().as_u16();
    if map_404 && status == 404 {
        return Err(JaegerQueryError::TraceNotFound(
            http_client::redact_endpoint(&uri),
        ));
    }
    if status != 200 {
        return Err(JaegerQueryError::HttpStatus {
            status,
            url: http_client::redact_endpoint(&uri),
        });
    }

    let limited = http_body_util::Limited::new(resp.into_body(), MAX_RESPONSE_BYTES);
    let body = http_body_util::BodyExt::collect(limited)
        .await
        .map_err(|e| JaegerQueryError::BodyRead(e.to_string()))?
        .to_bytes();

    String::from_utf8(body.to_vec()).map_err(|e| JaegerQueryError::BodyRead(e.to_string()))
}

// ---------------------------------------------------------------
// Core API functions
// ---------------------------------------------------------------

/// Search a Jaeger query backend for traces matching a service name
/// within a lookback window. Returns the full `SpanEvent` list directly
/// because the Jaeger query API returns full traces in the search
/// response (unlike Tempo which returns ID-only summaries).
///
/// # Errors
///
/// Returns `JaegerQueryError` on HTTP errors, timeouts, or JSON parse failures.
pub async fn search_traces(
    client: &HttpClient,
    endpoint: &str,
    service: &str,
    lookback: Duration,
    limit: usize,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    let encoded_service = percent_encode_query_value(service);
    let lookback_secs = lookback.as_secs();
    let uri_str = format!(
        "{endpoint}/api/traces?service={encoded_service}&lookback={lookback_secs}s&limit={limit}"
    );
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|_| JaegerQueryError::InvalidEndpoint(endpoint.to_string()))?;

    let json = fetch_json(client, uri, false).await?;

    let export: JaegerExport =
        serde_json::from_str(&json).map_err(|e| JaegerQueryError::JsonParse(e.to_string()))?;

    if export.data.is_empty() {
        return Err(JaegerQueryError::NoTracesFound);
    }

    Ok(convert_jaeger_export(&export))
}

/// Fetch a single trace by ID from a Jaeger query backend.
///
/// # Errors
///
/// Returns `JaegerQueryError` on HTTP errors, timeouts, or JSON parse failures.
pub async fn fetch_trace(
    client: &HttpClient,
    endpoint: &str,
    trace_id: &str,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    // Jaeger and Victoria Traces both accept hex-encoded trace IDs of
    // 16 or 32 chars. Reject empty or non-hex strings early so we do
    // not burn an HTTP round trip on an obviously bad input.
    if trace_id.is_empty() || !trace_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(JaegerQueryError::InvalidEndpoint(format!(
            "trace ID '{trace_id}' is empty or contains non-hex characters"
        )));
    }

    let uri_str = format!("{endpoint}/api/traces/{trace_id}");
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|_| JaegerQueryError::InvalidEndpoint(endpoint.to_string()))?;

    let json = fetch_json(client, uri, true).await?;

    let export: JaegerExport =
        serde_json::from_str(&json).map_err(|e| JaegerQueryError::JsonParse(e.to_string()))?;

    Ok(convert_jaeger_export(&export))
}

/// Ingest traces from a Jaeger query API backend: either a single
/// trace by ID or a service-scoped search. Covers Jaeger upstream
/// and Victoria Traces.
///
/// # Errors
///
/// Returns `JaegerQueryError` on API failures.
pub async fn ingest_from_jaeger_query(
    endpoint: &str,
    service: Option<&str>,
    trace_id: Option<&str>,
    lookback: Duration,
    max_traces: usize,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    validate_endpoint(endpoint)?;

    let client = http_client::build_client();

    if let Some(tid) = trace_id {
        tracing::info!(
            trace_id = tid,
            "Fetching single trace from Jaeger query API"
        );
        return fetch_trace(&client, endpoint, tid).await;
    }

    let svc = service.ok_or_else(|| {
        JaegerQueryError::InvalidEndpoint("either --trace-id or --service is required".to_string())
    })?;

    tracing::info!(
        service = svc,
        lookback_secs = lookback.as_secs(),
        max_traces,
        "Querying Jaeger API for traces"
    );

    search_traces(&client, endpoint, svc, lookback, max_traces).await
}

/// Endpoint validation with the same rules as `tempo`: must be
/// http:// or https://, must not embed credentials in the authority.
fn validate_endpoint(endpoint: &str) -> Result<(), JaegerQueryError> {
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(JaegerQueryError::InvalidEndpoint(format!(
            "endpoint must start with http:// or https://, got '{endpoint}'"
        )));
    }
    let after_scheme = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or("");
    let authority_end = after_scheme.find(['/', '?']).unwrap_or(after_scheme.len());
    if after_scheme[..authority_end].contains('@') {
        return Err(JaegerQueryError::InvalidEndpoint(
            "endpoint must not contain credentials (user:pass@host)".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{http_200_text, http_status, spawn_one_shot_server};

    fn http_200_json(body: &str) -> Vec<u8> {
        http_200_text("application/json", body)
    }

    const SAMPLE_TRACE: &str = r#"{
        "data": [{
            "traceID": "abc123",
            "spans": [{
                "spanID": "span-1",
                "operationName": "query",
                "references": [],
                "startTime": 1720621921123000,
                "duration": 1200,
                "processID": "p1",
                "tags": [
                    { "key": "db.statement", "value": "SELECT 1" },
                    { "key": "db.system", "value": "postgresql" }
                ]
            }],
            "processes": {
                "p1": { "serviceName": "order-svc" }
            }
        }]
    }"#;

    #[test]
    fn parse_lookback_wraps_shared_helper() {
        assert_eq!(parse_lookback("1h").unwrap(), Duration::from_hours(1));
        let err = parse_lookback("").expect_err("empty must fail");
        assert!(matches!(err, JaegerQueryError::InvalidLookback(_)));
    }

    #[tokio::test]
    async fn search_traces_returns_span_events() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(SAMPLE_TRACE)).await;
        let client = http_client::build_client();
        let events = search_traces(&client, &endpoint, "order-svc", Duration::from_mins(1), 10)
            .await
            .expect("search must succeed");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].service, "order-svc");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn search_empty_data_surfaces_no_traces_found() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(r#"{"data":[]}"#)).await;
        let client = http_client::build_client();
        let err = search_traces(&client, &endpoint, "order-svc", Duration::from_mins(1), 10)
            .await
            .expect_err("empty search must surface NoTracesFound");
        assert!(matches!(err, JaegerQueryError::NoTracesFound));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn search_http_500_surfaces_http_status() {
        let (endpoint, server) = spawn_one_shot_server(http_status(500, "Internal")).await;
        let client = http_client::build_client();
        let err = search_traces(&client, &endpoint, "svc", Duration::from_mins(1), 10)
            .await
            .expect_err("500 must surface HttpStatus");
        assert!(matches!(
            err,
            JaegerQueryError::HttpStatus { status: 500, .. }
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn search_malformed_json_surfaces_json_parse() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json("not json")).await;
        let client = http_client::build_client();
        let err = search_traces(&client, &endpoint, "svc", Duration::from_mins(1), 10)
            .await
            .expect_err("malformed JSON must surface JsonParse");
        assert!(matches!(err, JaegerQueryError::JsonParse(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_trace_returns_span_events() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(SAMPLE_TRACE)).await;
        let client = http_client::build_client();
        let events = fetch_trace(&client, &endpoint, "abc123")
            .await
            .expect("fetch must succeed");
        assert_eq!(events.len(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_trace_404_surfaces_trace_not_found() {
        let (endpoint, server) = spawn_one_shot_server(http_status(404, "Not Found")).await;
        let client = http_client::build_client();
        let err = fetch_trace(&client, &endpoint, "abc123")
            .await
            .expect_err("404 must surface TraceNotFound");
        assert!(matches!(err, JaegerQueryError::TraceNotFound(_)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_trace_rejects_non_hex_id() {
        let client = http_client::build_client();
        let err = fetch_trace(&client, "http://jaeger.local", "not-hex!")
            .await
            .expect_err("non-hex must be rejected");
        assert!(matches!(err, JaegerQueryError::InvalidEndpoint(_)));
    }

    #[tokio::test]
    async fn ingest_rejects_non_http_scheme() {
        let err = ingest_from_jaeger_query(
            "ftp://jaeger.local",
            Some("svc"),
            None,
            Duration::from_mins(1),
            10,
        )
        .await
        .expect_err("non-http must be rejected");
        assert!(matches!(err, JaegerQueryError::InvalidEndpoint(_)));
    }

    #[tokio::test]
    async fn ingest_rejects_credentials_in_endpoint() {
        let err = ingest_from_jaeger_query(
            "http://user:pass@jaeger.local",
            None,
            Some("abc"),
            Duration::from_mins(1),
            10,
        )
        .await
        .expect_err("credentials must be rejected");
        match err {
            JaegerQueryError::InvalidEndpoint(msg) => assert!(msg.contains("credentials")),
            other => panic!("expected InvalidEndpoint, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ingest_rejects_missing_service_and_trace_id() {
        let err = ingest_from_jaeger_query(
            "http://jaeger.local",
            None,
            None,
            Duration::from_mins(1),
            10,
        )
        .await
        .expect_err("missing both must be rejected");
        assert!(matches!(err, JaegerQueryError::InvalidEndpoint(_)));
    }

    #[tokio::test]
    async fn ingest_search_end_to_end() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(SAMPLE_TRACE)).await;
        let events = ingest_from_jaeger_query(
            &endpoint,
            Some("order-svc"),
            None,
            Duration::from_mins(1),
            5,
        )
        .await
        .expect("end-to-end search must succeed");
        assert_eq!(events.len(), 1);
        server.await.unwrap();
    }
}
