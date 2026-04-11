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
