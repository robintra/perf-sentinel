//! Daemon mode: streaming detection with OTLP and JSON ingestion.
//!
//! Runs an event loop that receives spans from multiple sources (OTLP gRPC,
//! OTLP HTTP, JSON socket), accumulates them in a `TraceWindow`, and emits
//! findings as NDJSON on stdout when traces expire.

pub mod findings_store;
pub mod query_api;

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, interval};

use crate::config::Config;
use crate::correlate::Trace;
use crate::correlate::window::{TraceWindow, WindowConfig};
use crate::detect;
use crate::detect::{Confidence, DetectConfig};
use crate::event::SpanEvent;
use crate::normalize;
use crate::report::GreenSummary;
use crate::report::metrics::MetricsState;
use crate::score;
use crate::score::cloud_energy::CloudEnergyState;
use crate::score::electricity_maps::ElectricityMapsState;
use crate::score::scaphandre::ScaphandreState;

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
/// Panics if `config.max_active_traces` is 0 (config validation prevents this).
pub async fn run(config: Config) -> Result<(), DaemonError> {
    let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(1024);
    let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
        max_events_per_trace: config.max_events_per_trace,
        trace_ttl_ms: config.trace_ttl_ms,
        max_active_traces: std::num::NonZeroUsize::new(config.max_active_traces)
            .expect("config validates max_active_traces >= 1"),
    })));
    let metrics = Arc::new(MetricsState::new());
    let findings_store = Arc::new(findings_store::FindingsStore::new(
        config.max_retained_findings,
    ));
    let correlator = setup_correlator(&config);

    let (grpc_handle, http_handle, json_socket_handle) = spawn_listeners(
        &config,
        tx.clone(),
        window.clone(),
        findings_store.clone(),
        correlator.clone(),
        metrics.clone(),
    )
    .await?;

    let scaphandre = setup_scaphandre_scraper(&config, &metrics);
    let cloud = setup_cloud_scraper(&config, &metrics);
    let emaps = setup_emaps_scraper(&config);

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
            green_enabled: config.green_enabled,
            sampling_rate: config.sampling_rate,
            evict_ms: config.trace_ttl_ms / 2,
            confidence: config.confidence(),
        },
    )
    .await;

    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(&config.json_socket);
    }
    Ok(())
}

