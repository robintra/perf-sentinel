//! Shared HTTP(S) client utilities for scrapers and API clients.
//!
//! Provides a TLS-capable hyper client, body size limits, and endpoint
//! redaction. Used by the Scaphandre scraper, cloud energy scraper,
//! Electricity Maps scraper, and Tempo ingestion module.

/// Hyper-util legacy client with TLS support via rustls.
///
/// Supports both `http://` and `https://` endpoints. Built once per
/// task via [`build_client`] and reused across requests so the
/// underlying connection pool stays warm.
pub(crate) type HttpClient = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Empty<bytes::Bytes>,
>;

/// Maximum response body size accepted from scrape endpoints.
///
/// 8 MiB is generous: real scrape responses are typically <1 MiB.
/// The cap prevents a misbehaving endpoint from exhausting RAM.
pub(crate) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Build a fresh hyper-util client with TLS support. Called once per
/// task at startup; the client is then reused for every fetch.
///
/// Uses rustls with Mozilla root certificates (webpki-roots) for
/// HTTPS endpoints. Plain HTTP endpoints also work.
pub(crate) fn build_client() -> HttpClient {
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}

/// Strip userinfo (`http://user:pass@host/`) from a `Uri` before
/// logging. Rebuilds the URL with only scheme, host, port, and path.
pub(crate) fn redact_endpoint(uri: &hyper::Uri) -> String {
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = uri.host().unwrap_or("?");
    let path_and_query = uri.path_and_query().map_or("/", |p| p.as_str());
    if let Some(port) = uri.port_u16() {
        format!("{scheme}://{host}:{port}{path_and_query}")
    } else {
        format!("{scheme}://{host}{path_and_query}")
    }
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
        let uri: hyper::Uri = "http://user:pass@example.com/metrics".parse().unwrap();
        assert_eq!(redact_endpoint(&uri), "http://example.com/metrics");
    }

    #[test]
    fn redact_endpoint_preserves_explicit_port() {
        let uri: hyper::Uri = "http://metrics.local:9090/metrics".parse().unwrap();
        assert_eq!(redact_endpoint(&uri), "http://metrics.local:9090/metrics");
    }

    #[test]
    fn redact_endpoint_preserves_https_scheme() {
        let uri: hyper::Uri = "https://api.electricitymap.org/v3/carbon-intensity/latest?zone=FR"
            .parse()
            .unwrap();
        let redacted = redact_endpoint(&uri);
        assert!(redacted.starts_with("https://api.electricitymap.org"));
        assert!(redacted.contains("zone=FR"));
    }

    #[test]
    fn redact_endpoint_strips_credentials_with_explicit_port() {
        let uri: hyper::Uri = "http://admin:secret@localhost:8080/scrape".parse().unwrap();
        // Only the userinfo must be gone; the port must stay.
        let redacted = redact_endpoint(&uri);
        assert_eq!(redacted, "http://localhost:8080/scrape");
        assert!(!redacted.contains("admin"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn redact_endpoint_handles_root_path() {
        let uri: hyper::Uri = "http://host/".parse().unwrap();
        assert_eq!(redact_endpoint(&uri), "http://host/");
    }

    /// Real HTTP round-trip against a one-shot mock server. This is the
    /// only way to catch regressions where the `https_or_http()`-built
    /// connector would refuse plain HTTP (e.g., a misconfigured rustls
    /// builder or a feature-flag drift on `hyper-rustls`). The mock
    /// server is hand-rolled to avoid pulling in wiremock / httptest
    /// just for a smoke test — same pattern as the scraper test modules.
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
        let uri: hyper::Uri = endpoint.parse().unwrap();
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
}
