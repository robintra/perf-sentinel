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
    let grpc_addr: std::net::SocketAddr = format!(
        "{}:{}",
        config.daemon.listen_addr, config.daemon.listen_port_grpc
    )
    .parse()?;
    let http_addr: std::net::SocketAddr = format!(
        "{}:{}",
        config.daemon.listen_addr, config.daemon.listen_port
    )
    .parse()?;

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
        config.daemon.max_payload_size,
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
    let (Some(cert_path), Some(key_path)) =
        (&config.daemon.tls.cert_path, &config.daemon.tls.key_path)
    else {
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
    let metrics_sink: Arc<dyn crate::ingest::otlp::MetricsSink> = metrics;
    let grpc_service = crate::ingest::otlp::OtlpGrpcService::new(tx, Some(metrics_sink));
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
    if !config.daemon.ack.enabled {
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
    let configured = config.daemon.ack.toml_path.is_some();
    let toml_path = config.daemon.ack.toml_path.as_deref().map_or_else(
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
    let (storage_path, configured) = match &config.daemon.ack.storage_path {
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
    let metrics_sink: Arc<dyn crate::ingest::otlp::MetricsSink> = metrics.clone();
    let otlp_router = crate::ingest::otlp::otlp_http_router(
        tx,
        config.daemon.max_payload_size,
        Some(metrics_sink),
    );
    // Clone the Arc unconditionally so `metrics_route` and the query
    // API state can share it when the API is enabled. When disabled,
    // the extra `Arc::clone` is one atomic refcount increment, not
    // worth a conditional to avoid.
    let metrics_router = crate::report::metrics::metrics_route(Arc::clone(&metrics));
    let health_router = super::health::health_route();
    let mut http_router = otlp_router.merge(metrics_router).merge(health_router);
    if config.daemon.api_enabled {
        let query_state = Arc::new(query_api::QueryApiState {
            findings_store,
            window,
            detect_config: DetectConfig::from(config),
            start_time: std::time::Instant::now(),
            correlator,
            metrics,
            scoring_config: config
                .green
                .electricity_maps
                .as_ref()
                .map(score::carbon::ScoringConfig::from_electricity_maps),
            green_summary,
            ack_store,
            toml_acks,
            ack_api_key: config.daemon.ack.api_key.clone(),
        });
        // CORS scoped to /api/* only, never to OTLP/metrics/health.
        // Locked by `cors_layer_does_not_leak_to_otlp_or_metrics_or_health_routes`.
        // The wildcard + api_key combination is rejected at config load by
        // `Config::validate_daemon_cors`, no runtime check needed here.
        let mut query_router = query_api::query_api_router(query_state);
        if let Some(cors) = build_cors_layer(&config.daemon.cors.allowed_origins) {
            query_router = query_router.layer(cors);
        }
        http_router = http_router.merge(query_router);
    } else {
        // The CORS-vs-API consistency check in `Config::validate`
        // already rejects `daemon.api_enabled = false +
        // cors.allowed_origins != []` at config load, so this branch
        // never sees a non-empty CORS list at runtime.
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

/// Build the CORS layer when the operator configured at least one
/// origin. Returns `None` when the list is empty so callers can skip
/// wiring the layer entirely (and the daemon emits no
/// `Access-Control-Allow-Origin` header on responses, matching the
/// pre-CORS behavior).
///
/// Wildcard mode (`["*"]`) maps to `AllowOrigin::any()`, intended for
/// development. Non-wildcard mode whitelists exact origins. Invalid
/// origins (HTTP-wise unparseable as `HeaderValue`) are dropped with a
/// `warn!` log rather than a panic, so a typo at the end of a long list
/// does not take the daemon down at startup.
fn build_cors_layer(origins: &[String]) -> Option<tower_http::cors::CorsLayer> {
    use axum::http::{HeaderName, Method, header::CONTENT_TYPE};
    use tower_http::cors::{AllowOrigin, CorsLayer};

    if origins.is_empty() {
        return None;
    }
    let allow_origin = if origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        let parsed: Vec<axum::http::HeaderValue> = origins
            .iter()
            .filter_map(|o| match o.parse::<axum::http::HeaderValue>() {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(
                        origin = %o,
                        error = %e,
                        "[daemon.cors] dropping invalid origin"
                    );
                    None
                }
            })
            .collect();
        if parsed.is_empty() {
            tracing::warn!(
                "[daemon.cors] every configured origin was invalid, CORS layer disabled"
            );
            return None;
        }
        AllowOrigin::list(parsed)
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
            // `x-user-id` is not enforced server-side; the `by` field on a
            // POST /api/findings/{sig}/ack body is operator-attested only.
            // Keep the allow-list narrow to what the daemon actually
            // consumes: Content-Type for POST bodies and X-API-Key for
            // ack auth.
            .allow_headers([CONTENT_TYPE, HeaderName::from_static("x-api-key")])
            // 2 minutes preflight cache. Long enough to amortize the
            // OPTIONS roundtrip across a typical user interaction (open
            // report, click ack, click revoke), short enough that a
            // tightened `[daemon.cors] allowed_origins` (rotated origin,
            // dropped tenant) takes effect quickly without a daemon
            // restart on the browser side.
            .max_age(Duration::from_mins(2)),
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
        let socket_path = config.daemon.json_socket.clone();
        let max_payload = config.daemon.max_payload_size;
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
    if !config.daemon.correlation.enabled {
        return None;
    }
    tracing::info!("Cross-trace correlation enabled");
    Some(Arc::new(Mutex::new(
        detect::correlate_cross::CrossTraceCorrelator::new(config.daemon.correlation.clone()),
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
    let Some(scaph_cfg) = config.green.scaphandre.clone() else {
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
    let Some(cloud_cfg) = config.green.cloud_energy.clone() else {
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
    let Some(emaps_cfg) = config.green.electricity_maps.clone() else {
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

#[cfg(test)]
mod cors_tests {
    use super::build_cors_layer;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;
    use tower::ServiceExt;

    /// Synthetic API surface mirroring the daemon's `/api/*` routes
    /// for CORS layer tests. Returned as an unlayered `Router` so each
    /// caller can decide whether to attach `build_cors_layer` directly
    /// (single-router tests) or merge it into a parent router that
    /// also carries OTLP/metrics/health (scoping tests).
    fn build_api_routes() -> Router {
        Router::new()
            .route("/api/status", get(|| async { Json(json!({"ok": true})) }))
            .route(
                "/api/findings/{sig}/ack",
                post(|| async { (StatusCode::CREATED, Json(json!({"ok": true}))) }),
            )
    }

    fn router_with_cors(origins: &[&str]) -> Router {
        let owned: Vec<String> = origins.iter().map(|s| (*s).to_string()).collect();
        let mut router = build_api_routes();
        if let Some(cors) = build_cors_layer(&owned) {
            router = router.layer(cors);
        }
        router
    }

    /// Preflight request builder for the `/api/findings/{sig}/ack`
    /// endpoint. Most preflight tests share the same Origin and target,
    /// only varying the requested method (POST / DELETE / PUT / ...).
    /// Returning the partial `Builder` lets tests that need additional
    /// headers (e.g. `Access-Control-Request-Headers`) chain them on.
    fn preflight_builder(request_method: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/findings/sig123/ack")
            .header(header::ORIGIN, "https://reports.example.com")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, request_method)
    }

    #[tokio::test]
    async fn cors_disabled_when_origins_empty_no_header_emitted() {
        let router = router_with_cors(&[]);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/status")
            .header(header::ORIGIN, "https://example.com")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "default empty origin list must emit no CORS header"
        );
    }

    #[tokio::test]
    async fn cors_wildcard_emits_any_origin() {
        let router = router_with_cors(&["*"]);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/status")
            .header(header::ORIGIN, "https://anything.example.com")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let allow = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .expect("wildcard mode must echo the Allow-Origin header");
        assert_eq!(allow.to_str().unwrap(), "*");
    }

    #[tokio::test]
    async fn cors_specific_origin_echoed_back() {
        let router = router_with_cors(&["https://reports.example.com"]);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/status")
            .header(header::ORIGIN, "https://reports.example.com")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let allow = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .expect("matching origin must be echoed");
        assert_eq!(allow.to_str().unwrap(), "https://reports.example.com");
    }

    #[tokio::test]
    async fn cors_unknown_origin_not_echoed() {
        let router = router_with_cors(&["https://reports.example.com"]);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/status")
            .header(header::ORIGIN, "https://malicious.example.com")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "non-whitelisted origin must not be echoed"
        );
    }

    #[tokio::test]
    async fn cors_preflight_options_includes_post_delete() {
        let router = router_with_cors(&["*"]);
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/findings/sig123/ack")
            .header(header::ORIGIN, "https://reports.example.com")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "x-api-key")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert!(
            response.status().is_success(),
            "preflight must return 2xx, got {:?}",
            response.status()
        );
        let allow_methods = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .expect("preflight must echo Allow-Methods")
            .to_str()
            .unwrap()
            .to_ascii_uppercase();
        assert!(
            allow_methods.contains("POST"),
            "Allow-Methods missing POST: {allow_methods}"
        );
        assert!(
            allow_methods.contains("DELETE"),
            "Allow-Methods missing DELETE: {allow_methods}"
        );
        assert!(
            allow_methods.contains("GET"),
            "Allow-Methods missing GET: {allow_methods}"
        );
    }

    #[tokio::test]
    async fn cors_preflight_allowed_headers_includes_x_api_key() {
        let router = router_with_cors(&["https://reports.example.com"]);
        let request = preflight_builder("POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "content-type, x-api-key",
            )
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        let allow_headers = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .expect("preflight must echo Allow-Headers")
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            allow_headers.contains("x-api-key"),
            "Allow-Headers missing x-api-key: {allow_headers}"
        );
        assert!(
            allow_headers.contains("content-type"),
            "Allow-Headers missing content-type: {allow_headers}"
        );
        // x-user-id was dropped from the allow-list because the daemon
        // does not enforce it server-side. Keep the allow-list narrow
        // to what the API actually consumes.
        assert!(
            !allow_headers.contains("x-user-id"),
            "Allow-Headers should not advertise x-user-id (not enforced server-side): {allow_headers}"
        );
    }

    #[tokio::test]
    async fn cors_preflight_advertises_max_age() {
        let router = router_with_cors(&["*"]);
        let request = preflight_builder("POST").body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let max_age = response
            .headers()
            .get(header::ACCESS_CONTROL_MAX_AGE)
            .expect("preflight must advertise Max-Age")
            .to_str()
            .unwrap();
        // 2 minutes, short enough for a tightened whitelist to take
        // effect on the next browser preflight without a forced refresh.
        assert_eq!(max_age, "120");
    }

    #[test]
    fn cors_invalid_origins_filter_returns_none_when_all_invalid() {
        let origins = vec!["\nbad\norigin".to_string()];
        let layer = build_cors_layer(&origins);
        assert!(
            layer.is_none(),
            "all-invalid input must disable CORS rather than panic"
        );
    }

    #[test]
    fn cors_partial_invalid_origins_keeps_valid_ones() {
        let origins = vec![
            "https://good.example.com".to_string(),
            "\nbad\n".to_string(),
        ];
        let layer = build_cors_layer(&origins);
        assert!(
            layer.is_some(),
            "at least one valid origin must keep the layer enabled"
        );
    }

    #[tokio::test]
    async fn cors_does_not_advertise_allow_credentials() {
        // The CORS layer is built without `allow_credentials(true)`,
        // which keeps the response compatible with `AllowOrigin::any()`
        // and matches the X-API-Key-as-header (not cookie) auth model.
        // Catch a regression that flipped the default.
        let router = router_with_cors(&["*"]);
        let request = preflight_builder("POST").body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .is_none(),
            "allow_credentials must stay false, got: {:?}",
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
        );
    }

    #[tokio::test]
    async fn cors_preflight_does_not_advertise_put_or_patch() {
        // The allow-list is intentionally narrow: GET, POST, DELETE,
        // OPTIONS only. PUT and PATCH are not part of the daemon's
        // public surface; if a future handler adds one, the CORS
        // allow-list must be extended in lockstep, not silently
        // permissive.
        let router = router_with_cors(&["*"]);
        let request = preflight_builder("PUT").body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let allow_methods = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .map(|v| v.to_str().unwrap_or("").to_ascii_uppercase())
            .unwrap_or_default();
        assert!(
            !allow_methods.split(',').any(|m| m.trim() == "PUT"),
            "PUT must not be advertised, got: {allow_methods}"
        );
        assert!(
            !allow_methods.split(',').any(|m| m.trim() == "PATCH"),
            "PATCH must not be advertised, got: {allow_methods}"
        );
    }

    #[tokio::test]
    async fn cors_layer_does_not_leak_to_otlp_or_metrics_or_health_routes() {
        // Mirror the real `build_http_router` topology: OTLP +
        // /metrics + /health are merged FIRST without CORS, then the
        // /api/* sub-router is built separately, layered with CORS,
        // and merged in. Axum's `Router::merge` preserves per-router
        // layer scoping, so an `OPTIONS /v1/traces` from a wildcard
        // origin must NOT echo `Access-Control-Allow-Origin`. A
        // future refactor that flips merge order, swaps `merge` for
        // `nest`, or moves the layer to the outer router would break
        // this property; the test locks the security-load-bearing
        // invariant in.
        let outer_routes = Router::new()
            .route("/v1/traces", post(|| async { StatusCode::OK }))
            .route("/metrics", get(|| async { "metrics" }))
            .route("/health", get(|| async { "health" }));
        let cors = build_cors_layer(&["*".to_string()]).expect("wildcard mode produces a layer");
        let api_routes = build_api_routes().layer(cors);
        let router = outer_routes.merge(api_routes);

        // Probe each non-API route with an Origin header; CORS must
        // not echo back. Cover both the actual-request method and the
        // unsolicited preflight: the most realistic browser-side leak
        // vector for a wildcard CORS misconfig is an attacker page
        // issuing `OPTIONS /v1/traces` to discover the OTLP surface.
        for path in ["/v1/traces", "/metrics", "/health"] {
            let actual_method = if path == "/v1/traces" {
                Method::POST
            } else {
                Method::GET
            };
            for method in [actual_method.clone(), Method::OPTIONS] {
                let mut builder = Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header(header::ORIGIN, "https://attacker.example.com");
                if method == Method::OPTIONS {
                    builder = builder.header(
                        header::ACCESS_CONTROL_REQUEST_METHOD,
                        actual_method.as_str(),
                    );
                }
                let request = builder.body(Body::empty()).unwrap();
                let response = router.clone().oneshot(request).await.unwrap();
                let allow_origin = response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN);
                assert!(
                    allow_origin.is_none(),
                    "{method} {path} must not echo Allow-Origin under CORS scoping, got: {allow_origin:?}"
                );
                let allow_methods = response.headers().get(header::ACCESS_CONTROL_ALLOW_METHODS);
                assert!(
                    allow_methods.is_none(),
                    "{method} {path} must not advertise Allow-Methods under CORS scoping, got: {allow_methods:?}"
                );
            }
        }

        // Sanity check the positive: /api/status DOES echo Allow-Origin.
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/status")
            .header(header::ORIGIN, "https://attacker.example.com")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("*"),
            "the /api/* sub-router must still echo Allow-Origin",
        );
    }
}