/// Assemble the optional TLS acceptor and spawn the gRPC, HTTP (or HTTPS),
/// and JSON socket listeners. All three handles are returned so the caller
/// can abort them on Ctrl-C.
async fn spawn_listeners(
    config: &Config,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    window: Arc<Mutex<TraceWindow>>,
    findings_store: Arc<findings_store::FindingsStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    metrics: Arc<MetricsState>,
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
    );
    let http_router = build_http_router(
        config,
        tx.clone(),
        window,
        findings_store,
        correlator,
        metrics,
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
) -> tokio::task::JoinHandle<()> {
    let grpc_service = crate::ingest::otlp::OtlpGrpcService::new(tx);
    tokio::spawn(async move {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer;
        if tls_acceptor.is_some() {
            tracing::info!("OTLP gRPC+TLS listening on {addr}");
        } else {
            tracing::info!("OTLP gRPC listening on {addr}");
        }
        let incoming = tls_tcp_incoming(listener, tls_acceptor);
        if let Err(e) = tonic::transport::Server::builder()
            .timeout(Duration::from_secs(60))
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

/// Assemble the OTLP HTTP + metrics + optional query API router, with the
/// request-timeout layer.
fn build_http_router(
    config: &Config,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    window: Arc<Mutex<TraceWindow>>,
    findings_store: Arc<findings_store::FindingsStore>,
    correlator: Option<Arc<Mutex<detect::correlate_cross::CrossTraceCorrelator>>>,
    metrics: Arc<MetricsState>,
) -> axum::Router {
    let otlp_router = crate::ingest::otlp::otlp_http_router(tx, config.max_payload_size);
    let metrics_router = crate::report::metrics::metrics_route(metrics);
    let mut http_router = otlp_router.merge(metrics_router);
    if config.daemon_api_enabled {
        let query_state = Arc::new(query_api::QueryApiState {
            findings_store,
            window,
            detect_config: DetectConfig::from(config),
            start_time: std::time::Instant::now(),
            correlator,
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
            .layer(tower::timeout::TimeoutLayer::new(Duration::from_secs(60))),
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
            run_json_socket(&socket_path, tx, max_payload).await;
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
fn setup_correlator(
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
struct ScraperSetup<S> {
    state: Option<Arc<S>>,
    handle: Option<tokio::task::JoinHandle<()>>,
    staleness_ms: u64,
}

/// Spawn the Scaphandre scraper when `[green.scaphandre]` is configured.
/// Staleness threshold is 3x the scrape interval (hung-scraper defense).
fn setup_scaphandre_scraper(
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
fn setup_cloud_scraper(
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
fn setup_emaps_scraper(config: &Config) -> ScraperSetup<ElectricityMapsState> {
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

/// Bundle of handles aborted on Ctrl-C.
struct ShutdownTargets<'a> {
    energy: EnergyScraperHandles<'a>,
    listeners: ListenerHandles<'a>,
}

/// Config slice the main event loop needs — the values that are pulled out
/// of `Config` once at startup and never change.
#[derive(Clone, Copy)]
struct EventLoopConfig {
    green_enabled: bool,
    sampling_rate: f64,
    evict_ms: u64,
    confidence: Confidence,
}

/// Drive the daemon's main `tokio::select!` loop: receive events, run the
/// TTL ticker, and handle Ctrl-C. Returns when Ctrl-C is received.
#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    rx: &mut mpsc::Receiver<Vec<SpanEvent>>,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
    findings_store: &findings_store::FindingsStore,
    correlator: Option<&Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
    detect_config: &DetectConfig,
    energy_sources: &EnergySources<'_>,
    shutdown: ShutdownTargets<'_>,
    loop_cfg: EventLoopConfig,
) {
    let mut ticker = interval(Duration::from_millis(loop_cfg.evict_ms.max(100)));
    // Prevent burst-catchup if process_traces takes longer than the tick
    // interval. The Scaphandre and cloud scrapers already use Delay.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Cardinality cap on the per-service Prometheus counter prevents OOM
    // from a malicious OTLP sender injecting millions of unique
    // `service.name` values.
    let mut service_meter = ServiceMeter {
        known_services: std::collections::HashSet::new(),
        max_service_cardinality: 1024,
        service_cap_warned: false,
    };
    let parts = || ProcessTracesCtxParts {
        detect_config,
        green_enabled: loop_cfg.green_enabled,
        metrics,
        confidence: loop_cfg.confidence,
        findings_store,
        correlator,
    };

    loop {
        tokio::select! {
            Some(events) = rx.recv() => {
                let lru_evicted = ingest_event_batch(
                    events,
                    loop_cfg.sampling_rate,
                    window,
                    metrics,
                    &mut service_meter,
                ).await;
                flush_evicted(lru_evicted, energy_sources, parts()).await;
            }
            _ = ticker.tick() => {
                let expired = evict_expired_traces(window, metrics).await;
                flush_evicted(expired, energy_sources, parts()).await;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Shutting down daemon, processing remaining traces...");
                shutdown_listeners(shutdown.energy, shutdown.listeners);
                let remaining = {
                    let mut w = window.lock().await;
                    w.drain_all()
                };
                flush_evicted(remaining, energy_sources, parts()).await;
                break;
            }
        }
    }
}

/// Per-service I/O op counter state with a cardinality cap. Prevents OOM
/// from a malicious OTLP sender injecting millions of unique
/// `service.name` values.
struct ServiceMeter {
    known_services: std::collections::HashSet<String>,
    max_service_cardinality: usize,
    service_cap_warned: bool,
}

impl ServiceMeter {
    fn record(&mut self, service: &str, metrics: &MetricsState) {
        if self.known_services.contains(service) {
            metrics
                .service_io_ops_total
                .with_label_values(&[service])
                .inc();
        } else if self.known_services.len() < self.max_service_cardinality {
            self.known_services.insert(service.to_string());
            metrics
                .service_io_ops_total
                .with_label_values(&[service])
                .inc();
        } else if !self.service_cap_warned {
            tracing::warn!(
                cap = self.max_service_cardinality,
                "Service cardinality cap reached; new services will \
                 not have per-service I/O op counters"
            );
            self.service_cap_warned = true;
        }
    }
}

/// Lifetime-bound bundle of energy/intensity scraper state used to build
/// the per-tick `CarbonContext`. Borrowed by `flush_evicted`.
struct EnergySources<'a> {
    base_carbon_ctx: &'a score::carbon::CarbonContext,
    scaphandre_state: Option<&'a ScaphandreState>,
    scaphandre_staleness_ms: u64,
    cloud_state: Option<&'a CloudEnergyState>,
    cloud_staleness_ms: u64,
    emaps_state: Option<&'a ElectricityMapsState>,
    emaps_staleness_ms: u64,
}

/// Borrowed parts of `ProcessTracesCtx` shared across all flush sites.
struct ProcessTracesCtxParts<'a> {
    detect_config: &'a DetectConfig,
    green_enabled: bool,
    metrics: &'a MetricsState,
    confidence: Confidence,
    findings_store: &'a findings_store::FindingsStore,
    correlator: Option<&'a Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
}

/// `JoinHandle`s for the optional energy / intensity scrapers.
#[derive(Clone, Copy)]
struct EnergyScraperHandles<'a> {
    scaphandre: Option<&'a tokio::task::JoinHandle<()>>,
    cloud: Option<&'a tokio::task::JoinHandle<()>>,
    emaps: Option<&'a tokio::task::JoinHandle<()>>,
}

/// `JoinHandle`s for the listener tasks bound at startup.
#[derive(Clone, Copy)]
struct ListenerHandles<'a> {
    grpc: &'a tokio::task::JoinHandle<()>,
    http: &'a tokio::task::JoinHandle<()>,
    json_socket: Option<&'a tokio::task::JoinHandle<()>>,
}

/// Sample, normalize, meter, and push a batch of events into the window.
/// Returns the LRU-evicted traces so the caller can route them through
/// detect+score+store.
async fn ingest_event_batch(
    events: Vec<SpanEvent>,
    sampling_rate: f64,
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
    service_meter: &mut ServiceMeter,
) -> Vec<(String, Vec<normalize::NormalizedEvent>)> {
    let events = apply_sampling(events, sampling_rate);
    let event_count = events.len();
    // Normalize OUTSIDE the lock to minimize lock hold time.
    let normalized: Vec<_> = events.into_iter().map(normalize::normalize).collect();
    for event in &normalized {
        service_meter.record(event.event.service.as_str(), metrics);
    }
    let now_ms = current_time_ms();
    let mut lru_evicted = Vec::new();
    {
        // Lock held for O(batch_size) push() calls. Each push is O(1)
        // amortized (LRU insert/promote). Batch size is bounded by the
        // mpsc channel capacity (1024) and max_payload_size.
        let mut w = window.lock().await;
        for event in normalized {
            if let Some(evicted) = w.push(event, now_ms) {
                lru_evicted.push(evicted);
            }
        }
        metrics.active_traces.set(w.active_traces() as f64);
    }
    metrics.events_processed_total.inc_by(event_count as f64);
    lru_evicted
}

/// Pop TTL-expired traces under the lock and refresh the active gauge.
async fn evict_expired_traces(
    window: &Arc<Mutex<TraceWindow>>,
    metrics: &MetricsState,
) -> Vec<(String, Vec<normalize::NormalizedEvent>)> {
    let now_ms = current_time_ms();
    let mut w = window.lock().await;
    let expired = w.evict_expired(now_ms);
    metrics.active_traces.set(w.active_traces() as f64);
    expired
}

/// Build a tick `CarbonContext` and route the traces through detect+score.
/// No-op when `traces` is empty.
async fn flush_evicted(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    sources: &EnergySources<'_>,
    parts: ProcessTracesCtxParts<'_>,
) {
    if traces.is_empty() {
        return;
    }
    let tick_ctx = build_tick_ctx(
        sources.base_carbon_ctx,
        sources.scaphandre_state,
        sources.scaphandre_staleness_ms,
        sources.cloud_state,
        sources.cloud_staleness_ms,
        sources.emaps_state,
        sources.emaps_staleness_ms,
    );
    process_traces(
        traces,
        ProcessTracesCtx {
            detect_config: parts.detect_config,
            green_enabled: parts.green_enabled,
            carbon_ctx: &tick_ctx,
            metrics: parts.metrics,
            confidence: parts.confidence,
            findings_store: parts.findings_store,
            correlator: parts.correlator,
        },
    )
    .await;
}

/// Abort all spawned tasks before the daemon returns. Order matters:
/// scrapers first so their log lines don't interleave with the shutdown
/// message, then the listeners.
fn shutdown_listeners(energy: EnergyScraperHandles<'_>, listeners: ListenerHandles<'_>) {
    if let Some(handle) = energy.emaps {
        handle.abort();
    }
    if let Some(handle) = energy.cloud {
        handle.abort();
    }
    if let Some(handle) = energy.scaphandre {
        handle.abort();
    }
    listeners.grpc.abort();
    listeners.http.abort();
    if let Some(handle) = listeners.json_socket {
        handle.abort();
    }
}

/// Build a per-tick `CarbonContext` by optionally patching the base
/// context with a fresh energy snapshot merged from all configured
/// energy sources (Scaphandre RAPL and/or cloud `SPECpower`) plus
/// real-time Electricity Maps intensity.
///
/// Returns `Cow::Borrowed(base)` when no scraper produced fresh data
/// (the common case when all three scrapers are either disabled or
/// still warming up), avoiding the `CarbonContext::clone` on every
/// tick. Materializes an owned clone only when at least one scraper
/// has a reading to inject. `process_traces` takes `&CarbonContext`
/// so the Cow is cheap to use at the call site via `&*ctx`.
///
/// Scaphandre entries take precedence over cloud entries for the same
/// service (direct RAPL measurement beats `SPECpower` interpolation).
fn build_tick_ctx<'a>(
    base: &'a score::carbon::CarbonContext,
    scaphandre_state: Option<&ScaphandreState>,
    scaphandre_staleness_ms: u64,
    cloud_state: Option<&CloudEnergyState>,
    cloud_staleness_ms: u64,
    emaps_state: Option<&ElectricityMapsState>,
    emaps_staleness_ms: u64,
) -> std::borrow::Cow<'a, score::carbon::CarbonContext> {
    let now = score::scaphandre::monotonic_ms();

    // Cloud entries first (lower precedence).
    let cloud_snap = cloud_state
        .map(|s| s.snapshot(now, cloud_staleness_ms))
        .unwrap_or_default();
    // Scaphandre entries override cloud for the same service.
    let scaph_snap = scaphandre_state
        .map(|s| s.snapshot(now, scaphandre_staleness_ms))
        .unwrap_or_default();
    // Electricity Maps real-time intensity (independent of energy snapshot).
    let emaps_snap = emaps_state
        .map(|s| s.snapshot(now, emaps_staleness_ms))
        .unwrap_or_default();

    // Fast path: nothing fresh this tick → no clone, just borrow base.
    if cloud_snap.is_empty() && scaph_snap.is_empty() && emaps_snap.is_empty() {
        return std::borrow::Cow::Borrowed(base);
    }

    // Slow path: materialize a merged snapshot and clone base.
    let mut merged: std::collections::HashMap<String, score::carbon::EnergyEntry> =
        std::collections::HashMap::with_capacity(cloud_snap.len() + scaph_snap.len());
    for (service, energy_kwh) in cloud_snap {
        merged.insert(service, score::carbon::EnergyEntry::cloud(energy_kwh));
    }
    for (service, energy_kwh) in scaph_snap {
        merged.insert(service, score::carbon::EnergyEntry::scaphandre(energy_kwh));
    }

    let mut ctx = base.clone();
    ctx.energy_snapshot = if merged.is_empty() {
        None
    } else {
        Some(merged)
    };
    if !emaps_snap.is_empty() {
        ctx.real_time_intensity = Some(emaps_snap);
    }

    std::borrow::Cow::Owned(ctx)
}

/// Record slow span durations into a Prometheus histogram.
///
/// `histogram_quantile()` can then compute accurate global percentiles
/// across sharded daemon instances. Handles resolved once before the loop
/// to avoid per-span `HashMap` lookups in `with_label_values`.
fn record_slow_durations(traces: &[Trace], detect_config: &DetectConfig, metrics: &MetricsState) {
    let slow_threshold_us = detect_config.slow_threshold_ms.saturating_mul(1000);
    let hist_sql = metrics.slow_duration_seconds.with_label_values(&["sql"]);
    let hist_http = metrics
        .slow_duration_seconds
        .with_label_values(&["http_out"]);
    for trace in traces {
        for span in &trace.spans {
            if span.event.duration_us > slow_threshold_us {
                let hist = match span.event.event_type {
                    crate::event::EventType::Sql => &hist_sql,
                    crate::event::EventType::HttpOut => &hist_http,
                };
                hist.observe(span.event.duration_us as f64 / 1_000_000.0);
            }
        }
    }
}

/// Update Prometheus counters, gauges, and exemplars, then emit findings
/// as NDJSON to stdout.
fn emit_findings_and_update_metrics(
    trace_count: usize,
    findings: &[detect::Finding],
    green_summary: &GreenSummary,
    metrics: &MetricsState,
) {
    use std::io::Write;

    metrics.traces_analyzed_total.inc_by(trace_count as f64);
    metrics
        .total_io_ops
        .inc_by(green_summary.total_io_ops as f64);
    metrics
        .avoidable_io_ops
        .inc_by(green_summary.avoidable_io_ops as f64);
    let cumulative_total = metrics.total_io_ops.get();
    if cumulative_total > 0.0 {
        metrics
            .io_waste_ratio
            .set(metrics.avoidable_io_ops.get() / cumulative_total);
    }
    metrics.record_exemplars(findings, green_summary);

    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for finding in findings {
        metrics
            .findings_total
            .with_label_values(&[finding.finding_type.as_str(), finding.severity.as_str()])
            .inc();
        if serde_json::to_writer(&mut lock, finding).is_ok() {
            let _ = writeln!(lock);
        }
    }
}

/// Process a batch of completed/expired traces: detect, score, emit NDJSON.
///
/// Shared context passed to [`process_traces`] on every tick.
///
/// Groups the configuration, state, and downstream sinks so the function
/// signature stays readable. All fields are borrowed for the duration of
/// the call, no ownership transfer.
struct ProcessTracesCtx<'a> {
    detect_config: &'a DetectConfig,
    green_enabled: bool,
    carbon_ctx: &'a score::carbon::CarbonContext,
    metrics: &'a MetricsState,
    confidence: Confidence,
    findings_store: &'a findings_store::FindingsStore,
    correlator: Option<&'a Mutex<detect::correlate_cross::CrossTraceCorrelator>>,
}

/// stamps `confidence` on every finding after detection. The
/// value is derived from `config.daemon_environment` in `run()` and passed
/// here unchanged. `analyze` batch mode does not call this function; it
/// uses `pipeline::analyze_with_traces` which hardcodes
/// `Confidence::CiBatch`.
async fn process_traces(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    ctx: ProcessTracesCtx<'_>,
) {
    if traces.is_empty() {
        return;
    }

    let trace_count = traces.len();
    let trace_structs: Vec<Trace> = traces
        .into_iter()
        .map(|(trace_id, spans)| Trace { trace_id, spans })
        .collect();

    let mut findings = detect::detect(&trace_structs, ctx.detect_config);

    // Cross-trace slow percentile analysis. The internal detector
    // requires >= 2 distinct traces per template, so we gate on 2.
    if trace_structs.len() >= 2 {
        findings.extend(detect::slow::detect_slow_cross_trace(
            &trace_structs,
            ctx.detect_config.slow_threshold_ms,
            ctx.detect_config.slow_min_occurrences,
        ));
    }

    record_slow_durations(&trace_structs, ctx.detect_config, ctx.metrics);

    let (mut findings, green_summary) = if ctx.green_enabled {
        score::score_green(&trace_structs, findings, Some(ctx.carbon_ctx))
    } else {
        let total_io_ops = trace_structs.iter().map(|t| t.spans.len()).sum();
        (findings, GreenSummary::disabled(total_io_ops))
    };

    // stamp the daemon's confidence label. Detectors emitted
    // `Confidence::default()` (= CiBatch); overwrite with the real value
    // captured from Config at daemon startup.
    for finding in &mut findings {
        finding.confidence = ctx.confidence;
    }
    let findings = findings;

    let now_ms = current_time_ms();
    if !findings.is_empty() {
        ctx.findings_store.push_batch(&findings, now_ms).await;
    }

    if let Some(correlator) = ctx.correlator {
        correlator.lock().await.ingest(&findings, now_ms);
    }

    emit_findings_and_update_metrics(trace_count, &findings, &green_summary, ctx.metrics);
}

/// Apply trace-level sampling: cache decisions per `trace_id` to avoid
/// redundant hashing for events sharing a trace.
///
/// The cache is keyed on the u64 FNV-1a hash of the `trace_id` rather
/// than on a `String` clone, so a burst of 100k events with 10k
/// distinct traces incurs zero heap allocations for the cache keys.
/// Hash collisions are harmless: a collision only means two different
/// traces share the same keep/drop decision, which is the same
/// statistical behavior as rolling the dice independently.
fn apply_sampling(events: Vec<SpanEvent>, rate: f64) -> Vec<SpanEvent> {
    if rate >= 1.0 {
        return events;
    }
    let mut cache = std::collections::HashMap::<u64, bool>::new();
    events
        .into_iter()
        .filter(|e| {
            let h = hash_trace_id(&e.trace_id);
            if let Some(&decision) = cache.get(&h) {
                return decision;
            }
            let decision = hash_to_decision(h, rate);
            cache.insert(h, decision);
            decision
        })
        .collect()
}

/// FNV-1a 64-bit hash of a `trace_id`. Extracted from `should_sample` so
/// it can be called once per event in `apply_sampling` and reused as
/// both the cache key and the sampling decision input.
#[inline]
fn hash_trace_id(trace_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in trace_id.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Map a precomputed trace hash to a keep/drop decision.
#[inline]
#[allow(clippy::cast_precision_loss)] // rate comparison is approximate by design
fn hash_to_decision(hash: u64, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    (hash as f64 / u64::MAX as f64) < rate
}

/// Deterministic per-trace sampling used by the unit tests. Production
/// code goes through [`hash_trace_id`] + [`hash_to_decision`] directly
/// in `apply_sampling` to avoid rehashing when the cache is consulted.
#[cfg(test)]
fn should_sample(trace_id: &str, rate: f64) -> bool {
    hash_to_decision(hash_trace_id(trace_id), rate)
}

// ---------------------------------------------------------------------------
// TLS helpers
// ---------------------------------------------------------------------------

/// Maximum time allowed for a TLS handshake to complete. Connections that
/// do not finish the handshake within this window are dropped, preventing
/// slowloris-style resource exhaustion.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A stream that is either a plain TCP connection or a TLS-wrapped one.
/// Implements `AsyncRead + AsyncWrite` so tonic and hyper can use it
/// transparently without knowing whether TLS is active.
enum MaybeTlsStream {
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
fn tls_tcp_incoming(
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
fn load_tls_pem(cert_path: &str, key_path: &str) -> Result<(Vec<u8>, Vec<u8>), DaemonError> {
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
fn build_tls_acceptor(
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
async fn serve_https(
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

/// Get current time in milliseconds since epoch.
///
/// Returns 0 and logs a warning if the system clock is set before the
/// Unix epoch (effectively a configuration error). Downstream code treats
/// the timestamp as a monotonic-ish sort key; a single zero tick produces
/// visible bucketing but no correctness issue.
fn current_time_ms() -> u64 {
    if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    } else {
        tracing::warn!(
            "System clock is before Unix epoch; using 0 as current_time_ms. \
             Check system time configuration."
        );
        0
    }
}

/// Run the JSON socket listener on Unix platforms.
///
/// Reads newline-delimited JSON (NDJSON): each line is a JSON array of `SpanEvent`s.
#[cfg(unix)]
async fn run_json_socket(path: &str, tx: mpsc::Sender<Vec<SpanEvent>>, max_payload_size: usize) {
    use tokio::net::UnixListener;

    // Symlink-TOCTOU defense: refuse to unlink anything at `path` that
    // is a symlink. A local attacker who controls the parent directory
    // could otherwise point `path` at `/etc/passwd` (or any other file
    // the daemon user owns) and the `remove_file` on the next line
    // would follow the symlink and delete the target. `symlink_metadata`
    // does NOT follow symlinks, so we can detect and refuse safely.
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::error!(
                "Refusing to bind Unix socket at {path}: path is a \
                 symlink — remove it manually after verifying the \
                 target is safe"
            );
            return;
        }
        _ => {}
    }

    // Clean up stale socket file (now verified to be a regular file or
    // absent).
    let _ = std::fs::remove_file(path);

    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind Unix socket {path}: {e}");
            return;
        }
    };

    // Restrict socket permissions to owner-only (prevent other local users from injecting events)
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::error!(
                "Failed to set socket permissions on {path}: {e} — refusing to listen on insecure socket"
            );
            let _ = std::fs::remove_file(path);
            return;
        }
    }

    tracing::info!("JSON socket listening on {path}");

    // Limit concurrent connections to prevent local DoS via connection flooding
    let semaphore = Arc::new(tokio::sync::Semaphore::new(128));

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let tx = tx.clone();
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    break; // semaphore closed
                };
                tokio::spawn(async move {
                    handle_json_connection(stream, tx, max_payload_size).await;
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::error!("Unix socket accept error: {e}");
            }
        }
    }
}

