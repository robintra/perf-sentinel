//! Listener and scraper startup for the daemon.
//!
//! Binds the OTLP gRPC, OTLP HTTP (or HTTPS), and Unix JSON socket
//! listeners, assembles the HTTP router (OTLP + metrics + query API),
//! and spawns the optional energy/intensity scrapers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::Duration;

use crate::acknowledgments;
use crate::config::Config;
use crate::correlate::window::TraceWindow;
use crate::detect;
use crate::detect::DetectConfig;
use crate::event::SpanEvent;
use crate::report::GreenSummary;
use crate::report::metrics::MetricsState;
use crate::score;
use crate::score::cloud_energy::CloudEnergyState;
use crate::score::electricity_maps::ElectricityMapsState;
use crate::score::scaphandre::ScaphandreState;

use super::DaemonError;
use super::ack::{self, AckStore};
use super::findings_store;
use super::query_api;
use super::tls::{build_tls_acceptor, load_tls_pem, serve_https, tls_tcp_incoming};

/// Assemble the optional TLS acceptor and spawn the gRPC, HTTP (or HTTPS),
/// and JSON socket listeners. All three handles are returned so the caller
/// can abort them on Ctrl-C.
pub(super) async fn spawn_listeners(
    config: &Config,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    window: Arc<Mutex<TraceWindow>>,
    findings_store: Arc<findings_store::FindingsStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    metrics: Arc<MetricsState>,
    green_summary: Arc<RwLock<GreenSummary>>,
) -> Result<
    (
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
        Option<tokio::task::JoinHandle<()>>,
    ),
    DaemonError,
> {
    let grpc_addr: std::net::SocketAddr =
        format!("{}:{}", config.listen_addr, config.listen_port_grpc).parse()?;
    let http_addr: std::net::SocketAddr =
        format!("{}:{}", config.listen_addr, config.listen_port).parse()?;

    let http_listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .map_err(DaemonError::HttpBind)?;
    let grpc_listener = tokio::net::TcpListener::bind(grpc_addr)
        .await
        .map_err(DaemonError::GrpcBind)?;

    let tls_acceptor = load_optional_tls(config)?;

    let grpc_handle = spawn_grpc_listener(
        grpc_listener,
        grpc_addr,
        tls_acceptor.clone(),
        tx.clone(),
        config.max_payload_size,
        Arc::clone(&metrics),
    );
    let (toml_acks, ack_store) = init_ack_resources(config).await?;
    let http_router = build_http_router(
        config,
        tx.clone(),
        window,
        findings_store,
        correlator,
        metrics,
        green_summary,
        toml_acks,
        ack_store,
    );
    let http_handle = spawn_http_listener(http_listener, http_addr, tls_acceptor, http_router);
    let json_socket_handle = spawn_json_socket_listener(config, tx);

    Ok((grpc_handle, http_handle, json_socket_handle))
}

/// Load the TLS cert+key pair when both paths are configured. Returns
/// `Ok(None)` when TLS is disabled.
fn load_optional_tls(config: &Config) -> Result<Option<tokio_rustls::TlsAcceptor>, DaemonError> {
    let (Some(cert_path), Some(key_path)) = (&config.tls_cert_path, &config.tls_key_path) else {
        return Ok(None);
    };
    let (cert, key) = load_tls_pem(cert_path, key_path)?;
    Ok(Some(build_tls_acceptor(&cert, &key)?))
}

/// Spawn the OTLP gRPC listener (plain or TLS-wrapped).
fn spawn_grpc_listener(
    listener: tokio::net::TcpListener,
    addr: std::net::SocketAddr,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    max_payload: usize,
    metrics: Arc<MetricsState>,
) -> tokio::task::JoinHandle<()> {
    let grpc_service = crate::ingest::otlp::OtlpGrpcService::new(tx, Some(metrics));
    tokio::spawn(async move {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer;
        if tls_acceptor.is_some() {
            tracing::info!("OTLP gRPC+TLS listening on {addr}");
        } else {
            tracing::info!("OTLP gRPC listening on {addr}");
        }
        let incoming = tls_tcp_incoming(listener, tls_acceptor);
        if let Err(e) = tonic::transport::Server::builder()
            .timeout(Duration::from_mins(1))
            .add_service(
                TraceServiceServer::new(grpc_service).max_decoding_message_size(max_payload),
            )
            .serve_with_incoming(incoming)
            .await
        {
            tracing::error!("gRPC server error: {e}");
        }
    })
}

