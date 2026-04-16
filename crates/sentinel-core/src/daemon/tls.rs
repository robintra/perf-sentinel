//! TLS helpers for the daemon: stream wrapping, accept-loop incoming
//! stream, PEM loading, and an HTTPS serve loop for axum routers.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::Duration;

use super::{DaemonError, TlsConfigError};

/// Maximum time allowed for a TLS handshake to complete. Connections that
/// do not finish the handshake within this window are dropped, preventing
/// slowloris-style resource exhaustion.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A stream that is either a plain TCP connection or a TLS-wrapped one.
/// Implements `AsyncRead + AsyncWrite` so tonic and hyper can use it
/// transparently without knowing whether TLS is active.
pub(super) enum MaybeTlsStream {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>),
}

impl AsyncRead for MaybeTlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// tonic requires streams to implement `Connected` for remote addr info.
impl tonic::transport::server::Connected for MaybeTlsStream {
    type ConnectInfo = std::net::SocketAddr;

    fn connect_info(&self) -> Self::ConnectInfo {
        match self {
            Self::Plain(s) => s.peer_addr().unwrap_or_else(|_| ([0, 0, 0, 0], 0).into()),
            Self::Tls(s) => s
                .get_ref()
                .0
                .peer_addr()
                .unwrap_or_else(|_| ([0, 0, 0, 0], 0).into()),
        }
    }
}

/// Create an async stream of connections (plain or TLS) from a TCP listener.
/// When `tls_acceptor` is `Some`, each accepted TCP connection is upgraded
/// to TLS before being yielded. Failed TLS handshakes are silently dropped.
///
/// Internally spawns a task that feeds a bounded channel; the returned
/// `ReceiverStream` is consumed by tonic's `serve_with_incoming`.
pub(super) fn tls_tcp_incoming(
    listener: tokio::net::TcpListener,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
) -> tokio_stream::wrappers::ReceiverStream<Result<MaybeTlsStream, std::io::Error>> {
    let (tx, rx) = mpsc::channel(128);

    tokio::spawn(async move {
        loop {
            let (tcp, addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::debug!("TCP accept error: {e}");
                    continue;
                }
            };
            let stream = if let Some(ref acceptor) = tls_acceptor {
                match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.clone().accept(tcp))
                    .await
                {
                    Ok(Ok(tls)) => MaybeTlsStream::Tls(Box::new(tls)),
                    Ok(Err(e)) => {
                        tracing::debug!("TLS handshake failed from {addr}: {e}");
                        continue;
                    }
                    Err(_) => {
                        tracing::debug!("TLS handshake timed out from {addr}");
                        continue;
                    }
                }
            } else {
                MaybeTlsStream::Plain(tcp)
            };
            if tx.send(Ok(stream)).await.is_err() {
                break; // receiver dropped, shutting down
            }
        }
    });

    tokio_stream::wrappers::ReceiverStream::new(rx)
}

/// Read TLS certificate and key from disk. Returns raw PEM bytes.
/// Never logs the key content.
pub(super) fn load_tls_pem(
    cert_path: &str,
    key_path: &str,
) -> Result<(Vec<u8>, Vec<u8>), DaemonError> {
    let cert = std::fs::read(cert_path).map_err(|source| {
        DaemonError::TlsConfig(TlsConfigError::ReadCert {
            path: cert_path.to_string(),
            source,
        })
    })?;
    let key = std::fs::read(key_path).map_err(|source| {
        DaemonError::TlsConfig(TlsConfigError::ReadKey {
            path: key_path.to_string(),
            source,
        })
    })?;
    Ok((cert, key))
}

