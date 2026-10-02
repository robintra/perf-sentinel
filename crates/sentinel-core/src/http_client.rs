//! Shared HTTP(S) client utilities for scrapers and API clients.
//!
//! Provides a TLS-capable hyper client, body size limits and endpoint
//! redaction. Used by the Scaphandre scraper, cloud energy scraper,
//! Electricity Maps scraper, Tempo ingestion module and the `query`
//! CLI subcommand.
//!
//! The client follows two environment conventions of corporate networks:
//! an `https://` destination goes through `HTTPS_PROXY` (or `ALL_PROXY`)
//! unless `NO_PROXY` exempts it, over an HTTP `CONNECT` tunnel, and the PEM
//! certificates of `SSL_CERT_FILE` are trusted next to the Mozilla roots.
//! `http://` destinations always connect directly.

/// Re-export `hyper::Uri` so callers don't need a direct hyper dependency.
pub use hyper::Uri;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::client::proxy::matcher::Matcher;
use hyper_util::rt::TokioIo;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

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
    hyper_rustls::HttpsConnector<ProxyConnector>,
    http_body_util::Empty<bytes::Bytes>,
>;

/// Sibling of [`HttpClient`] for requests carrying a body (POST, DELETE
/// with payload). The request body type is pinned at the client builder,
/// so a separate alias is needed when callers want to send `Full<Bytes>`
/// rather than `Empty<Bytes>`.
pub type HttpClientWithBody = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<ProxyConnector>,
    http_body_util::Full<bytes::Bytes>,
>;

/// Maximum response body size accepted from scrape endpoints.
///
/// 8 MiB is generous: real scrape responses are typically <1 MiB.
/// The cap prevents a misbehaving endpoint from exhausting RAM.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// TCP connector that reaches an `https://` destination through the proxy
/// the environment names for it, with an HTTP `CONNECT` tunnel, and every
/// other destination directly. TLS is then negotiated end to end with the
/// destination, inside the tunnel.
#[derive(Clone)]
pub struct ProxyConnector {
    http: HttpConnector,
    proxy: Arc<Matcher>,
}

impl std::fmt::Debug for ProxyConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConnector").finish_non_exhaustive()
    }
}

impl tower::Service<Uri> for ProxyConnector {
    type Response = TokioIo<tokio::net::TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.http.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        // Plain `http://` stays direct: those calls reach internal services,
        // and a cluster-wide `HTTP_PROXY` must not divert them.
        let intercept = if dst.scheme() == Some(&hyper::http::uri::Scheme::HTTPS) {
            self.proxy.intercept(&dst)
        } else {
            None
        };
        match intercept {
            // The tunnel speaks plaintext HTTP to the proxy, so only an
            // `http://` proxy may receive it, credentials included.
            Some(intercept)
                if intercept.uri().scheme() == Some(&hyper::http::uri::Scheme::HTTP) =>
            {
                let mut tunnel = Tunnel::new(intercept.uri().clone(), self.http.clone());
                if let Some(auth) = intercept.basic_auth() {
                    tunnel = tunnel.with_auth(auth.clone());
                }
                let tunneling = tunnel.call(dst);
                return Box::pin(async move { tunneling.await.map_err(Into::into) });
            }
            Some(intercept) => warn_unsupported_proxy(intercept.uri()),
            None => {}
        }
        let connecting = self.http.call(dst);
        Box::pin(async move { connecting.await.map_err(Into::into) })
    }
}

/// A SOCKS or `https://` proxy URL is not tunneled: say so once, without the
/// URL (it may carry credentials), and let the call connect directly.
fn warn_unsupported_proxy(proxy: &Uri) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let scheme = proxy.scheme_str().unwrap_or("").to_string();
    WARNED.call_once(|| {
        tracing::warn!(
            scheme,
            "HTTPS_PROXY / ALL_PROXY: only http:// proxy URLs are supported, connecting directly"
        );
    });
}