/// Load the CI ack TOML baseline and initialize the daemon JSONL ack
/// store. Both are skipped when `[daemon.ack] enabled = false`.
///
/// Error policy is split by source:
///
/// - When the operator explicitly set `[daemon.ack] storage_path` or
///   `[daemon.ack] toml_path` and that path fails to load, the daemon
///   refuses to start with a typed `DaemonError`. The operator chose
///   the path, a typo or permission issue should be loud at startup,
///   not silently downgraded to a 503 hours later.
/// - When the path was resolved from the default
///   (`dirs::data_local_dir()` / `.perf-sentinel-acknowledgments.toml`
///   in CWD), failures are logged at WARN and the daemon stays up.
///   This keeps a quirky filesystem (parallel test isolation, missing
///   `HOME`, denied write perms on `~/.local/share`) from taking the
///   whole daemon down for an opt-in feature.
async fn init_ack_resources(
    config: &Config,
) -> Result<
    (
        Arc<HashMap<String, query_api::ResolvedTomlAck>>,
        Option<Arc<AckStore>>,
    ),
    DaemonError,
> {
    if !config.ack_enabled {
        return Ok((Arc::new(HashMap::new()), None));
    }
    let toml_acks = load_toml_acks(config)?;
    let store = match init_store(config).await {
        Ok(s) => Some(s),
        Err((e, configured_path)) => {
            if let Some(path) = configured_path {
                return Err(DaemonError::AckStoreInit { path, source: e });
            }
            tracing::warn!(
                error = %e,
                "Failed to initialize daemon ack store at default path, \
                 runtime ack endpoints will return 503. Set [daemon.ack] \
                 storage_path explicitly to opt out of the default location."
            );
            None
        }
    };
    Ok((Arc::new(toml_acks), store))
}

fn load_toml_acks(
    config: &Config,
) -> Result<HashMap<String, query_api::ResolvedTomlAck>, DaemonError> {
    let configured = config.ack_toml_path.is_some();
    let toml_path = config.ack_toml_path.as_deref().map_or_else(
        || PathBuf::from(".perf-sentinel-acknowledgments.toml"),
        |s| Path::new(s).to_path_buf(),
    );
    let path_existed = toml_path.exists();
    let file = match acknowledgments::load_from_file(&toml_path) {
        Ok(f) => f,
        Err(e) if configured => {
            return Err(DaemonError::AckTomlLoad {
                path: toml_path.display().to_string(),
                source: e,
            });
        }
        Err(e) => {
            tracing::warn!(
                path = %toml_path.display(),
                error = %e,
                "Failed to load CI ack TOML at default path, baseline empty"
            );
            return Ok(HashMap::new());
        }
    };
    let now = Utc::now();
    let toml_acks: HashMap<_, _> = file
        .acknowledged
        .into_iter()
        .filter(|a| acknowledgments::is_ack_active(a, now))
        .map(|a| {
            let expires_at_dt = parse_expires_at_end_of_day(a.expires_at.as_deref());
            (
                a.signature.clone(),
                query_api::ResolvedTomlAck {
                    inner: a,
                    expires_at_dt,
                },
            )
        })
        .collect();
    if path_existed {
        tracing::info!(
            path = %toml_path.display(),
            count = toml_acks.len(),
            "Loaded CI ack TOML baseline"
        );
    } else {
        tracing::info!(
            path = %toml_path.display(),
            "No CI ack TOML found at startup, set [daemon.ack] toml_path to override"
        );
    }
    Ok(toml_acks)
}