/// Build a `tokio_rustls::TlsAcceptor` from PEM cert chain + key.
/// Used for the HTTP/OTLP listener; gRPC uses tonic's native TLS.
pub(super) fn build_tls_acceptor(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<tokio_rustls::TlsAcceptor, DaemonError> {
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| DaemonError::TlsConfig(TlsConfigError::ParseCerts(e)))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem)
        .map_err(|e| DaemonError::TlsConfig(TlsConfigError::ParseKey(e)))?;

    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| DaemonError::TlsConfig(TlsConfigError::ServerConfig(e)))?;

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// Serve an axum `Router` over TLS using a manual accept loop.
///
/// Each accepted TCP connection is upgraded to TLS via the acceptor,
/// then served with hyper. Failed TLS handshakes are logged at debug
/// level and silently dropped (not fatal to the server).
pub(super) async fn serve_https(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    tls_acceptor: tokio_rustls::TlsAcceptor,
) {
    use tower::ServiceExt;

    loop {
        let (tcp_stream, remote_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::debug!("TCP accept error: {e}");
                continue;
            }
        };

        let acceptor = tls_acceptor.clone();
        let app = app.clone();

        tokio::spawn(async move {
            let tls_stream = match tokio::time::timeout(
                TLS_HANDSHAKE_TIMEOUT,
                acceptor.accept(tcp_stream),
            )
            .await
            {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::debug!("TLS handshake failed from {remote_addr}: {e}");
                    return;
                }
                Err(_) => {
                    tracing::debug!("TLS handshake timed out from {remote_addr}");
                    return;
                }
            };

            let io = hyper_util::rt::TokioIo::new(tls_stream);

            // Bridge axum (tower) router to hyper service: convert
            // Incoming body to axum::body::Body, then oneshot the router.
            let service =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let app = app.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        let req = hyper::Request::from_parts(parts, axum::body::Body::new(body));
                        Ok::<_, std::convert::Infallible>(
                            app.oneshot(req).await.unwrap_or_else(|err| match err {}),
                        )
                    }
                });

            // auto::Builder negotiates HTTP/1.1 and HTTP/2, matching
            // the behavior of axum::serve on the non-TLS path. OTLP
            // clients commonly use HTTP/2 when TLS is active.
            if let Err(e) =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(io, service)
                    .await
            {
                tracing::debug!("HTTPS connection error from {remote_addr}: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_tls_pem_returns_read_cert_error_for_missing_file() {
        let err = load_tls_pem("/nonexistent/cert.pem", "/nonexistent/key.pem").unwrap_err();
        match err {
            DaemonError::TlsConfig(TlsConfigError::ReadCert { path, .. }) => {
                assert_eq!(path, "/nonexistent/cert.pem");
            }
            other => panic!("expected ReadCert error, got: {other:?}"),
        }
    }

    #[test]
    fn load_tls_pem_returns_read_key_error_when_cert_exists_but_key_missing() {
        // Create a temp cert so the first read succeeds.
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        std::fs::write(&cert_path, b"dummy").unwrap();
        let err = load_tls_pem(cert_path.to_str().unwrap(), "/nonexistent/key.pem").unwrap_err();
        match err {
            DaemonError::TlsConfig(TlsConfigError::ReadKey { path, .. }) => {
                assert_eq!(path, "/nonexistent/key.pem");
            }
            other => panic!("expected ReadKey error, got: {other:?}"),
        }
    }

    #[test]
    fn build_tls_acceptor_rejects_invalid_cert_pem() {
        let bad_cert = b"not a pem certificate";
        let bad_key = b"not a pem key";
        // TlsAcceptor does not implement Debug, so we can't `.unwrap_err()`.
        // Match on the Result directly.
        match build_tls_acceptor(bad_cert, bad_key) {
            Ok(_) => panic!("expected build_tls_acceptor to reject invalid PEM"),
            Err(DaemonError::TlsConfig(
                TlsConfigError::ParseCerts(_) | TlsConfigError::ParseKey(_),
            )) => {}
            Err(other) => panic!("expected ParseCerts or ParseKey, got: {other:?}"),
        }
    }

    #[test]
    fn tls_config_error_display_contains_source_context() {
        let err = DaemonError::TlsConfig(TlsConfigError::ReadCert {
            path: "/etc/foo.pem".to_string(),
            source: std::io::Error::other("permission denied"),
        });
        let msg = format!("{err}");
        assert!(msg.contains("TLS"));
        assert!(msg.contains("/etc/foo.pem"));
    }
}
