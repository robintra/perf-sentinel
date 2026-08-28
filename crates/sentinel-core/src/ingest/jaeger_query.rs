//! Jaeger query API ingestion: query any backend that speaks the
//! Jaeger query HTTP API for traces. Covers Jaeger upstream and
//! Victoria Traces (which implements the same API surface).
//!
//! Unlike Tempo's `/api/search` (returns trace IDs only, each trace
//! fetched separately), Jaeger's `/api/traces` returns full traces in
//! the search response, so one HTTP round trip covers the entire
//! ingestion. The payload shape is shared with the file-mode `jaeger`
//! parser: `{"data": [{"traceID": ..., "spans": [...], "processes": {...}}]}`.
//!
//! # Security
//!
//! The endpoint validator accepts any `http(s)` URL without
//! credentials. It does NOT block RFC 1918 or link-local targets, so
//! the subcommand must only be invoked with trusted endpoint values.
//! Contexts that relay user-provided endpoints (for example CI
//! pipelines driven by external PRs) should sanitize the input
//! upstream. See `docs/LIMITATIONS.md` for the full caveat list.

use std::sync::Arc;
use std::time::Duration;

use crate::event::SpanEvent;
use crate::http_client::{self, HttpClient};
use crate::ingest::auth_header::AuthHeader;
use crate::ingest::jaeger::{JaegerExport, convert_jaeger_export};
use crate::ingest::lookback::{SearchWindow, WindowError};
use crate::ingest::url_enc::{percent_encode_query_value, validate_http_endpoint};

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

    #[error("invalid trace ID: {0}")]
    InvalidTraceId(String),

    #[error("missing required argument: {0}")]
    MissingArgument(String),

    #[error("invalid search window: {0}")]
    InvalidWindow(#[from] WindowError),

    #[error("invalid auth header: {0}")]
    InvalidAuthHeader(String),

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

// ---------------------------------------------------------------
// HTTP constants
// ---------------------------------------------------------------

/// Maximum body size for Jaeger query responses (256 MiB). Larger than
/// Tempo's per-trace cap because `/api/traces` returns full traces for
/// every hit in a single response. A `limit=500` multi-trace search
/// can legitimately push into the tens of megabytes.
const MAX_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

/// Size at which we emit a `tracing::info!` log flagging that the
/// response is non-trivial. Helps operators reason about transport
/// cost and spot unexpectedly large replies early.
const RESPONSE_BYTES_LOG_THRESHOLD: usize = 16 * 1024 * 1024;

/// End-to-end request timeout bounding both header receive and body
/// read. Kept generous because `/api/traces` may scan a non-trivial
/// index on the backend. `from_mins` is enforced here by the
/// `duration_suboptimal_units` clippy lint, the `tempo` module uses
/// `from_secs` for its sub-minute timeouts where the lint stays quiet.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// Upper bound on the trace-ID length accepted by the hex-only check.
/// Jaeger and Victoria Traces both use 16 or 32 hex chars. The cap
/// makes a hypothetical 10 000-char hex string fail fast before it
/// lands in the URL builder.
const MAX_TRACE_ID_LEN: usize = 128;

// ---------------------------------------------------------------
// HTTP fetch helper
// ---------------------------------------------------------------

/// Fetch a JSON body from the backend with the standard accept
/// headers, size cap, and end-to-end timeout.
///
/// The `tokio::time::timeout` wraps BOTH the `client.request` future
/// (TCP + TLS + headers) AND the body drain, so a backend that sends
/// headers promptly then trickles the body still hits the timeout.
async fn fetch_json(
    client: &HttpClient,
    uri: hyper::Uri,
    auth: Option<&AuthHeader>,
    map_404: bool,
) -> Result<bytes::Bytes, JaegerQueryError> {
    let run = async {
        let mut builder = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(&uri)
            .header("Accept", "application/json")
            .header("User-Agent", "perf-sentinel");
        if let Some(auth) = auth {
            builder = builder.header(&auth.name, &auth.value);
        }
        let req = builder
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .map_err(|e| JaegerQueryError::Transport(e.to_string()))?;

        let resp = client
            .request(req)
            .await
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

        if body.len() >= RESPONSE_BYTES_LOG_THRESHOLD {
            tracing::info!(
                body_bytes = body.len(),
                "Large Jaeger query response received"
            );
        }

        Ok(body)
    };

    tokio::time::timeout(REQUEST_TIMEOUT, run)
        .await
        .map_err(|_| JaegerQueryError::Timeout)?
}

// ---------------------------------------------------------------
// Core API functions
// ---------------------------------------------------------------

/// Search a Jaeger query backend for traces matching a service name
/// within a lookback window, then return the full `SpanEvent` list.
///
/// The Jaeger `/api/traces` endpoint bundles entire span payloads into
/// the search response (unlike Tempo's ID-only `/api/search`), so this
/// one call covers what Tempo would split across `search_traces` plus
/// a per-ID `fetch_trace` fanout. The name is kept symmetric with
/// `tempo::search_traces` even though the returned type differs.
///
/// # Errors
///
/// Returns `JaegerQueryError::InvalidWindow` before any request is issued
/// when the window is empty or inverted, then `JaegerQueryError` on HTTP
/// errors, timeouts, or JSON parse failures.
pub async fn search_and_fetch_traces(
    client: &HttpClient,
    endpoint: &str,
    service: &str,
    window: SearchWindow,
    limit: usize,
    auth: Option<&AuthHeader>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    search_and_fetch_traces_with_grouping(client, endpoint, service, window, limit, auth, None)
        .await
}

#[allow(clippy::too_many_arguments)]
async fn search_and_fetch_traces_with_grouping(
    client: &HttpClient,
    endpoint: &str,
    service: &str,
    window: SearchWindow,
    limit: usize,
    auth: Option<&AuthHeader>,
    grouping_attributes: Option<&[Arc<str>]>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    let encoded_service = percent_encode_query_value(service);
    // Both window kinds send explicit bounds, in the microseconds this API
    // counts in. `lookback` is deliberately not sent: Victoria Traces reads
    // it only on its service-graph endpoint, never on this search, so a
    // relative window used to be dropped and the query ran from the epoch.
    let (start_ms, end_ms) = window.resolve()?;
    let start_us = start_ms.saturating_mul(1000);
    let end_us = end_ms.saturating_mul(1000);
    let uri_str = format!(
        "{endpoint}/api/traces?service={encoded_service}&start={start_us}&end={end_us}&limit={limit}"
    );
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|_| JaegerQueryError::InvalidEndpoint(endpoint.to_string()))?;

    let body = fetch_json(client, uri, auth, false).await?;

    // `serde_json::from_slice` operates directly on `&[u8]`, avoiding
    // the `Bytes -> Vec<u8> -> String` round trip that would double
    // the peak RSS of large multi-trace responses.
    let export: JaegerExport =
        serde_json::from_slice(&body).map_err(|e| JaegerQueryError::JsonParse(e.to_string()))?;

    if export.data.is_empty() {
        return Err(JaegerQueryError::NoTracesFound);
    }

    let events = convert_jaeger_export(&export, grouping_attributes);
    tracing::info!(
        traces = export.data.len(),
        events = events.len(),
        "Jaeger search returned traces"
    );
    Ok(events)
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
    auth: Option<&AuthHeader>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    fetch_trace_with_grouping(client, endpoint, trace_id, auth, None).await
}

