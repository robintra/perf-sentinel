//! Daemon mode: streaming detection with OTLP and JSON ingestion.
//!
//! Runs an event loop that receives spans from multiple sources (OTLP gRPC,
//! OTLP HTTP, JSON socket), accumulates them in a `TraceWindow`, and emits
//! findings as NDJSON on stdout when traces expire.

pub mod ack;
pub mod archive;
pub mod findings_store;
pub mod health;
pub mod query_api;

mod event_loop;
#[cfg(unix)]
mod json_socket;
mod listeners;
mod sampling;
mod tls;

use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, mpsc};

use crate::config::Config;
use crate::correlate::window::{TraceWindow, WindowConfig};
use crate::detect::DetectConfig;
use crate::event::SpanEvent;
use crate::report::GreenSummary;
use crate::report::metrics::MetricsState;

use event_loop::{
    EnergyScraperHandles, EnergySources, EventLoopConfig, ListenerHandles, ShutdownTargets,
    run_event_loop,
};
use listeners::{
    setup_cloud_scraper, setup_correlator, setup_emaps_scraper, setup_scaphandre_scraper,
    spawn_listeners,
};

/// Errors that can occur when running the daemon.
///
/// Marked `#[non_exhaustive]` so that adding future variants (e.g. a
/// new failure mode for a newly-integrated listener) stays a
/// SemVer-minor change. External consumers that `match` on this enum
/// must use a catch-all arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonError {
    /// Listen address could not be parsed as a socket address.
    #[error("invalid listen address: {0}")]
    InvalidAddr(#[from] std::net::AddrParseError),
    /// HTTP listener failed to bind.
    #[error("failed to bind HTTP listener: {0}")]
    HttpBind(std::io::Error),
    /// gRPC listener failed to bind.
    #[error("failed to bind gRPC listener: {0}")]
    GrpcBind(std::io::Error),
    /// TLS configuration or certificate loading failed.
    #[error("TLS configuration error: {0}")]
    TlsConfig(#[source] TlsConfigError),
    /// Loading the CI ack TOML baseline failed at startup, and the
    /// path was operator-configured (so the failure escalates rather
    /// than silently downgrading to an empty baseline).
    #[error("failed to load acknowledgments TOML at '{path}'")]
    AckTomlLoad {
        /// Operator-configured path that failed to load.
        path: String,
        /// Underlying load error.
        #[source]
        source: crate::acknowledgments::AcknowledgmentLoadError,
    },
    /// Initializing the daemon ack JSONL store failed at startup, and
    /// the storage path was operator-configured.
    #[error("failed to initialize ack store at '{path}'")]
    AckStoreInit {
        /// Operator-configured path that failed to initialize.
        path: String,
        /// Underlying init error.
        #[source]
        source: ack::AckError,
    },
    /// `[reporting] intent = "official"` is configured but the org-config
    /// is missing fields required for a publishable disclosure. Every
    /// missing or invalid field is listed in `errors`.
    #[error("[reporting] official intent is misconfigured:\n - {}", errors.join("\n - "))]
    ReportingValidation {
        /// All validation failures detected at startup.
        errors: Vec<String>,
    },
    /// Opening the per-window report archive file failed at startup.
    #[error("failed to open report archive at '{path}'")]
    ArchiveOpen {
        /// Operator-configured path that failed to open.
        path: String,
        /// Underlying open error.
        #[source]
        source: archive::ArchiveError,
    },
}

/// Typed sub-enum for TLS configuration failures.
///
/// Replaces the prior `Box<dyn std::error::Error>` variant with five
/// concrete cases so callers can match on `TlsConfigError` and get
/// structured context instead of a `format!`-flattened string.
///
/// Marked `#[non_exhaustive]` so that adding future variants (e.g. a
/// handshake-specific failure or an unsupported protocol version) stays
/// a SemVer-minor change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TlsConfigError {
    /// Could not read the PEM-encoded certificate chain from disk.
    #[error("failed to read TLS cert '{path}'")]
    ReadCert {
        /// Path to the cert file that could not be opened.
        path: String,
        /// Underlying I/O error (permissions, missing file, etc.).
        #[source]
        source: std::io::Error,
    },
    /// Could not read the PEM-encoded private key from disk.
    #[error("failed to read TLS key '{path}'")]
    ReadKey {
        /// Path to the key file that could not be opened.
        path: String,
        /// Underlying I/O error (permissions, missing file, etc.).
        #[source]
        source: std::io::Error,
    },
    /// The certificate chain PEM could not be parsed.
    #[error("failed to parse TLS cert chain")]
    ParseCerts(#[source] tokio_rustls::rustls::pki_types::pem::Error),
    /// The private key PEM could not be parsed.
    #[error("failed to parse TLS private key")]
    ParseKey(#[source] tokio_rustls::rustls::pki_types::pem::Error),
    /// `rustls::ServerConfig::with_single_cert` rejected the cert+key pair
    /// (e.g. mismatched key, unsupported algorithm).
    #[error("rustls server config rejected the cert+key pair")]
    ServerConfig(#[source] tokio_rustls::rustls::Error),
}

/// Run the daemon: start all listeners and process events in a loop.
///
/// # Errors
///
/// Returns an error if the configured addresses are invalid or a listener fails to bind.
///
/// # Panics
///
/// Panics if `config.daemon.max_active_traces` is 0 (config validation prevents this).
pub async fn run(config: Config) -> Result<(), DaemonError> {
    validate_official_reporting(&config)?;
    let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(1024);
    let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
        max_events_per_trace: config.daemon.max_events_per_trace,
        trace_ttl_ms: config.daemon.trace_ttl_ms,
        max_active_traces: std::num::NonZeroUsize::new(config.daemon.max_active_traces)
            .expect("config validates max_active_traces >= 1"),
    })));
    let metrics = Arc::new(MetricsState::new());
    let findings_store = Arc::new(findings_store::FindingsStore::new(
        config.daemon.max_retained_findings,
    ));
    let correlator = setup_correlator(&config);
    // Shared cell mutated by the event loop after each batch and read
    // by the /api/export/report handler. Initialized to disabled(0):
    // the cold-start guard (`events_processed == 0 -> 503`) ensures
    // clients never observe the initial value.
    let green_summary_cell = Arc::new(RwLock::new(GreenSummary::disabled(0)));

    let (grpc_handle, http_handle, json_socket_handle) = spawn_listeners(
        &config,
        tx.clone(),
        window.clone(),
        findings_store.clone(),
        correlator.clone(),
        metrics.clone(),
        green_summary_cell.clone(),
    )
    .await?;

    let scaphandre = setup_scaphandre_scraper(&config, &metrics);
    let cloud = setup_cloud_scraper(&config, &metrics);
    let emaps = setup_emaps_scraper(&config);

    let archive_handle = match &config.daemon.archive {
        Some(cfg) => Some(
            archive::spawn(cfg).map_err(|source| DaemonError::ArchiveOpen {
                path: cfg.path.clone(),
                source,
            })?,
        ),
        None => None,
    };

    let base_carbon_ctx = config.carbon_context();
    let detect_config = DetectConfig::from(&config);
    let energy_sources = EnergySources {
        base_carbon_ctx: &base_carbon_ctx,
        scaphandre_state: scaphandre.state.as_deref(),
        scaphandre_staleness_ms: scaphandre.staleness_ms,
        cloud_state: cloud.state.as_deref(),
        cloud_staleness_ms: cloud.staleness_ms,
        emaps_state: emaps.state.as_deref(),
        emaps_staleness_ms: emaps.staleness_ms,
    };
    let shutdown = ShutdownTargets {
        energy: EnergyScraperHandles {
            scaphandre: scaphandre.handle.as_ref(),
            cloud: cloud.handle.as_ref(),
            emaps: emaps.handle.as_ref(),
        },
        listeners: ListenerHandles {
            grpc: &grpc_handle,
            http: &http_handle,
            json_socket: json_socket_handle.as_ref(),
        },
    };

    run_event_loop(
        &mut rx,
        &window,
        &metrics,
        &findings_store,
        correlator.as_deref(),
        &detect_config,
        &energy_sources,
        shutdown,
        EventLoopConfig {
            green_enabled: config.green.enabled,
            sampling_rate: config.daemon.sampling_rate,
            evict_ms: config.daemon.trace_ttl_ms / 2,
            confidence: config.confidence(),
        },
        &green_summary_cell,
        archive_handle.as_ref(),
    )
    .await;

    if let Some(handle) = archive_handle {
        drop(handle.tx);
        let _ = handle.join.await;
    }

    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(&config.daemon.json_socket);
    }
    Ok(())
}