/// Resolve the storage path and open the JSONL store. Returns the
/// configured path alongside the error so the caller can decide
/// whether to escalate to a fatal `DaemonError` (operator-supplied
/// path) or downgrade to a WARN log (default-resolved path).
async fn init_store(config: &Config) -> Result<Arc<AckStore>, (ack::AckError, Option<String>)> {
    let (storage_path, configured) = match &config.ack_storage_path {
        Some(p) => (PathBuf::from(p), Some(p.clone())),
        None => match ack::default_storage_path() {
            Ok(p) => (p, None),
            Err(e) => return Err((e, None)),
        },
    };
    match AckStore::new(storage_path).await {
        Ok(store) => {
            tracing::info!(
                path = %store.storage_path().display(),
                "Daemon ack store ready"
            );
            Ok(store)
        }
        Err(e) => Err((e, configured)),
    }
}

fn parse_expires_at_end_of_day(value: Option<&str>) -> Option<chrono::DateTime<Utc>> {
    let raw = value?;
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let end_of_day = date.and_hms_opt(23, 59, 59)?;
    Some(end_of_day.and_utc())
}

/// Assemble the OTLP HTTP + metrics + optional query API router, with the
/// request-timeout layer.
#[allow(clippy::too_many_arguments)]
fn build_http_router(
    config: &Config,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    window: Arc<Mutex<TraceWindow>>,
    findings_store: Arc<findings_store::FindingsStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    metrics: Arc<MetricsState>,
    green_summary: Arc<RwLock<GreenSummary>>,
    toml_acks: Arc<HashMap<String, query_api::ResolvedTomlAck>>,
    ack_store: Option<Arc<AckStore>>,
) -> axum::Router {
    let otlp_router = crate::ingest::otlp::otlp_http_router(
        tx,
        config.max_payload_size,
        Some(Arc::clone(&metrics)),
    );
    // Clone the Arc unconditionally so `metrics_route` and the query
    // API state can share it when the API is enabled. When disabled,
    // the extra `Arc::clone` is one atomic refcount increment, not
    // worth a conditional to avoid.
    let metrics_router = crate::report::metrics::metrics_route(Arc::clone(&metrics));
    let health_router = super::health::health_route();
    let mut http_router = otlp_router.merge(metrics_router).merge(health_router);
    if config.daemon_api_enabled {
        let query_state = Arc::new(query_api::QueryApiState {
            findings_store,
            window,
            detect_config: DetectConfig::from(config),
            start_time: std::time::Instant::now(),
            correlator,
            metrics,
            scoring_config: config
                .green_electricity_maps
                .as_ref()
                .map(score::carbon::ScoringConfig::from_electricity_maps),
            green_summary,
            ack_store,
            toml_acks,
            ack_api_key: config.ack_api_key.clone(),
        });
        http_router = http_router.merge(query_api::query_api_router(query_state));
    } else {
        tracing::info!("Daemon query API disabled by config");
    }
    http_router.layer(
        tower::ServiceBuilder::new()
            .layer(axum::error_handling::HandleErrorLayer::new(|_| async {
                tracing::debug!("HTTP request timed out");
                axum::http::StatusCode::REQUEST_TIMEOUT
            }))
            .layer(tower::timeout::TimeoutLayer::new(Duration::from_mins(1))),
    )
}

/// Spawn the OTLP HTTP listener, picking the TLS or plain variant based on
/// whether an acceptor was configured.
fn spawn_http_listener(
    listener: tokio::net::TcpListener,
    addr: std::net::SocketAddr,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    router: axum::Router,
) -> tokio::task::JoinHandle<()> {
    if let Some(acceptor) = tls_acceptor {
        tokio::spawn(async move {
            tracing::info!("OTLP HTTPS listening on {addr}");
            serve_https(listener, router, acceptor).await;
        })
    } else {
        tokio::spawn(async move {
            tracing::info!("OTLP HTTP listening on {addr}");
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!("HTTP server error: {e}");
            }
        })
    }
}