/// The Mozilla roots, plus the certificates of `SSL_CERT_FILE`.
fn root_store(extra: &[CertificateDer<'static>]) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let (_, ignored) = roots.add_parsable_certificates(extra.iter().cloned());
    if ignored > 0 {
        tracing::warn!(
            ignored,
            "SSL_CERT_FILE: certificates rustls cannot use as roots were skipped"
        );
    }
    roots
}

/// Every certificate of a PEM bundle, or none when it cannot be read.
fn load_pem_roots(path: &Path) -> Vec<CertificateDer<'static>> {
    let certs: Vec<_> = match CertificateDer::pem_file_iter(path) {
        Ok(iter) => iter.filter_map(Result::ok).collect(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "SSL_CERT_FILE could not be read, only the bundled roots are trusted");
            return Vec::new();
        }
    };
    if certs.is_empty() {
        tracing::warn!(path = %path.display(), "SSL_CERT_FILE holds no PEM certificate, only the bundled roots are trusted");
    }
    certs
}

/// `SSL_CERT_FILE`, read once per process so a bad bundle warns once.
static EXTRA_ROOTS: LazyLock<Vec<CertificateDer<'static>>> = LazyLock::new(|| {
    std::env::var_os("SSL_CERT_FILE")
        .filter(|v| !v.is_empty())
        .map(|v| load_pem_roots(Path::new(&v)))
        .unwrap_or_default()
});

/// Build a hyper-util client over the given request body type. Private
/// generic so the proxy and TLS configuration live in one place and cannot
/// drift between [`build_client`] and [`build_client_with_body`].
fn build_client_from<B>(
    proxy: Matcher,
    extra_roots: &[CertificateDer<'static>],
) -> hyper_util::client::legacy::Client<hyper_rustls::HttpsConnector<ProxyConnector>, B>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
{
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let mut http = HttpConnector::new();
    // The TLS layer above handles `https://`, so this one must not refuse it.
    http.enforce_http(false);
    let connector = ProxyConnector {
        http,
        proxy: Arc::new(proxy),
    };
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store(extra_roots))
        .with_no_client_auth();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .wrap_connector(connector);
    Client::builder(TokioExecutor::new()).build(https)
}

fn build_client_inner<B>()
-> hyper_util::client::legacy::Client<hyper_rustls::HttpsConnector<ProxyConnector>, B>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
{
    build_client_from(Matcher::from_env(), &EXTRA_ROOTS)
}

/// Build a fresh hyper-util client with TLS support. Called once per
/// task at startup. The client is then reused for every fetch.
///
/// Uses rustls with the Mozilla root certificates (webpki-roots) plus the
/// certificates of `SSL_CERT_FILE`, and routes `https://` through
/// `HTTPS_PROXY` / `ALL_PROXY` minus `NO_PROXY`. Plain HTTP endpoints also
/// work, always directly.
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

/// Strip userinfo from a raw endpoint string when the URI did not parse
/// (so [`redact_endpoint`] cannot run). Defense in depth: config-load
/// validation already rejects `@` in the authority for every scraper.
#[must_use]
pub fn redact_endpoint_str(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return raw.to_string();
    };
    let first_slash = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..first_slash];
    let tail = &rest[first_slash..];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{host_port}{tail}")
}

/// Errors from [`fetch_get`], shared by every caller so each one can
/// `.map_err()` into its domain-specific error type with a one-liner.
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
    /// The response outgrew [`MAX_BODY_BYTES`]. Distinct from
    /// [`FetchError::BodyRead`] because it is the one body failure an
    /// operator can act on: it names a payload the peer built too large,
    /// typically a daemon whose export knobs are set past what its own
    /// clients can read, not a broken transfer.
    #[error("response exceeded the {0} byte body limit")]
    BodyTooLarge(usize),
    #[error("endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("request timed out")]
    Timeout,
}