/// Daemon startup gate for `[reporting] intent = "official"`.
/// See `docs/design/08-PERIODIC-DISCLOSURE.md`.
fn validate_official_reporting(config: &Config) -> Result<(), DaemonError> {
    use crate::report::periodic::org_config;

    if config.reporting.intent.as_deref() == Some("audited") {
        return Err(DaemonError::ReportingValidation {
            errors: vec![
                "[reporting] intent = \"audited\" is not yet implemented, refusing to start daemon"
                    .to_string(),
            ],
        });
    }
    if config.reporting.intent.as_deref() != Some("official") {
        return Ok(());
    }

    let mut errors: Vec<String> = Vec::new();
    let loaded = match &config.reporting.org_config_path {
        None => {
            errors.push(
                "[reporting] org_config_path is required when intent = \"official\"".to_string(),
            );
            None
        }
        Some(path) => match org_config::load_from_path(path) {
            Ok(cfg) => Some(cfg),
            Err(err) => {
                errors.push(err.to_string());
                None
            }
        },
    };

    if let Some(cfg) = loaded {
        errors.extend(org_config::validate_for_official(&cfg));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(DaemonError::ReportingValidation { errors })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn daemon_run_rejects_invalid_listen_address() {
        // Malformed listen_addr fails the `format!().parse()` call in
        // `run` before any listener binds. Covers the InvalidAddr path.
        let config = Config {
            daemon: crate::config::DaemonConfig {
                listen_addr: "not an address".to_string(),
                ..crate::config::DaemonConfig::default()
            },
            ..Config::default()
        };
        // Bogus port paths still reach .parse(), which fails.
        let err = run(config).await.expect_err("should fail");
        assert!(matches!(err, DaemonError::InvalidAddr(_)));
    }

    #[tokio::test]
    async fn daemon_run_refuses_official_without_org_config_path() {
        let config = Config {
            reporting: crate::config::ReportingConfig {
                intent: Some("official".to_string()),
                ..Default::default()
            },
            ..Config::default()
        };
        let err = run(config).await.expect_err("must refuse");
        match err {
            DaemonError::ReportingValidation { errors } => {
                assert!(
                    errors.iter().any(|e| e.contains("org_config_path")),
                    "got {errors:?}"
                );
            }
            other => panic!("expected ReportingValidation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn daemon_run_refuses_official_with_unreadable_org_config() {
        let config = Config {
            reporting: crate::config::ReportingConfig {
                intent: Some("official".to_string()),
                org_config_path: Some("/no/such/path/org.toml".to_string()),
                ..Default::default()
            },
            ..Config::default()
        };
        let err = run(config).await.expect_err("must refuse");
        assert!(matches!(err, DaemonError::ReportingValidation { .. }));
    }

    #[tokio::test]
    async fn daemon_run_refuses_official_when_org_config_misses_fields() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        // Incomplete: empty name, lowercase country, missing core patterns.
        writeln!(
            file,
            r#"
[organisation]
name = ""
country = "fr"

[methodology]
sci_specification = "ISO/IEC 21031:2024"
enabled_patterns = ["slow_sql"]
disabled_patterns = []
conformance = "partial"

[methodology.calibration]
carbon_intensity_source = "electricity_maps"
specpower_table_version = "2024-2026"

[scope_manifest]
total_applications_declared = 1
"#
        )
        .unwrap();

        let config = Config {
            reporting: crate::config::ReportingConfig {
                intent: Some("official".to_string()),
                org_config_path: Some(file.path().display().to_string()),
                ..Default::default()
            },
            ..Config::default()
        };
        let err = run(config).await.expect_err("must refuse");
        match err {
            DaemonError::ReportingValidation { errors } => {
                assert!(errors.iter().any(|e| e.contains("organisation.name")));
                assert!(errors.iter().any(|e| e.contains("country")));
                assert!(errors.iter().any(|e| e.contains("n_plus_one_sql")));
            }
            other => panic!("expected ReportingValidation, got {other:?}"),
        }
    }

    #[test]
    fn daemon_error_display_is_informative() {
        // Smoke test for thiserror messages on every variant. Operators
        // should be able to tell "bad port number" from "port already in
        // use" at a glance.
        let e1: DaemonError = "not a socket"
            .parse::<std::net::SocketAddr>()
            .unwrap_err()
            .into();
        assert!(format!("{e1}").contains("invalid listen address"));
        let e2 = DaemonError::HttpBind(std::io::Error::other("boom"));
        assert!(format!("{e2}").contains("HTTP listener"));
        let e3 = DaemonError::GrpcBind(std::io::Error::other("boom"));
        assert!(format!("{e3}").contains("gRPC listener"));
    }

    // ------------------------------------------------------------------
    // daemon::run end-to-end on ephemeral ports
    // ------------------------------------------------------------------
    //
    // Spins up the daemon on ephemeral TCP ports (0 → OS-assigned) and
    // a tempdir-scoped Unix socket, sends one NDJSON line, then polls
    // the HTTP /metrics endpoint and asserts the daemon actually
    // processed the event. Ctrl-C is never sent; the test aborts the
    // JoinHandle instead, so the shutdown branch is not covered here
    // (validated separately by manual testing).

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_run_ingests_json_socket_events_and_exposes_metrics() {
        use std::fmt::Write as _;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream, UnixStream};
        use tokio::time::Duration;

        // Grab ephemeral ports via TCP binds, then release them so the
        // daemon can rebind. There is a brief race window between drop
        // and rebind; we compensate below with a retry loop on the
        // Unix-socket client connect, which is the first externally
        // observable effect of a successful bind.
        let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = l1.local_addr().unwrap().port();
        let grpc_port = l2.local_addr().unwrap().port();
        drop(l1);
        drop(l2);

        let (_dir, socket_path) = json_socket::unique_socket_dir_and_path("daemon-run");
        let socket_path_str = socket_path.to_string_lossy().into_owned();
        let config = Config {
            daemon: crate::config::DaemonConfig {
                listen_addr: "127.0.0.1".to_string(),
                listen_port: http_port,
                listen_port_grpc: grpc_port,
                json_socket: socket_path_str,
                trace_ttl_ms: 200, // fast eviction so the ticker fires during test
                max_active_traces: 10,
                ..crate::config::DaemonConfig::default()
            },
            ..Config::default()
        };

        let daemon_handle = tokio::spawn(async move {
            let _ = run(config).await;
        });

        // Poll for the Unix socket to appear (with retries) instead of
        // a fixed sleep. Gives slow CI runners more headroom without
        // hard-coding a conservative wait on fast dev machines.
        let mut client = None;
        for _ in 0..40 {
            if let Ok(s) = UnixStream::connect(&socket_path).await {
                client = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let mut client = client.expect("daemon Unix socket must bind within 1s");

        // Send 6 N+1-worthy events so the detector has something to
        // actually flag, which guarantees `findings_total` increments.
        let mut payload = String::from("[");
        for i in 1..=6 {
            if i > 1 {
                payload.push(',');
            }
            // Hand-written JSON (not serde) so the test has zero deps
            // on the event types' serde layout evolving over time.
            let _ = write!(
                payload,
                r#"{{"timestamp":"2025-07-10T14:32:01.{i:03}Z","trace_id":"daemon-t1","span_id":"s{i}","service":"svc-e2e","type":"sql","operation":"SELECT","target":"SELECT * FROM users WHERE id = {i}","duration_us":100,"source":{{"endpoint":"GET /test","method":"m"}}}}"#
            );
        }
        payload.push(']');
        client.write_all(payload.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        // Poll the HTTP /metrics endpoint with retries. The daemon
        // flushes processed traces on every eviction tick (ttl/2 = 100ms),
        // so we give it up to 1.5s to update the counters. Using a
        // minimal hand-rolled HTTP/1.0 GET avoids pulling hyper into the
        // test just for this.
        let metrics_addr = format!("127.0.0.1:{http_port}");
        let mut observed_events = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let Ok(mut stream) = TcpStream::connect(&metrics_addr).await else {
                continue;
            };
            let req = "GET /metrics HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n";
            if stream.write_all(req.as_bytes()).await.is_err() {
                continue;
            }
            let mut buf = Vec::with_capacity(8192);
            if stream.read_to_end(&mut buf).await.is_err() {
                continue;
            }
            let body = String::from_utf8_lossy(&buf);
            // Look for any non-zero events counter. The metric text is
            // `perf_sentinel_events_processed_total <value>` (OpenMetrics).
            if body.contains("perf_sentinel_events_processed_total")
                && body.lines().any(|l| {
                    l.starts_with("perf_sentinel_events_processed_total")
                        && l.split_whitespace()
                            .last()
                            .and_then(|v| v.parse::<f64>().ok())
                            .is_some_and(|v| v > 0.0)
                })
            {
                observed_events = true;
                break;
            }
        }

        // Abort the daemon BEFORE the assert so the test cleans up on
        // both pass and fail paths.
        daemon_handle.abort();
        let _ = daemon_handle.await;
        // _dir drops here, removing the socket and parent tempdir.

        assert!(
            observed_events,
            "daemon should have processed the 6 events and surfaced a \
             non-zero `perf_sentinel_events_processed_total` on /metrics"
        );
    }

    // ------------------------------------------------------------------
    // /health is always exposed, even when the query API is disabled.
    // ------------------------------------------------------------------
    //
    // Liveness probes (K8s, load balancers, systemd) must not depend on
    // `daemon.api_enabled`. This test spins up a daemon with the query
    // API off, then hits `/health` over TCP to verify both the 200 OK
    // and the JSON body shape.

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_exposes_health_endpoint_even_when_query_api_disabled() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        use tokio::time::Duration;

        let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = l1.local_addr().unwrap().port();
        let grpc_port = l2.local_addr().unwrap().port();
        drop(l1);
        drop(l2);

        let (_dir, socket_path) = json_socket::unique_socket_dir_and_path("daemon-health");
        let config = Config {
            daemon: crate::config::DaemonConfig {
                listen_addr: "127.0.0.1".to_string(),
                listen_port: http_port,
                listen_port_grpc: grpc_port,
                json_socket: socket_path.to_string_lossy().into_owned(),
                api_enabled: false, // proves /health is not gated by the query API toggle
                ..crate::config::DaemonConfig::default()
            },
            ..Config::default()
        };

        let daemon_handle = tokio::spawn(async move {
            let _ = run(config).await;
        });

        let addr = format!("127.0.0.1:{http_port}");
        let mut body = String::new();
        let mut ok = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let Ok(mut stream) = TcpStream::connect(&addr).await else {
                continue;
            };
            let req = "GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n";
            if stream.write_all(req.as_bytes()).await.is_err() {
                continue;
            }
            let mut buf = Vec::with_capacity(1024);
            if stream.read_to_end(&mut buf).await.is_err() {
                continue;
            }
            body = String::from_utf8_lossy(&buf).into_owned();
            if body.starts_with("HTTP/1.") && body.contains(" 200 ") {
                ok = true;
                break;
            }
        }

        daemon_handle.abort();
        let _ = daemon_handle.await;

        assert!(ok, "/health should return 200 OK; got response: {body}");
        assert!(
            body.contains("\"status\":\"ok\""),
            "response body should include status=ok; got: {body}"
        );
        assert!(
            body.contains(env!("CARGO_PKG_VERSION")),
            "response body should include current version; got: {body}"
        );
    }
}