/// Spawn the Unix JSON socket listener when the target is Unix. On other
/// platforms logs a warning and returns `None`.
// `#[cfg(not(unix))]` branch returns `None`, so the `Option` is required.
#[allow(clippy::unnecessary_wraps)]
fn spawn_json_socket_listener(
    config: &Config,
    tx: mpsc::Sender<Vec<SpanEvent>>,
) -> Option<tokio::task::JoinHandle<()>> {
    #[cfg(unix)]
    {
        let socket_path = config.json_socket.clone();
        let max_payload = config.max_payload_size;
        Some(tokio::spawn(async move {
            super::json_socket::run_json_socket(&socket_path, tx, max_payload).await;
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = (config, tx);
        tracing::warn!("JSON socket ingestion not available on this platform; use OTLP HTTP/gRPC");
        None
    }
}

/// Build the optional cross-trace correlator from the config. The daemon
/// shares the same `Arc` with `QueryApiState` so the `/api/correlations`
/// endpoint and the ingestion loop see the same state.
pub(super) fn setup_correlator(
    config: &Config,
) -> Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>> {
    if !config.correlation_enabled {
        return None;
    }
    tracing::info!("Cross-trace correlation enabled");
    Some(Arc::new(Mutex::new(
        detect::correlate_cross::CrossTraceCorrelator::new(config.correlation_config.clone()),
    )))
}

/// Handles and staleness threshold for an optional energy/intensity
/// scraper. `state` is `None` when the scraper is disabled; `staleness_ms`
/// is `0` in that case and ignored by the snapshot read.
pub(super) struct ScraperSetup<S> {
    pub(super) state: Option<Arc<S>>,
    pub(super) handle: Option<tokio::task::JoinHandle<()>>,
    pub(super) staleness_ms: u64,
}

/// Spawn the Scaphandre scraper when `[green.scaphandre]` is configured.
/// Staleness threshold is 3x the scrape interval (hung-scraper defense).
pub(super) fn setup_scaphandre_scraper(
    config: &Config,
    metrics: &Arc<MetricsState>,
) -> ScraperSetup<ScaphandreState> {
    let Some(scaph_cfg) = config.green_scaphandre.clone() else {
        return ScraperSetup {
            state: None,
            handle: None,
            staleness_ms: 0,
        };
    };
    let staleness_ms = scaph_cfg.scrape_interval.as_millis() as u64 * 3;
    let state = ScaphandreState::new();
    let handle = score::scaphandre::spawn_scraper(scaph_cfg, state.clone(), metrics.clone());
    ScraperSetup {
        state: Some(state),
        handle: Some(handle),
        staleness_ms,
    }
}

/// Spawn the cloud energy (`SPECpower`) scraper when `[green.cloud]` is
/// configured. Same staleness convention as Scaphandre.
pub(super) fn setup_cloud_scraper(
    config: &Config,
    metrics: &Arc<MetricsState>,
) -> ScraperSetup<CloudEnergyState> {
    let Some(cloud_cfg) = config.green_cloud_energy.clone() else {
        return ScraperSetup {
            state: None,
            handle: None,
            staleness_ms: 0,
        };
    };
    let staleness_ms = cloud_cfg.scrape_interval.as_millis() as u64 * 3;
    let state = CloudEnergyState::new();
    let handle =
        score::cloud_energy::spawn_cloud_scraper(cloud_cfg, state.clone(), metrics.clone());
    ScraperSetup {
        state: Some(state),
        handle: Some(handle),
        staleness_ms,
    }
}

/// Spawn the Electricity Maps real-time intensity scraper when
/// `[green.electricity_maps]` is configured.
pub(super) fn setup_emaps_scraper(config: &Config) -> ScraperSetup<ElectricityMapsState> {
    let Some(emaps_cfg) = config.green_electricity_maps.clone() else {
        return ScraperSetup {
            state: None,
            handle: None,
            staleness_ms: 0,
        };
    };
    let staleness_ms = emaps_cfg.poll_interval.as_millis() as u64 * 3;
    let state = ElectricityMapsState::new();
    let handle = score::electricity_maps::spawn_electricity_maps_scraper(emaps_cfg, state.clone());
    ScraperSetup {
        state: Some(state),
        handle: Some(handle),
        staleness_ms,
    }
}