async fn fetch_trace_with_grouping(
    client: &HttpClient,
    endpoint: &str,
    trace_id: &str,
    auth: Option<&AuthHeader>,
    grouping_attributes: Option<&[Arc<str>]>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    validate_trace_id(trace_id)?;

    let uri_str = format!("{endpoint}/api/traces/{trace_id}");
    let uri: hyper::Uri = uri_str
        .parse()
        .map_err(|_| JaegerQueryError::InvalidEndpoint(endpoint.to_string()))?;

    let body = fetch_json(client, uri, auth, true).await?;

    let export: JaegerExport =
        serde_json::from_slice(&body).map_err(|e| JaegerQueryError::JsonParse(e.to_string()))?;

    Ok(convert_jaeger_export(&export, grouping_attributes))
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
    window: SearchWindow,
    max_traces: usize,
    auth_header: Option<&str>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    ingest_from_jaeger_query_impl(
        endpoint,
        service,
        trace_id,
        window,
        max_traces,
        auth_header,
        None,
    )
    .await
}

/// Ingest traces using the operator-configured grouping attributes.
///
/// # Errors
///
/// Returns `JaegerQueryError` on API failures.
pub async fn ingest_from_jaeger_query_with_grouping(
    endpoint: &str,
    service: Option<&str>,
    trace_id: Option<&str>,
    window: SearchWindow,
    max_traces: usize,
    auth_header: Option<&str>,
    grouping_attributes: Vec<Arc<str>>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    ingest_from_jaeger_query_impl(
        endpoint,
        service,
        trace_id,
        window,
        max_traces,
        auth_header,
        Some(grouping_attributes.into()),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn ingest_from_jaeger_query_impl(
    endpoint: &str,
    service: Option<&str>,
    trace_id: Option<&str>,
    window: SearchWindow,
    max_traces: usize,
    auth_header: Option<&str>,
    grouping_attributes: Option<Arc<[Arc<str>]>>,
) -> Result<Vec<SpanEvent>, JaegerQueryError> {
    validate_http_endpoint(endpoint)
        .map_err(|msg| JaegerQueryError::InvalidEndpoint(format!("{msg}, got '{endpoint}'")))?;

    // Parse the optional auth header once, reuse the typed form on
    // every request. The value is redacted from tracing output by
    // both the hyper sensitive flag and the manual Debug impl on
    // AuthHeader, so the credential never appears in logs.
    let parsed_auth = auth_header
        .map(AuthHeader::parse)
        .transpose()
        .map_err(|msg| JaegerQueryError::InvalidAuthHeader(msg.to_string()))?;
    if let Some(auth) = parsed_auth.as_ref() {
        tracing::info!(header_name = %auth.name, "Using auth header for Jaeger query requests");
        if endpoint.starts_with("http://") {
            tracing::warn!(
                "Sending auth header over cleartext HTTP, prefer https:// to avoid credential leak"
            );
        }
    }

    let client = http_client::build_client();

    if let Some(tid) = trace_id {
        tracing::info!(
            trace_id = tid,
            "Fetching single trace from Jaeger query API"
        );
        return fetch_trace_with_grouping(
            &client,
            endpoint,
            tid,
            parsed_auth.as_ref(),
            grouping_attributes.as_deref(),
        )
        .await;
    }

    let svc = service.ok_or_else(|| {
        JaegerQueryError::MissingArgument("either --trace-id or --service is required".to_string())
    })?;

    tracing::info!(
        service = svc,
        ?window,
        max_traces,
        "Querying Jaeger API for traces"
    );

    search_and_fetch_traces_with_grouping(
        &client,
        endpoint,
        svc,
        window,
        max_traces,
        parsed_auth.as_ref(),
        grouping_attributes.as_deref(),
    )
    .await
}

/// Check that a trace ID is a non-empty hex string of at most
/// `MAX_TRACE_ID_LEN` characters. Returned errors carry the dedicated
/// `InvalidTraceId` variant so callers can tell this apart from an
/// endpoint-validation failure.
fn validate_trace_id(trace_id: &str) -> Result<(), JaegerQueryError> {
    if trace_id.is_empty() {
        return Err(JaegerQueryError::InvalidTraceId(
            "trace ID is empty".to_string(),
        ));
    }
    if trace_id.len() > MAX_TRACE_ID_LEN {
        return Err(JaegerQueryError::InvalidTraceId(format!(
            "trace ID exceeds {MAX_TRACE_ID_LEN}-character cap ({} chars supplied)",
            trace_id.len()
        )));
    }
    if !trace_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(JaegerQueryError::InvalidTraceId(format!(
            "trace ID '{trace_id}' contains non-hex characters"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{
        http_200_text, http_status, spawn_capture_server, spawn_one_shot_server,
    };
    use core::assert_matches;

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

    #[tokio::test]
    async fn search_traces_returns_span_events() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(SAMPLE_TRACE)).await;
        let client = http_client::build_client();
        let events = search_and_fetch_traces(
            &client,
            &endpoint,
            "order-svc",
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await
        .expect("search must succeed");
        assert_eq!(events.len(), 1);
        assert_eq!(&*events[0].service, "order-svc");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn search_empty_data_surfaces_no_traces_found() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(r#"{"data":[]}"#)).await;
        let client = http_client::build_client();
        let err = search_and_fetch_traces(
            &client,
            &endpoint,
            "order-svc",
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await
        .expect_err("empty search must surface NoTracesFound");
        assert_matches!(err, JaegerQueryError::NoTracesFound);
        server.await.expect("server join");
    }

    /// Victoria Traces reads `lookback` only on its service-graph endpoint,
    /// never on this search, so sending it left the query running from the
    /// Unix epoch. A relative window must send bounds like any other.
    #[tokio::test]
    async fn a_lookback_window_also_sends_explicit_bounds() {
        let (endpoint, mut captured, server) =
            spawn_capture_server(http_200_json(SAMPLE_TRACE)).await;
        let client = http_client::build_client();
        let _ = search_and_fetch_traces(
            &client,
            &endpoint,
            "order-svc",
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await;
        let request = captured.recv().await.expect("captured request");
        let request = String::from_utf8_lossy(&request);
        assert!(request.contains("&start="), "got: {request}");
        assert!(request.contains("&end="), "got: {request}");
        assert!(!request.contains("lookback="), "got: {request}");
        server.await.expect("server join");
    }

    /// The Jaeger query API takes microseconds, not the seconds Tempo takes.
    #[tokio::test]
    async fn an_absolute_window_sends_microsecond_bounds() {
        let (endpoint, mut captured, server) =
            spawn_capture_server(http_200_json(SAMPLE_TRACE)).await;
        let client = http_client::build_client();
        let _ = search_and_fetch_traces(
            &client,
            &endpoint,
            "order-svc",
            SearchWindow::Absolute {
                start_ms: 1_787_838_000_000,
                end_ms: 1_787_839_200_500,
            },
            10,
            None,
        )
        .await;
        let request = captured.recv().await.expect("captured request");
        let request = String::from_utf8_lossy(&request);
        // Delimited on both sides so an extra factor of a thousand cannot
        // slip past a prefix match.
        assert!(
            request.contains("&start=1787838000000000&"),
            "got: {request}"
        );
        // The trailing 500 ms survives as microseconds: this API counts finer
        // than Tempo, and the window is not rounded down to it.
        assert!(request.contains("&end=1787839200500000&"), "got: {request}");
        assert!(!request.contains("lookback="), "got: {request}");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn search_http_500_surfaces_http_status() {
        let (endpoint, server) = spawn_one_shot_server(http_status(500, "Internal")).await;
        let client = http_client::build_client();
        let err = search_and_fetch_traces(
            &client,
            &endpoint,
            "svc",
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await
        .expect_err("500 must surface HttpStatus");
        assert_matches!(err, JaegerQueryError::HttpStatus { status: 500, .. });
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn search_malformed_json_surfaces_json_parse() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json("not json")).await;
        let client = http_client::build_client();
        let err = search_and_fetch_traces(
            &client,
            &endpoint,
            "svc",
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await
        .expect_err("malformed JSON must surface JsonParse");
        assert_matches!(err, JaegerQueryError::JsonParse(_));
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn fetch_trace_returns_span_events() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(SAMPLE_TRACE)).await;
        let client = http_client::build_client();
        let events = fetch_trace(&client, &endpoint, "abc123", None)
            .await
            .expect("fetch must succeed");
        assert_eq!(events.len(), 1);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn fetch_trace_uses_configured_grouping_attributes() {
        let body = SAMPLE_TRACE.replace(
            r#"{ "key": "db.system", "value": "postgresql" }"#,
            r#"{ "key": "db.system", "value": "postgresql" },
                    { "key": "tenant.id", "value": "acme" }"#,
        );
        let (endpoint, server) = spawn_one_shot_server(http_200_json(&body)).await;
        let client = http_client::build_client();
        let grouping = [Arc::from("tenant.id")];

        let events = fetch_trace_with_grouping(&client, &endpoint, "abc123", None, Some(&grouping))
            .await
            .expect("fetch must succeed");
        assert_eq!(events[0].grouping[0].key.as_ref(), "tenant.id");
        assert_eq!(events[0].grouping[0].value.as_ref(), "acme");
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn fetch_trace_404_surfaces_trace_not_found() {
        let (endpoint, server) = spawn_one_shot_server(http_status(404, "Not Found")).await;
        let client = http_client::build_client();
        let err = fetch_trace(&client, &endpoint, "abc123", None)
            .await
            .expect_err("404 must surface TraceNotFound");
        assert_matches!(err, JaegerQueryError::TraceNotFound(_));
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn fetch_trace_rejects_non_hex_id() {
        let client = http_client::build_client();
        let err = fetch_trace(&client, "http://jaeger.local", "not-hex!", None)
            .await
            .expect_err("non-hex must be rejected");
        assert_matches!(err, JaegerQueryError::InvalidTraceId(_));
    }

    #[tokio::test]
    async fn fetch_trace_rejects_empty_id() {
        let client = http_client::build_client();
        let err = fetch_trace(&client, "http://jaeger.local", "", None)
            .await
            .expect_err("empty must be rejected");
        match err {
            JaegerQueryError::InvalidTraceId(msg) => assert!(msg.contains("empty")),
            other => panic!("expected InvalidTraceId, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_trace_rejects_oversized_id() {
        let client = http_client::build_client();
        let oversized = "a".repeat(MAX_TRACE_ID_LEN + 1);
        let err = fetch_trace(&client, "http://jaeger.local", &oversized, None)
            .await
            .expect_err("oversized must be rejected");
        match err {
            JaegerQueryError::InvalidTraceId(msg) => assert!(msg.contains("cap")),
            other => panic!("expected InvalidTraceId, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ingest_rejects_non_http_scheme() {
        let err = ingest_from_jaeger_query(
            "ftp://jaeger.local",
            Some("svc"),
            None,
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await
        .expect_err("non-http must be rejected");
        assert_matches!(err, JaegerQueryError::InvalidEndpoint(_));
    }

    #[tokio::test]
    async fn ingest_rejects_credentials_in_endpoint() {
        let err = ingest_from_jaeger_query(
            "http://user:pass@jaeger.local",
            None,
            Some("abc"),
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
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
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            None,
        )
        .await
        .expect_err("missing both must be rejected");
        assert_matches!(err, JaegerQueryError::MissingArgument(_));
    }

    #[tokio::test]
    async fn ingest_search_end_to_end() {
        let (endpoint, server) = spawn_one_shot_server(http_200_json(SAMPLE_TRACE)).await;
        let events = ingest_from_jaeger_query(
            &endpoint,
            Some("order-svc"),
            None,
            SearchWindow::Lookback(Duration::from_mins(1)),
            5,
            None,
        )
        .await
        .expect("end-to-end search must succeed");
        assert_eq!(events.len(), 1);
        server.await.expect("server join");
    }

    #[tokio::test]
    async fn ingest_rejects_malformed_auth_header() {
        let err = ingest_from_jaeger_query(
            "http://jaeger.local",
            Some("svc"),
            None,
            SearchWindow::Lookback(Duration::from_mins(1)),
            10,
            Some("NoColonHere"),
        )
        .await
        .expect_err("malformed auth header must be rejected");
        assert_matches!(err, JaegerQueryError::InvalidAuthHeader(_));
    }

    /// End-to-end check that a configured `--auth-header` lands on the
    /// request wire. The mock server captures the raw request bytes
    /// and we assert the `Authorization` line is present.
    #[tokio::test]
    async fn search_sends_auth_header_on_wire() {
        let response = http_200_json(SAMPLE_TRACE);
        let (endpoint, mut rx, server) = spawn_capture_server(response).await;

        let events = ingest_from_jaeger_query(
            &endpoint,
            Some("order-svc"),
            None,
            SearchWindow::Lookback(Duration::from_mins(1)),
            5,
            Some("Authorization: Bearer topsecret"),
        )
        .await
        .expect("ingest must succeed");
        assert_eq!(events.len(), 1);

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).expect("utf8");
        assert!(
            text.contains("authorization: Bearer topsecret")
                || text.contains("Authorization: Bearer topsecret"),
            "auth header missing from request, got:\n{text}"
        );
        server.await.expect("server join");
    }
}
