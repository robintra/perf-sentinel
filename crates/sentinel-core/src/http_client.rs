//! Shared HTTP(S) client utilities for scrapers and API clients.
//!
//! Provides a TLS-capable hyper client, body size limits and endpoint
//! redaction. Used by the Scaphandre scraper, cloud energy scraper,
//! Electricity Maps scraper, Tempo ingestion module and the `query`
//! CLI subcommand.

/// Re-export `hyper::Uri` so callers don't need a direct hyper dependency.
pub use hyper::Uri;

/// Auth header name shared between every surface that emits or consumes
/// it: the daemon's inbound ack-auth check (`daemon::query_api::check_ack_auth`),
/// the outbound HTTP client below (Tempo / Electricity Maps / daemon CLI),
/// and the HTML report's live-mode `fetchWithAuth` helper. Centralized
/// here because `http_client` is enabled under both the `daemon` and
/// `tempo` features, so a single source-of-truth covers every build
/// variant. A drift-guard test in `report::html` asserts the template
/// references this exact constant.
pub const API_KEY_HEADER: &str = "X-API-Key";

/// Hyper-util legacy client with TLS support via rustls.
///
/// Supports both `http://` and `https://` endpoints. Built once per
/// task via [`build_client`] and reused across requests so the
/// underlying connection pool stays warm.
pub type HttpClient = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Empty<bytes::Bytes>,
>;

/// Sibling of [`HttpClient`] for requests carrying a body (POST, DELETE
/// with payload). The request body type is pinned at the client builder,
/// so a separate alias is needed when callers want to send `Full<Bytes>`
/// rather than `Empty<Bytes>`.
pub type HttpClientWithBody = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<bytes::Bytes>,
>;

/// Maximum response body size accepted from scrape endpoints.
///
/// 8 MiB is generous: real scrape responses are typically <1 MiB.
/// The cap prevents a misbehaving endpoint from exhausting RAM.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Build a hyper-util client over the given request body type. Private
/// generic so the TLS configuration lives in one place and cannot drift
/// between [`build_client`] and [`build_client_with_body`].
fn build_client_inner<B>() -> hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    B,
>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
{
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}

/// Build a fresh hyper-util client with TLS support. Called once per
/// task at startup; the client is then reused for every fetch.
///
/// Uses rustls with Mozilla root certificates (webpki-roots) for
/// HTTPS endpoints. Plain HTTP endpoints also work.
#[must_use]
pub fn build_client() -> HttpClient {
    build_client_inner::<http_body_util::Empty<bytes::Bytes>>()
}

/// Sibling of [`build_client`] for [`HttpClientWithBody`]. Same TLS
/// configuration, only the request body type differs.
#[must_use]
pub fn build_client_with_body() -> HttpClientWithBody {
    build_client_inner::<http_body_util::Full<bytes::Bytes>>()
}

/// Strip userinfo (`http://user:pass@host/`) from a `Uri` before
/// logging. Rebuilds the URL with only scheme, host, port and path.
pub fn redact_endpoint(uri: &Uri) -> String {
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = uri.host().unwrap_or("?");
    let path_and_query = uri.path_and_query().map_or("/", |p| p.as_str());
    if let Some(port) = uri.port_u16() {
        format!("{scheme}://{host}:{port}{path_and_query}")
    } else {
        format!("{scheme}://{host}{path_and_query}")
    }
}