/// Process a single JSON socket connection: read NDJSON lines and forward events.
#[cfg(unix)]
async fn handle_json_connection(
    stream: tokio::net::UnixStream,
    tx: mpsc::Sender<Vec<SpanEvent>>,
    max_payload_size: usize,
) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    const CONNECTION_LIMIT_FACTOR: u64 = 16;
    let limited = stream.take(max_payload_size as u64 * CONNECTION_LIMIT_FACTOR);
    let reader = tokio::io::BufReader::new(limited);
    let mut lines = reader.lines();
    let ingest = crate::ingest::json::JsonIngest::new(max_payload_size);
    while let Ok(Some(line)) = lines.next_line().await {
        if line.len() > max_payload_size {
            tracing::warn!("JSON socket: line exceeds max payload size, skipping");
            continue;
        }
        match crate::ingest::IngestSource::ingest(&ingest, line.as_bytes()) {
            Ok(events) if !events.is_empty() => {
                if tx.send(events).await.is_err() {
                    tracing::warn!("JSON socket: event channel closed");
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("JSON socket: failed to parse line: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::window::WindowConfig;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize;

    fn make_normalized(trace_id: &str, target: &str) -> normalize::NormalizedEvent {
        normalize::normalize(SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
        })
    }

    fn default_detect_config() -> DetectConfig {
        DetectConfig {
            n_plus_one_threshold: 5,
            window_ms: 500,
            slow_threshold_ms: 500,
            slow_min_occurrences: 3,
            max_fanout: 20,
            chatty_service_min_calls: 15,
            pool_saturation_concurrent_threshold: 10,
            serialized_min_sequential: 3,
        }
    }

    fn empty_carbon_ctx() -> score::carbon::CarbonContext {
        score::carbon::CarbonContext::default()
    }

    /// Build a `ProcessTracesCtx` for tests with sensible defaults.
    fn test_ctx<'a>(
        detect_config: &'a DetectConfig,
        carbon_ctx: &'a score::carbon::CarbonContext,
        metrics: &'a MetricsState,
        findings_store: &'a findings_store::FindingsStore,
        green_enabled: bool,
    ) -> ProcessTracesCtx<'a> {
        ProcessTracesCtx {
            detect_config,
            green_enabled,
            carbon_ctx,
            metrics,
            confidence: Confidence::DaemonStaging,
            findings_store,
            correlator: None,
        }
    }

    #[tokio::test]
    async fn process_traces_empty_does_nothing() {
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;
    }

    #[tokio::test]
    async fn process_traces_with_n_plus_one() {
        // 6 events with different params -> N+1 finding
        let events: Vec<_> = (1..=6)
            .map(|i| {
                make_normalized(
                    "t1",
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                )
            })
            .collect();
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;
    }

    #[tokio::test]
    async fn process_traces_clean_no_finding() {
        // 2 events with different templates -> no finding
        let events = vec![
            make_normalized("t1", "SELECT * FROM users WHERE id = 1"),
            make_normalized("t1", "SELECT * FROM orders WHERE id = 2"),
        ];
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;
    }

    #[test]
    fn current_time_ms_returns_nonzero() {
        let ms = current_time_ms();
        assert!(ms > 0, "current_time_ms should return a positive value");
    }

    #[test]
    fn evict_expired_returns_traces() {
        let config = WindowConfig {
            trace_ttl_ms: 100,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);

        let event = normalize::normalize(SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: "t1".to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
        });

        w.push(event, 0);
        assert_eq!(w.active_traces(), 1);

        // Not yet expired
        let expired = w.evict_expired(50);
        assert!(expired.is_empty());
        assert_eq!(w.active_traces(), 1);

        // Now expired (150 - 0 = 150 > 100)
        let expired = w.evict_expired(150);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "t1");
        assert_eq!(expired[0].1.len(), 1);
        assert_eq!(w.active_traces(), 0);
    }

    #[tokio::test]
    async fn process_traces_updates_metrics() {
        let events: Vec<_> = (1..=6)
            .map(|i| {
                make_normalized(
                    "t1",
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                )
            })
            .collect();
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, true),
        )
        .await;

        let output = metrics.render();
        assert!(output.contains("perf_sentinel_traces_analyzed_total"));
        assert!(output.contains("perf_sentinel_findings_total"));
    }

    #[tokio::test]
    async fn process_traces_green_disabled() {
        let events: Vec<_> = (1..=6)
            .map(|i| {
                make_normalized(
                    "t1",
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                )
            })
            .collect();
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        let store = findings_store::FindingsStore::new(100);
        let detect_config = default_detect_config();
        process_traces(
            vec![("t1".to_string(), events)],
            test_ctx(&detect_config, &ctx, &metrics, &store, false),
        )
        .await;
        // avoidable_io_ops counter should stay at 0 when green is disabled
        assert!((metrics.avoidable_io_ops.get() - 0.0).abs() < f64::EPSILON);
        // but total_io_ops should still be counted
        assert!(metrics.total_io_ops.get() > 0.0);
    }

    #[test]
    fn should_sample_deterministic() {
        // Same trace_id always produces the same result
        let r1 = should_sample("trace-abc-123", 0.5);
        let r2 = should_sample("trace-abc-123", 0.5);
        assert_eq!(r1, r2);
    }

    #[test]
    fn should_sample_rate_zero_drops_all() {
        assert!(!should_sample("any-trace", 0.0));
        assert!(!should_sample("another-trace", 0.0));
    }

    #[test]
    fn should_sample_rate_one_keeps_all() {
        assert!(should_sample("any-trace", 1.0));
        assert!(should_sample("another-trace", 1.0));
    }

    #[test]
    fn should_sample_rate_half_splits() {
        // With enough distinct trace IDs, roughly half should be sampled
        let sampled = (0..1000)
            .filter(|i| should_sample(&format!("trace-{i}"), 0.5))
            .count();
        // Allow wide margin: between 30% and 70%
        assert!(
            (300..=700).contains(&sampled),
            "expected ~500 sampled, got {sampled}"
        );
    }

    // ------------------------------------------------------------------
    // build_tick_ctx merge tests
    // ------------------------------------------------------------------

    #[test]
    fn build_tick_ctx_no_scrapers_yields_borrowed_cow() {
        // Fast path: no scrapers → Cow::Borrowed, no clone.
        let base = score::carbon::CarbonContext::default();
        let ctx = build_tick_ctx(&base, None, 0, None, 0, None, 0);
        assert!(matches!(ctx, std::borrow::Cow::Borrowed(_)));
        assert!(ctx.energy_snapshot.is_none());
    }

    #[test]
    fn build_tick_ctx_scaphandre_only() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let ctx = build_tick_ctx(&base, Some(&scaph), 500, None, 0, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
    }

    #[test]
    fn build_tick_ctx_cloud_only() {
        let base = score::carbon::CarbonContext::default();
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-b".into(), 2e-7, 100);
        let ctx = build_tick_ctx(&base, None, 0, Some(&cloud), 500, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-b"].model_tag, "cloud_specpower");
    }

    #[test]
    fn build_tick_ctx_scaphandre_overrides_cloud_for_same_service() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-a".into(), 5e-7, 100);
        cloud.insert_for_test("svc-b".into(), 3e-7, 100);
        let ctx = build_tick_ctx(&base, Some(&scaph), 500, Some(&cloud), 500, None, 0);
        let snap = ctx.energy_snapshot.as_ref().unwrap();
        assert_eq!(snap.len(), 2);
        // svc-a: Scaphandre wins (1e-7, not 5e-7)
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
        assert!((snap["svc-a"].energy_per_op_kwh - 1e-7).abs() < 1e-15);
        // svc-b: cloud only
        assert_eq!(snap["svc-b"].model_tag, "cloud_specpower");
    }

    #[test]
    fn build_tick_ctx_stale_entries_filtered() {
        // Test staleness via the state's snapshot() method directly.
        // An entry at time 0 with a staleness of 1ms should be stale
        // when queried at time 100.
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("stale-svc".into(), 1e-7, 0);
        let snap = scaph.snapshot(100, 1);
        assert!(
            snap.is_empty(),
            "entry at time 0 should be stale when now=100, staleness=1"
        );
        // A fresh entry should appear.
        scaph.insert_for_test("fresh-svc".into(), 2e-7, 99);
        let snap2 = scaph.snapshot(100, 50);
        assert!(snap2.contains_key("fresh-svc"));
        assert!(!snap2.contains_key("stale-svc"));
    }

    // ------------------------------------------------------------------
    // apply_sampling
    // ------------------------------------------------------------------

    fn make_event(trace_id: &str) -> SpanEvent {
        SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "s1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
        }
    }

    #[test]
    fn apply_sampling_full_rate_returns_all() {
        let events = vec![make_event("t1"), make_event("t2"), make_event("t3")];
        let sampled = apply_sampling(events, 1.0);
        assert_eq!(sampled.len(), 3);
    }

    #[test]
    fn apply_sampling_zero_rate_drops_all() {
        let events = vec![make_event("t1"), make_event("t2")];
        let sampled = apply_sampling(events, 0.0);
        assert!(sampled.is_empty());
    }

    #[test]
    fn apply_sampling_same_trace_id_cached_decision() {
        // The per-trace sampling cache must guarantee that every event
        // sharing a trace_id gets the same keep/drop verdict. At rate
        // 1.0 apply_sampling short-circuits to "keep all" before
        // touching the cache, so we also test a partial rate where the
        // cache-hit branch is actually exercised.
        let events = vec![
            make_event("same-trace"),
            make_event("same-trace"),
            make_event("same-trace"),
            make_event("same-trace"),
        ];
        let sampled = apply_sampling(events, 1.0);
        assert_eq!(
            sampled.len(),
            4,
            "rate 1.0 must keep every event regardless of trace_id"
        );

        // Partial rate: all three events share a trace_id, so the
        // cache forces a single decision. Acceptable outcomes are
        // 0 (all dropped) or 3 (all kept). Anything in between would
        // mean the cache lost the decision, which is exactly the
        // invariant this test is guarding.
        let events2 = vec![
            make_event("cached-trace"),
            make_event("cached-trace"),
            make_event("cached-trace"),
        ];
        let sampled2 = apply_sampling(events2, 0.5);
        assert!(
            sampled2.is_empty() || sampled2.len() == 3,
            "all events for the same trace_id must share the cached \
             decision, got {} of 3 kept (expected 0 or 3)",
            sampled2.len()
        );
    }

    #[test]
    fn apply_sampling_mixed_trace_ids_with_partial_rate() {
        // Sanity test: with 100 distinct trace IDs at rate 0.5, roughly
        // half go through. Exercises the cache-miss + `should_sample`
        // path in apply_sampling.
        let events: Vec<_> = (0..100)
            .map(|i| make_event(&format!("trace-{i}")))
            .collect();
        let sampled = apply_sampling(events, 0.5);
        assert!(
            (10..=90).contains(&sampled.len()),
            "expected ~50 sampled, got {}",
            sampled.len()
        );
    }

    // ------------------------------------------------------------------
    // handle_json_connection (Unix-only, uses UnixStream::pair)
    // ------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn handle_json_connection_happy_path_forwards_events() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (client, server) = UnixStream::pair().expect("UnixStream::pair should succeed");
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);

        // Spawn the connection handler (reads from `server`).
        let handle = tokio::spawn(async move {
            handle_json_connection(server, tx, 1024 * 1024).await;
        });

        // Write one NDJSON line with a minimal valid SpanEvent array,
        // then close the client half so the server sees EOF and returns.
        let line = r#"[{"timestamp":"2025-07-10T14:32:01.123Z","trace_id":"t1","span_id":"s1","service":"svc","type":"sql","operation":"SELECT","target":"SELECT 1","duration_us":100,"source":{"endpoint":"GET /test","method":"m"}}]"#;
        let mut client = client;
        client.write_all(line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        // The handler should send the decoded events through the channel.
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive events within 2s")
            .expect("channel still open");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].trace_id, "t1");

        handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn handle_json_connection_skips_oversize_line() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);

        // Small max_payload so the line is over the limit.
        let handle = tokio::spawn(async move {
            handle_json_connection(server, tx, 32).await;
        });

        let mut client = client;
        // This line is > 32 bytes, triggers the "line exceeds max payload size" branch.
        let oversize_line = r#"[{"timestamp":"2025-07-10T14:32:01.123Z","trace_id":"t1","span_id":"s1","service":"svc","type":"sql","operation":"SELECT","target":"x","duration_us":1,"source":{"endpoint":"/","method":"m"}}]"#;
        client.write_all(oversize_line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        let recv = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            recv.is_err() || recv.unwrap().is_none(),
            "oversize line must be dropped, channel should not receive anything"
        );
        handle.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn handle_json_connection_skips_malformed_line() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);

        let handle = tokio::spawn(async move {
            handle_json_connection(server, tx, 1024 * 1024).await;
        });

        let mut client = client;
        // Malformed: hits the Err(e) branch in the match.
        client.write_all(b"not json at all\n").await.unwrap();
        client.shutdown().await.unwrap();

        let recv = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(
            recv.is_err() || recv.unwrap().is_none(),
            "malformed line must be dropped"
        );
        handle.await.unwrap();
    }

    // ------------------------------------------------------------------
    // run_json_socket (Unix-only, uses a tempdir-scoped socket path)
    // ------------------------------------------------------------------

    /// Build a unique Unix-socket path inside a fresh `tempfile::TempDir`
    /// rooted at `/tmp/`, not `std::env::temp_dir()`.
    ///
    /// Why `/tmp/` instead of `tempfile::tempdir()` (no arg): on macOS
    /// `std::env::temp_dir()` resolves to `/var/folders/<hash>/T/...`,
    /// which easily exceeds the Unix-socket `SUN_LEN` limit (104 bytes
    /// on macOS, 108 on Linux). A `tempfile::TempDir` rooted at `/tmp`
    /// gives us:
    ///
    /// - **Collision-free by construction** (random 6-char suffix from
    ///   `tempfile`, not a timestamp-based pseudo-unique name).
    /// - **Symlink-TOCTOU safe**: the directory is created with
    ///   `mkdir(..., 0o700)` atomically, so a local attacker cannot
    ///   substitute a symlink between path generation and socket bind.
    /// - **Auto-cleanup on drop**: the `TempDir` owner (the test body)
    ///   removes the directory when it goes out of scope, including the
    ///   socket file and the parent dir.
    ///
    /// The returned `TempDir` must be kept alive for the duration of the
    /// test; the returned path borrows from it.
    #[cfg(unix)]
    fn unique_socket_dir_and_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("psd-{name}-"))
            .tempdir_in("/tmp")
            .expect("mkdtemp in /tmp should succeed");
        let path = dir.path().join("daemon.sock");
        (dir, path)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_json_socket_accepts_connection_and_forwards_events() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        // Keep `_dir` alive until the end of the test; drop removes the
        // socket + parent tempdir. `path` is a PathBuf owned by us.
        let (_dir, path) = unique_socket_dir_and_path("accept");
        let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        let path_for_server = path.to_string_lossy().into_owned();
        let server = tokio::spawn(async move {
            run_json_socket(&path_for_server, tx, 1024 * 1024).await;
        });

        // Give the listener a brief moment to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect as a client, write one NDJSON line, close.
        let mut client = UnixStream::connect(&path).await.expect("connect to socket");
        let line = r#"[{"timestamp":"2025-07-10T14:32:01.123Z","trace_id":"t-sock","span_id":"s1","service":"svc","type":"sql","operation":"SELECT","target":"SELECT 1","duration_us":100,"source":{"endpoint":"GET /test","method":"m"}}]"#;
        client.write_all(line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.shutdown().await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("should receive events within 2s")
            .expect("channel still open");
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].trace_id, "t-sock");

        server.abort();
        let _ = server.await;
        // _dir drops here, removing the socket and parent tempdir.
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_json_socket_fails_to_bind_on_invalid_path() {
        // Path inside a non-existent directory → bind returns Err, the
        // function emits a tracing::error and returns without panicking.
        let path = "/nonexistent-directory-for-test/perf-sentinel.sock".to_string();
        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        // Should return near-immediately (bind fails).
        tokio::time::timeout(Duration::from_secs(2), run_json_socket(&path, tx, 1024))
            .await
            .expect("bind failure must return immediately, not hang");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_json_socket_refuses_to_clobber_symlink() {
        // Symlink-TOCTOU regression guard: create a symlink at `path`
        // pointing at a sentinel victim file, call run_json_socket, and
        // verify the victim is NOT deleted (i.e., the symlink-aware
        // pre-check fired and the function returned early).
        use std::os::unix::fs::symlink;

        let (dir, sock_path) = unique_socket_dir_and_path("symlink-guard");
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "important").unwrap();
        // Replace the sock path with a symlink to the victim.
        symlink(&victim, &sock_path).expect("symlink creation");

        let (tx, _rx) = mpsc::channel::<Vec<SpanEvent>>(16);
        let sock_str = sock_path.to_string_lossy().into_owned();
        tokio::time::timeout(Duration::from_secs(2), run_json_socket(&sock_str, tx, 1024))
            .await
            .expect("symlink refusal must return immediately, not hang");

        // Victim must still exist and still contain its original data.
        let content = std::fs::read_to_string(&victim)
            .expect("victim file must still exist after symlink refusal");
        assert_eq!(content, "important");
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

        let (_dir, socket_path) = unique_socket_dir_and_path("daemon-run");
        let socket_path_str = socket_path.to_string_lossy().into_owned();
        let config = Config {
            listen_addr: "127.0.0.1".to_string(),
            listen_port: http_port,
            listen_port_grpc: grpc_port,
            json_socket: socket_path_str,
            trace_ttl_ms: 200, // fast eviction so the ticker fires during test
            max_active_traces: 10,
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

    #[tokio::test]
    async fn daemon_run_rejects_invalid_listen_address() {
        // Malformed listen_addr fails the `format!().parse()` call in
        // `run` before any listener binds. Covers the InvalidAddr path.
        let config = Config {
            listen_addr: "not an address".to_string(),
            ..Config::default()
        };
        // Bogus port paths still reach .parse(), which fails.
        let err = run(config).await.expect_err("should fail");
        assert!(matches!(err, DaemonError::InvalidAddr(_)));
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

    // ── TLS error-path coverage ─────────────────────────────────

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

    // ── current_time_ms branch coverage ─────────────────────────

    #[test]
    fn current_time_ms_is_positive_under_normal_clock() {
        // Under any normal test environment the system clock is well
        // past the Unix epoch; the warning branch is only reachable
        // via clock misconfiguration.
        assert!(current_time_ms() > 0);
    }
}