/// Tell a body that outgrew [`MAX_BODY_BYTES`] apart from a transfer that
/// broke for any other reason.
///
/// `Limited` reports the overrun as a `LengthLimitError` boxed inside the
/// body error, so the two are indistinguishable through `Display` alone,
/// and collapsing them loses the only one an operator can fix.
fn classify_body_error(e: &(dyn std::error::Error + 'static), max_bytes: usize) -> FetchError {
    if is_body_limit_error(e) {
        return FetchError::BodyTooLarge(max_bytes);
    }
    FetchError::BodyRead(format!("{e}"))
}

/// Walk the source chain for the `LengthLimitError` a `Limited` body
/// boxes on overrun. The one walker for every ingest client, so a new
/// caller cannot re-collapse the only body failure an operator can fix
/// into its generic read error.
pub(crate) fn is_body_limit_error(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(e);
    while let Some(err) = source {
        if err.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = err.source();
    }
    false
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
    fetch_get_limited(client, uri, user_agent, timeout, auth, MAX_BODY_BYTES).await
}

/// [`fetch_get`] with an explicit body cap instead of [`MAX_BODY_BYTES`].
///
/// # Errors
///
/// Same as [`fetch_get`], with [`FetchError::BodyTooLarge`] carrying `max_bytes`.
pub async fn fetch_get_limited(
    client: &HttpClient,
    uri: &Uri,
    user_agent: &str,
    timeout: std::time::Duration,
    auth: Option<&crate::ingest::auth_header::AuthHeader>,
    max_bytes: usize,
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

    let limited = Limited::new(response.into_body(), max_bytes);
    let collected = limited
        .collect()
        .await
        .map_err(|e| classify_body_error(e.as_ref(), max_bytes))?;
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
/// timeout or body read failure. Non-2xx statuses are not errors here.
/// They are returned to the caller as the first tuple element.
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
        .map_err(|e| classify_body_error(e.as_ref(), MAX_BODY_BYTES))?;
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
        // method, missing feature) would fail here.
        let _client: HttpClient = build_client();
    }

    /// Self-signed CA, public certificate only, valid until 2126.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBezCCASGgAwIBAgIJALFW2JsTFpZeMAoGCCqGSM49BAMCMCAxHjAcBgNVBAMM
FXBlcmYtc2VudGluZWwgdGVzdCBDQTAgFw0yNjEwMDIxNDMwNDJaGA8yMTI2MDkw
ODE0MzA0MlowIDEeMBwGA1UEAwwVcGVyZi1zZW50aW5lbCB0ZXN0IENBMFkwEwYH
KoZIzj0CAQYIKoZIzj0DAQcDQgAEKMmndE8/nBxrVtcrHUOh0RDguya1w0JU/lbK
dmR2oVcuQNU4zyZjEjr4aMO6A0R0TX7RgIxbEJ0tp0xk6pYHLKNCMEAwDwYDVR0T
AQH/BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFOAInpFXYI0TKpvZ
WVrpkH89K3zaMAoGCCqGSM49BAMCA0gAMEUCIG+tlfC0Ghm/gLmhPlD3+TsQphYO
7EWVzB18zlKhKyO7AiEAs1nuIya6CNgNWxg2qFTe/HI9FsMQuMK1GdozSgJsboM=
-----END CERTIFICATE-----
";

    #[test]
    fn pem_bundle_certificates_are_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.pem");
        std::fs::write(&path, format!("{TEST_CA_PEM}{TEST_CA_PEM}")).unwrap();
        assert_eq!(load_pem_roots(&path).len(), 2);
    }

    #[test]
    fn missing_pem_bundle_loads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_pem_roots(&dir.path().join("absent.pem")).is_empty());
    }

    #[test]
    fn extra_roots_are_trusted_next_to_the_mozilla_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca.pem");
        std::fs::write(&path, TEST_CA_PEM).unwrap();
        let extra = load_pem_roots(&path);
        assert_eq!(extra.len(), 1);
        assert_eq!(
            root_store(&extra).len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1
        );
    }

    /// One-shot fake proxy: returns the request head it received and
    /// refuses the tunnel, which is enough to see where the client went.
    async fn fake_proxy() -> (std::net::SocketAddr, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 2048];
            let n = socket.read(&mut buf).await.unwrap();
            let _ = socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await;
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (addr, handle)
    }

    /// Listener that records the first byte a client sends, to tell a
    /// direct TLS connection (`0x16`, a `ClientHello`) from anything else.
    async fn direct_target() -> (u16, tokio::task::JoinHandle<u8>) {
        use tokio::io::AsyncReadExt;
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = target.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.unwrap();
            let mut first = [0u8; 1];
            socket.read_exact(&mut first).await.unwrap();
            first[0]
        });
        (port, handle)
    }

    #[tokio::test]
    async fn https_destination_is_tunneled_through_the_proxy() {
        let (proxy_addr, proxy) = fake_proxy().await;
        let proxy_url = format!("http://{proxy_addr}");
        let client: HttpClient =
            build_client_from(Matcher::builder().https(proxy_url).build(), &[]);
        let uri: Uri = "https://grid.example.invalid/v4/latest".parse().unwrap();
        let result = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None).await;
        assert!(result.is_err(), "the fake proxy refuses every tunnel");
        let head = tokio::time::timeout(std::time::Duration::from_secs(5), proxy)
            .await
            .expect("the request must reach the proxy")
            .unwrap();
        assert!(
            head.starts_with("CONNECT grid.example.invalid:443 HTTP/1.1\r\n"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn no_proxy_host_is_reached_directly() {
        use tokio::io::AsyncReadExt;
        let (proxy_addr, proxy) = fake_proxy().await;
        let proxy_url = format!("http://{proxy_addr}");
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = target.local_addr().unwrap().port();
        let direct = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.unwrap();
            let mut first = [0u8; 1];
            socket.read_exact(&mut first).await.unwrap();
            first[0]
        });
        let matcher = Matcher::builder().https(proxy_url).no("127.0.0.1").build();
        let client: HttpClient = build_client_from(matcher, &[]);
        let uri: Uri = format!("https://127.0.0.1:{port}/").parse().unwrap();
        let _ = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(2), None).await;
        let first_byte = tokio::time::timeout(std::time::Duration::from_secs(5), direct)
            .await
            .expect("the target must be reached directly")
            .unwrap();
        assert_eq!(
            first_byte, 0x16,
            "a TLS ClientHello opens a direct connection"
        );
        assert!(!proxy.is_finished(), "the proxy must not be contacted");
        proxy.abort();
    }

    #[tokio::test]
    async fn http_destination_never_uses_the_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (proxy_addr, proxy) = fake_proxy().await;
        let proxy_url = format!("http://{proxy_addr}");
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = target.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        });
        let matcher = Matcher::builder()
            .all(proxy_url.clone())
            .http(proxy_url)
            .build();
        let client: HttpClient = build_client_from(matcher, &[]);
        let uri: Uri = format!("http://{addr}/").parse().unwrap();
        let body = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None)
            .await
            .expect("plain HTTP goes straight to the target");
        assert_eq!(&body[..], b"ok");
        server.await.unwrap();
        assert!(!proxy.is_finished(), "the proxy must not be contacted");
        proxy.abort();
    }

    /// Only `http://` proxies are tunneled. A SOCKS or TLS proxy URL, which
    /// the matcher accepts, must never receive a plaintext `CONNECT` (nor the
    /// credentials of an `https://user:pass@` URL): the call goes direct.
    #[tokio::test]
    async fn non_http_proxy_urls_are_never_dialed() {
        for scheme in ["socks5://", "https://user:pass@"] {
            let (proxy_addr, proxy) = fake_proxy().await;
            let (port, direct) = direct_target().await;
            let matcher = Matcher::builder()
                .all(format!("{scheme}{proxy_addr}"))
                .build();
            let client: HttpClient = build_client_from(matcher, &[]);
            let uri: Uri = format!("https://127.0.0.1:{port}/").parse().unwrap();
            let _ = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(2), None).await;
            let first_byte = tokio::time::timeout(std::time::Duration::from_secs(5), direct)
                .await
                .expect("the target must be reached directly")
                .unwrap();
            assert_eq!(first_byte, 0x16, "{scheme}: direct TLS expected");
            assert!(
                !proxy.is_finished(),
                "{scheme}: the proxy must not be contacted"
            );
            proxy.abort();
        }
    }

    #[tokio::test]
    async fn proxy_credentials_are_sent_as_basic_auth() {
        let (proxy_addr, proxy) = fake_proxy().await;
        let matcher = Matcher::builder()
            .https(format!("http://user:pass@{proxy_addr}"))
            .build();
        let client: HttpClient = build_client_from(matcher, &[]);
        let uri: Uri = "https://grid.example.invalid/".parse().unwrap();
        let _ = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None).await;
        let head = tokio::time::timeout(std::time::Duration::from_secs(5), proxy)
            .await
            .expect("the request must reach the proxy")
            .unwrap()
            .to_ascii_lowercase();
        // base64("user:pass")
        assert!(
            head.contains("proxy-authorization: basic dxnlcjpwyxnz"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn fetch_get_limited_enforces_its_own_cap() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
                )
                .await;
        });
        let client = build_client();
        let uri: Uri = format!("http://{addr}/").parse().unwrap();
        let err = fetch_get_limited(
            &client,
            &uri,
            "t",
            std::time::Duration::from_secs(5),
            None,
            4,
        )
        .await
        .expect_err("a 10-byte body over a 4-byte cap must fail");
        assert!(matches!(err, FetchError::BodyTooLarge(4)), "{err:?}");
        server.await.unwrap();
    }

    /// A throwaway CA and a `localhost` server certificate it signs.
    #[cfg(feature = "daemon")]
    fn private_pki() -> (CertificateDer<'static>, CertificateDer<'static>, Vec<u8>) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "perf-sentinel test CA");
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&leaf_key, &issuer)
            .unwrap();
        (
            ca.der().clone(),
            leaf.der().clone(),
            leaf_key.serialize_der(),
        )
    }

    /// One-shot HTTPS server presenting `leaf`, answering `ok`.
    #[cfg(feature = "daemon")]
    async fn tls_server(leaf: CertificateDer<'static>, key: Vec<u8>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let Ok(mut tls) = acceptor.accept(socket).await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = tls.read(&mut buf).await;
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
            let _ = tls.shutdown().await;
        });
        port
    }

    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn private_ca_in_extra_roots_is_trusted() {
        let (ca, leaf, key) = private_pki();
        let port = tls_server(leaf, key).await;
        let client: HttpClient = build_client_from(Matcher::builder().build(), &[ca]);
        let uri: Uri = format!("https://localhost:{port}/").parse().unwrap();
        let body = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None)
            .await
            .expect("a server signed by an extra root must be trusted");
        assert_eq!(&body[..], b"ok");
    }

    /// A self-signed server certificate (a BMC default) placed in
    /// `SSL_CERT_FILE` is trusted as its own anchor, which the docs promise
    /// as long as its subject alternative names cover the host.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn self_signed_server_certificate_in_extra_roots_is_trusted() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let port = tls_server(cert.der().clone(), key.serialize_der()).await;
        let client: HttpClient =
            build_client_from(Matcher::builder().build(), &[cert.der().clone()]);
        let uri: Uri = format!("https://localhost:{port}/").parse().unwrap();
        let body = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None)
            .await
            .expect("a self-signed certificate listed as a root must be trusted");
        assert_eq!(&body[..], b"ok");
    }

    /// Being a root does not waive the name check: a self-signed certificate
    /// whose subject alternative names miss the host is still refused.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn self_signed_certificate_for_another_host_is_rejected() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["bmc.example".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let port = tls_server(cert.der().clone(), key.serialize_der()).await;
        let client: HttpClient =
            build_client_from(Matcher::builder().build(), &[cert.der().clone()]);
        let uri: Uri = format!("https://localhost:{port}/").parse().unwrap();
        let result = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None).await;
        assert!(
            result.is_err(),
            "a name mismatch must fail even for a listed root"
        );
    }

    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn private_ca_without_extra_roots_is_rejected() {
        let (_, leaf, key) = private_pki();
        let port = tls_server(leaf, key).await;
        let client: HttpClient = build_client_from(Matcher::builder().build(), &[]);
        let uri: Uri = format!("https://localhost:{port}/").parse().unwrap();
        let result = fetch_get(&client, &uri, "t", std::time::Duration::from_secs(5), None).await;
        assert!(
            result.is_err(),
            "the Mozilla roots alone must not trust a private CA"
        );
    }

    #[test]
    fn redact_endpoint_strips_credentials_with_default_port() {
        // `hyper::Uri::host()` drops the userinfo (`user:pass@`), so the
        // rebuilt URL cannot leak secrets into logs.
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
        // Only the userinfo must be gone. The port must stay.
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

    /// A body past `MAX_BODY_BYTES` must surface as `BodyTooLarge`, not
    /// as a generic `BodyRead`: it is the one body failure whose fix is a
    /// configuration change on the peer, and the CLI reports it as such.
    #[tokio::test]
    async fn fetch_get_reports_an_oversized_body_distinctly() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let over = MAX_BODY_BYTES + 1024;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {over}\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.write_all(head.as_bytes()).await;
            // Written in chunks: one 8 MB allocation in the test process
            // is wasteful when the client aborts at the limit anyway.
            let chunk = vec![b'x'; 64 * 1024];
            let mut sent = 0usize;
            while sent < over {
                if socket.write_all(&chunk).await.is_err() {
                    break;
                }
                sent += chunk.len();
            }
            let _ = socket.shutdown().await;
        });

        let client = build_client();
        let uri: Uri = format!("http://{addr}/").parse().unwrap();
        let err = fetch_get(
            &client,
            &uri,
            "perf-sentinel-test",
            std::time::Duration::from_secs(10),
            None,
        )
        .await
        .expect_err("a body over the cap must fail");
        assert!(
            matches!(err, FetchError::BodyTooLarge(n) if n == MAX_BODY_BYTES),
            "expected BodyTooLarge, got {err:?}"
        );
        server.abort();
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

    // --- redact_endpoint_str (raw-string variant) -------------------

    #[test]
    fn redact_endpoint_str_strips_userinfo() {
        assert_eq!(
            redact_endpoint_str("http://admin:secret@localhost:8080/scrape"),
            "http://localhost:8080/scrape"
        );
    }

    #[test]
    fn redact_endpoint_str_returns_input_when_no_scheme() {
        // Without a scheme there is no authority delimiter, so the
        // helper bails out. Caller-side validation rejects this case
        // before logs ever reach the helper.
        assert_eq!(
            redact_endpoint_str("user:pass@example.com/foo"),
            "user:pass@example.com/foo"
        );
    }

    #[test]
    fn redact_endpoint_str_keeps_at_in_path() {
        // `@` inside the path component is valid per RFC 3986 and must
        // not be confused with userinfo.
        assert_eq!(redact_endpoint_str("http://host/u@v"), "http://host/u@v");
    }

    #[test]
    fn redact_endpoint_str_with_multiple_at_in_userinfo() {
        // RFC 3986 §3.2.1 says the last `@` in the authority is the
        // userinfo terminator. `rsplit_once('@')` honors that.
        assert_eq!(
            redact_endpoint_str("http://a@b:c@host/path"),
            "http://host/path"
        );
    }

    #[test]
    fn redact_endpoint_str_handles_empty_input() {
        assert_eq!(redact_endpoint_str(""), "");
    }

    #[test]
    fn redact_endpoint_str_strips_only_userinfo_when_path_also_has_at() {
        assert_eq!(
            redact_endpoint_str("http://user:pass@host:8080/x@y"),
            "http://host:8080/x@y"
        );
    }
}