/// Errors from [`fetch_get`]. Uses the same variants that the individual
/// scrapers had independently, now unified so callers `.map_err()` into
/// their domain-specific error type with a one-liner.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchError {
    #[error("failed to build HTTP request")]
    RequestBuild(#[source] hyper::http::Error),
    #[error("HTTP transport error")]
    Transport(#[source] hyper_util::client::legacy::Error),
    #[error("body read failed: {0}")]
    BodyRead(String),
    #[error("endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("request timed out")]
    Timeout,
}

/// Perform a `GET` request with a timeout and body size cap.
///
/// Returns the response body as raw bytes. Shared by the Scaphandre,
/// cloud energy and Electricity Maps scrapers so the fetch/timeout/
/// body-cap logic lives in one place.
///
/// When `auth` is `Some`, the parsed header is attached to the
/// request. The value is already marked `sensitive` by
/// [`crate::ingest::auth_header::AuthHeader::parse`], so hyper
/// redacts it from debug output and HPACK tables.
///
/// # Errors
///
/// Returns [`FetchError`] on request build failure, transport error,
/// non-2xx status, body read failure or timeout.
pub async fn fetch_get(
    client: &HttpClient,
    uri: &Uri,
    user_agent: &str,
    timeout: std::time::Duration,
    auth: Option<&crate::ingest::auth_header::AuthHeader>,
) -> Result<bytes::Bytes, FetchError> {
    use http_body_util::{BodyExt, Empty, Limited};

    let mut builder = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(uri.clone())
        .header(hyper::header::USER_AGENT, user_agent);
    if let Some(auth) = auth {
        builder = builder.header(&auth.name, &auth.value);
    }
    let req = builder
        .body(Empty::<bytes::Bytes>::new())
        .map_err(FetchError::RequestBuild)?;

    let response = tokio::time::timeout(timeout, client.request(req))
        .await
        .map_err(|_| FetchError::Timeout)?
        .map_err(FetchError::Transport)?;

    if !response.status().is_success() {
        return Err(FetchError::HttpStatus(response.status().as_u16()));
    }

    let limited = Limited::new(response.into_body(), MAX_BODY_BYTES);
    let collected = limited
        .collect()
        .await
        .map_err(|e| FetchError::BodyRead(format!("{e}")))?;
    Ok(collected.to_bytes())
}

/// Perform a request that carries a body (typically POST or DELETE)
/// and returns both the status code and the raw response body, without
/// failing on non-2xx. Used by the `perf-sentinel ack` CLI which needs
/// to discriminate 401 / 409 / 503 to map them onto exit codes and
/// hint messages.
///
/// `api_key`, when `Some`, is attached as the `X-API-Key` header (the
/// daemon's auth scheme, cf `crates/sentinel-core/src/daemon/query_api.rs`
/// `check_ack_auth`). `body` may be empty for DELETE.
///
/// # Errors
///
/// Returns [`FetchError`] on request build failure, transport error,
/// timeout or body read failure. Non-2xx statuses are not errors here,
/// they are returned to the caller as the first tuple element.
pub async fn fetch_with_body(
    client: &HttpClientWithBody,
    method: hyper::Method,
    uri: &Uri,
    user_agent: &str,
    timeout: std::time::Duration,
    api_key: Option<&str>,
    body: bytes::Bytes,
) -> Result<(hyper::StatusCode, bytes::Bytes), FetchError> {
    use http_body_util::{BodyExt, Full, Limited};

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(uri.clone())
        .header(hyper::header::USER_AGENT, user_agent)
        .header(hyper::header::CONTENT_TYPE, "application/json");
    if let Some(key) = api_key {
        // Build the header value explicitly so we can flag it
        // sensitive: hyper redacts sensitive values from Debug output
        // and HPACK tables, mirroring the AuthHeader pattern used by
        // [`fetch_get`].
        let mut value = hyper::header::HeaderValue::from_str(key)
            .map_err(|e| FetchError::RequestBuild(e.into()))?;
        value.set_sensitive(true);
        builder = builder.header(API_KEY_HEADER, value);
    }
    let req = builder
        .body(Full::new(body))
        .map_err(FetchError::RequestBuild)?;

    let response = tokio::time::timeout(timeout, client.request(req))
        .await
        .map_err(|_| FetchError::Timeout)?
        .map_err(FetchError::Transport)?;

    let status = response.status();
    let limited = Limited::new(response.into_body(), MAX_BODY_BYTES);
    let collected = limited
        .collect()
        .await
        .map_err(|e| FetchError::BodyRead(format!("{e}")))?;
    Ok((status, collected.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_client_constructs_without_panic() {
        // `build_client()` wires hyper-rustls + the Tokio executor.
        // The return type is opaque, so we cannot inspect fields, but a
        // panic-free construction is the main property we care about:
        // a regression in the hyper-rustls builder surface (renamed
        // method, missing feature) would blow up here.
        let _client: HttpClient = build_client();
    }

    #[test]
    fn redact_endpoint_strips_credentials_with_default_port() {
        // Userinfo (`user:pass@`) is dropped by `hyper::Uri::host()`,
        // which is exactly what we want: no chance of leaking secrets
        // into logs via the rebuilt URL.
        let uri: Uri = "http://user:pass@example.com/metrics".parse().unwrap();
        assert_eq!(redact_endpoint(&uri), "http://example.com/metrics");
    }

    #[test]
    fn redact_endpoint_preserves_explicit_port() {
        let uri: Uri = "http://metrics.local:9090/metrics".parse().unwrap();
        assert_eq!(redact_endpoint(&uri), "http://metrics.local:9090/metrics");
    }

    #[test]
    fn redact_endpoint_preserves_https_scheme() {
        let uri: Uri = "https://api.electricitymap.org/v3/carbon-intensity/latest?zone=FR"
            .parse()
            .unwrap();
        let redacted = redact_endpoint(&uri);
        assert!(redacted.starts_with("https://api.electricitymap.org"));
        assert!(redacted.contains("zone=FR"));
    }

    #[test]
    fn redact_endpoint_strips_credentials_with_explicit_port() {
        let uri: Uri = "http://admin:secret@localhost:8080/scrape".parse().unwrap();
        // Only the userinfo must be gone; the port must stay.
        let redacted = redact_endpoint(&uri);
        assert_eq!(redacted, "http://localhost:8080/scrape");
        assert!(!redacted.contains("admin"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn redact_endpoint_handles_root_path() {
        let uri: Uri = "http://host/".parse().unwrap();
        assert_eq!(redact_endpoint(&uri), "http://host/");
    }

    /// Real HTTP round-trip against a one-shot mock server. This is the
    /// only way to catch regressions where the `https_or_http()`-built
    /// connector would refuse plain HTTP (e.g., a misconfigured rustls
    /// builder or a feature-flag drift on `hyper-rustls`). The mock
    /// server is hand-rolled to avoid pulling in wiremock / httptest
    /// just for a smoke test, same pattern as the scraper test modules.
    #[tokio::test]
    async fn build_client_can_perform_plain_http_round_trip() {
        use http_body_util::{BodyExt, Empty, Limited};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = format!("http://{addr}/");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = "HTTP/1.1 200 OK\r\n\
                            Content-Type: text/plain\r\n\
                            Content-Length: 5\r\n\
                            Connection: close\r\n\
                            \r\n\
                            hello";
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });

        let client = build_client();
        let uri: Uri = endpoint.parse().unwrap();
        let req = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(&uri)
            .header(hyper::header::USER_AGENT, "perf-sentinel-test")
            .body(Empty::<bytes::Bytes>::new())
            .unwrap();

        let resp = client
            .request(req)
            .await
            .expect("round-trip should succeed");
        assert_eq!(resp.status().as_u16(), 200);
        let body = Limited::new(resp.into_body(), MAX_BODY_BYTES)
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&body[..], b"hello");
        server.await.unwrap();
    }

    /// Confirms that when an `AuthHeader` is passed, the header name and
    /// value land on the request wire. Uses the shared one-shot TCP
    /// listener + mpsc-capture pattern so the assertion is byte-exact.
    #[tokio::test]
    async fn fetch_get_attaches_auth_header() {
        use crate::ingest::auth_header::AuthHeader;

        let response = b"HTTP/1.1 200 OK\r\n\
                         Content-Type: text/plain\r\n\
                         Content-Length: 2\r\n\
                         Connection: close\r\n\
                         \r\n\
                         ok"
        .to_vec();
        let (endpoint, mut rx, server) = crate::test_helpers::spawn_capture_server(response).await;

        let client = build_client();
        let uri: Uri = format!("{endpoint}/").parse().expect("uri");
        let auth = AuthHeader::parse("Authorization: Bearer topsecret").expect("valid");
        let bytes = fetch_get(
            &client,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(5),
            Some(&auth),
        )
        .await
        .expect("fetch_get must succeed");
        assert_eq!(&bytes[..], b"ok");

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).expect("utf8");
        assert!(
            text.contains("authorization: Bearer topsecret")
                || text.contains("Authorization: Bearer topsecret"),
            "auth header missing from request, got:\n{text}"
        );
        server.await.expect("server join");
    }

    #[test]
    fn build_client_with_body_constructs_without_panic() {
        let _client: HttpClientWithBody = build_client_with_body();
    }

    #[tokio::test]
    async fn fetch_with_body_returns_status_and_body_on_201() {
        let response = crate::test_helpers::http_status(201, "Created");
        let (endpoint, _rx, server) = crate::test_helpers::spawn_capture_server(response).await;
        let client = build_client_with_body();
        let uri: Uri = format!("{endpoint}/api/findings/sig/ack").parse().unwrap();
        let (status, body) = fetch_with_body(
            &client,
            hyper::Method::POST,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(5),
            None,
            bytes::Bytes::from_static(b"{}"),
        )
        .await
        .expect("call must succeed");
        assert_eq!(status.as_u16(), 201);
        assert!(body.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_with_body_surfaces_409_without_erroring() {
        let response = crate::test_helpers::http_status(409, "Conflict");
        let (endpoint, _rx, server) = crate::test_helpers::spawn_capture_server(response).await;
        let client = build_client_with_body();
        let uri: Uri = format!("{endpoint}/api/findings/sig/ack").parse().unwrap();
        let (status, _) = fetch_with_body(
            &client,
            hyper::Method::POST,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(5),
            None,
            bytes::Bytes::from_static(b"{}"),
        )
        .await
        .expect("non-2xx must not produce an error");
        assert_eq!(status.as_u16(), 409);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_with_body_attaches_x_api_key_header() {
        let response = crate::test_helpers::http_status(204, "No Content");
        let (endpoint, mut rx, server) = crate::test_helpers::spawn_capture_server(response).await;
        let client = build_client_with_body();
        let uri: Uri = format!("{endpoint}/api/findings/sig/ack").parse().unwrap();
        let (status, _) = fetch_with_body(
            &client,
            hyper::Method::DELETE,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(5),
            Some("secret123"),
            bytes::Bytes::new(),
        )
        .await
        .expect("call must succeed");
        assert_eq!(status.as_u16(), 204);

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).unwrap();
        assert!(
            text.contains("x-api-key: secret123") || text.contains("X-API-Key: secret123"),
            "X-API-Key header missing, got:\n{text}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_with_body_sends_content_type_json() {
        let response = crate::test_helpers::http_status(201, "Created");
        let (endpoint, mut rx, server) = crate::test_helpers::spawn_capture_server(response).await;
        let client = build_client_with_body();
        let uri: Uri = format!("{endpoint}/api/findings/sig/ack").parse().unwrap();
        let _ = fetch_with_body(
            &client,
            hyper::Method::POST,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(5),
            None,
            bytes::Bytes::from_static(br#"{"reason":"x"}"#),
        )
        .await
        .expect("call must succeed");

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).unwrap();
        assert!(
            text.to_ascii_lowercase()
                .contains("content-type: application/json"),
            "Content-Type header missing, got:\n{text}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn fetch_with_body_sends_request_body() {
        let response = crate::test_helpers::http_status(201, "Created");
        let (endpoint, mut rx, server) = crate::test_helpers::spawn_capture_server(response).await;
        let client = build_client_with_body();
        let uri: Uri = format!("{endpoint}/api/findings/sig/ack").parse().unwrap();
        let payload = br#"{"by":"alice","reason":"deferred"}"#;
        let _ = fetch_with_body(
            &client,
            hyper::Method::POST,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(5),
            None,
            bytes::Bytes::from_static(payload),
        )
        .await
        .expect("call must succeed");

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).unwrap();
        assert!(
            text.contains(r#"{"by":"alice","reason":"deferred"}"#),
            "request body missing, got:\n{text}"
        );
        server.await.unwrap();
    }
}
