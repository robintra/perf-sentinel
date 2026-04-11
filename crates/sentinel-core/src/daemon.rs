//! Daemon mode: streaming detection with OTLP and JSON ingestion.
//!
//! Runs an event loop that receives spans from multiple sources (OTLP gRPC,
//! OTLP HTTP, JSON socket), accumulates them in a `TraceWindow`, and emits
//! findings as NDJSON on stdout when traces expire.

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
#[derive(Debug, thiserror::Error)]
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
#[allow(clippy::too_many_lines)]
pub async fn run(config: Config) -> Result<(), DaemonError> {
    let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(1024);

    let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
        max_events_per_trace: config.max_events_per_trace,
        trace_ttl_ms: config.trace_ttl_ms,
        max_active_traces: std::num::NonZeroUsize::new(config.max_active_traces)
            .expect("config validates max_active_traces >= 1"),
    })));

    let max_payload = config.max_payload_size;

    // Create Prometheus metrics state
    let metrics = Arc::new(MetricsState::new());

    // Parse and validate addresses before spawning
    let grpc_addr: std::net::SocketAddr =
        format!("{}:{}", config.listen_addr, config.listen_port_grpc).parse()?;
    let http_addr: std::net::SocketAddr =
        format!("{}:{}", config.listen_addr, config.listen_port).parse()?;

    // Bind both listeners before spawning (fail fast if ports are taken)
    let http_listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .map_err(DaemonError::HttpBind)?;
    let grpc_incoming =
        tonic::transport::server::TcpIncoming::bind(grpc_addr).map_err(DaemonError::GrpcBind)?;

    // JoinHandles captured so Ctrl-C can abort cleanly.
    let grpc_service = crate::ingest::otlp::OtlpGrpcService::new(tx.clone());
    let grpc_handle = tokio::spawn(async move {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer;
        tracing::info!("OTLP gRPC listening on {grpc_addr}");
        if let Err(e) = tonic::transport::Server::builder()
            .timeout(Duration::from_secs(60))
            .add_service(
                TraceServiceServer::new(grpc_service).max_decoding_message_size(max_payload),
            )
            .serve_with_incoming(grpc_incoming)
            .await
        {
            tracing::error!("gRPC server error: {e}");
        }
    });

    // OTLP HTTP + metrics.
    let otlp_router = crate::ingest::otlp::otlp_http_router(tx.clone(), max_payload);
    let metrics_router = crate::report::metrics::metrics_route(metrics.clone());
    let http_router = otlp_router.merge(metrics_router).layer(
        tower::ServiceBuilder::new()
            .layer(axum::error_handling::HandleErrorLayer::new(|_| async {
                tracing::debug!("HTTP request timed out");
                axum::http::StatusCode::REQUEST_TIMEOUT
            }))
            .layer(tower::timeout::TimeoutLayer::new(Duration::from_secs(60))),
    );
    let http_handle = tokio::spawn(async move {
        tracing::info!("OTLP HTTP listening on {http_addr}");
        if let Err(e) = axum::serve(http_listener, http_router).await {
            tracing::error!("HTTP server error: {e}");
        }
    });

    // JSON socket (Unix only). Socket file unlinked on shutdown.
    #[cfg(unix)]
    let json_socket_handle: Option<tokio::task::JoinHandle<()>> = {
        let socket_path = config.json_socket.clone();
        let socket_tx = tx.clone();
        let max_payload = config.max_payload_size;
        Some(tokio::spawn(async move {
            run_json_socket(&socket_path, socket_tx, max_payload).await;
        }))
    };
    #[cfg(not(unix))]
    let json_socket_handle: Option<tokio::task::JoinHandle<()>> = {
        tracing::warn!("JSON socket ingestion not available on this platform; use OTLP HTTP/gRPC");
        None
    };

    let detect_config = DetectConfig::from(&config);
    // Base carbon context used as a template. The actual context passed
    // to `process_traces` each tick is cloned from this with
    // `energy_snapshot` patched in from the shared energy states
    // (Scaphandre and/or cloud SPECpower).
    let base_carbon_ctx = config.carbon_context();
    let green_enabled = config.green_enabled;
    let sampling_rate = config.sampling_rate;
    let evict_ms = config.trace_ttl_ms / 2;
    // cache the confidence label once. The daemon stamps this
    // on every finding in `process_traces`. `analyze` batch mode uses
    // `Confidence::CiBatch` instead (stamped in `pipeline::analyze_with_traces`).
    let confidence = config.confidence();

    // optionally spawn the Scaphandre scraper. Absent config
    // → None → scoring uses the proxy model. Present config → spawn a
    // background task that updates `scaphandre_state` every
    // `scrape_interval_secs` and the staleness threshold used by the
    // snapshot read is 3× the interval (hung-scraper defense).
    let (scaphandre_state, scraper_handle, staleness_ms) = if let Some(scaph_cfg) =
        config.green_scaphandre.clone()
    {
        let staleness = scaph_cfg.scrape_interval.as_millis() as u64 * 3;
        let state = ScaphandreState::new();
        let handle = score::scaphandre::spawn_scraper(scaph_cfg, state.clone(), metrics.clone());
        (Some(state), Some(handle), staleness)
    } else {
        (None, None, 0)
    };

    // Same pattern for cloud energy scraper.
    let (cloud_state, cloud_handle, cloud_staleness_ms) =
        if let Some(cloud_cfg) = config.green_cloud_energy.clone() {
            let staleness = cloud_cfg.scrape_interval.as_millis() as u64 * 3;
            let state = CloudEnergyState::new();
            let handle =
                score::cloud_energy::spawn_cloud_scraper(cloud_cfg, state.clone(), metrics.clone());
            (Some(state), Some(handle), staleness)
        } else {
            (None, None, 0)
        };

    // Same pattern for Electricity Maps real-time intensity scraper.
    let (emaps_state, emaps_handle, emaps_staleness_ms) =
        if let Some(emaps_cfg) = config.green_electricity_maps.clone() {
            let staleness = emaps_cfg.poll_interval.as_millis() as u64 * 3;
            let state = ElectricityMapsState::new();
            let handle =
                score::electricity_maps::spawn_electricity_maps_scraper(emaps_cfg, state.clone());
            (Some(state), Some(handle), staleness)
        } else {
            (None, None, 0)
        };

    // Cardinality cap on the per-service Prometheus counter.
    // Prevents OOM from a malicious OTLP sender injecting millions of
    // unique service.name values. Events beyond the cap are still
    // processed for detection, just not metered per-service.
    let max_service_cardinality: usize = 1024;
    let mut known_services: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut service_cap_warned = false;

    // Main event loop
    let mut ticker = interval(Duration::from_millis(evict_ms.max(100)));
    // Prevent burst-catchup if process_traces takes longer than the tick
    // interval. Both the Scaphandre and cloud scrapers already use Delay.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            Some(events) = rx.recv() => {
                let events = apply_sampling(events, sampling_rate);
                let event_count = events.len();
                // Normalize OUTSIDE the lock to minimize lock hold time.
                let normalized: Vec<_> = events
                    .into_iter()
                    .map(normalize::normalize)
                    .collect();
                // Increment the per-service I/O op counter with a
                // cardinality cap. The Scaphandre/cloud energy scrapers
                // read this counter at each tick via snapshot-diff.
                for event in &normalized {
                    let service = event.event.service.as_str();
                    if known_services.contains(service) {
                        // Fast path: known service, no allocation.
                        metrics
                            .service_io_ops_total
                            .with_label_values(&[service])
                            .inc();
                    } else if known_services.len() < max_service_cardinality {
                        // New service under the cap: allocate once.
                        known_services.insert(service.to_string());
                        metrics
                            .service_io_ops_total
                            .with_label_values(&[service])
                            .inc();
                    } else if !service_cap_warned {
                        tracing::warn!(
                            cap = max_service_cardinality,
                            "Service cardinality cap reached; new services will \
                             not have per-service I/O op counters"
                        );
                        service_cap_warned = true;
                    }
                }
                let now_ms = current_time_ms();
                let mut lru_evicted = Vec::new();
                {
                    // Lock held for O(batch_size) push() calls. Each push
                    // is O(1) amortized (LRU insert/promote). Batch size is
                    // bounded by the mpsc channel capacity (1024) and
                    // max_payload_size, so lock duration is bounded.
                    let mut w = window.lock().await;
                    for event in normalized {
                        if let Some(evicted) = w.push(event, now_ms) {
                            lru_evicted.push(evicted);
                        }
                    }
                    metrics.active_traces.set(w.active_traces() as f64);
                }
                metrics.events_processed_total.inc_by(event_count as f64);
                // Process LRU-evicted traces so their findings are not lost.
                if !lru_evicted.is_empty() {
                    let tick_ctx = build_tick_ctx(
                        &base_carbon_ctx,
                        scaphandre_state.as_deref(),
                        staleness_ms,
                        cloud_state.as_deref(),
                        cloud_staleness_ms,
                        emaps_state.as_deref(),
                        emaps_staleness_ms,
                    );
                    process_traces(
                        lru_evicted,
                        &detect_config,
                        green_enabled,
                        &tick_ctx,
                        &metrics,
                        confidence,
                    );
                }
            }
            _ = ticker.tick() => {
                let now_ms = current_time_ms();
                let expired = {
                    let mut w = window.lock().await;
                    let expired = w.evict_expired(now_ms);
                    metrics.active_traces.set(w.active_traces() as f64);
                    expired
                };
                if !expired.is_empty() {
                    let tick_ctx = build_tick_ctx(
                        &base_carbon_ctx,
                        scaphandre_state.as_deref(),
                        staleness_ms,
                        cloud_state.as_deref(),
                        cloud_staleness_ms,
                        emaps_state.as_deref(),
                        emaps_staleness_ms,
                    );
                    process_traces(
                        expired,
                        &detect_config,
                        green_enabled,
                        &tick_ctx,
                        &metrics,
                        confidence,
                    );
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Shutting down daemon, processing remaining traces...");
                // Abort all spawned tasks BEFORE draining the window
                // and processing the remaining traces. This stops
                // any in-flight `tracing::error!` from the listener
                // tasks landing AFTER the "Shutting down" message,
                // and prevents the runtime from leaking the tasks
                // when `daemon::run` returns.
                //
                // Order matters slightly: kill the Scaphandre scraper
                // first so its log lines don't interleave with the
                // shutdown message, then the listeners.
                if let Some(handle) = &emaps_handle {
                    handle.abort();
                }
                if let Some(handle) = &cloud_handle {
                    handle.abort();
                }
                if let Some(handle) = &scraper_handle {
                    handle.abort();
                }
                grpc_handle.abort();
                http_handle.abort();
                if let Some(handle) = &json_socket_handle {
                    handle.abort();
                }
                let remaining = {
                    let mut w = window.lock().await;
                    w.drain_all()
                };
                let tick_ctx = build_tick_ctx(
                    &base_carbon_ctx,
                    scaphandre_state.as_deref(),
                    staleness_ms,
                    cloud_state.as_deref(),
                    cloud_staleness_ms,
                    emaps_state.as_deref(),
                    emaps_staleness_ms,
                );
                process_traces(
                    remaining,
                    &detect_config,
                    green_enabled,
                    &tick_ctx,
                    &metrics,
                    confidence,
                );
                // Best-effort socket cleanup. `run_json_socket` doesn't
                // unlink the Unix socket file on exit, so without this
                // a leftover socket file blocks the next daemon start
                // until manual `rm`. Ignore errors — if the file is
                // gone or unreachable, the next bind will fail loudly.
                #[cfg(unix)]
                {
                    let _ = std::fs::remove_file(&config.json_socket);
                }
                break;
            }
        }
    }

    Ok(())
}

/// Build a per-tick `CarbonContext` by cloning the base context and
/// patching in a fresh energy snapshot merged from all configured
/// energy sources (Scaphandre RAPL and/or cloud `SPECpower`).
///
/// Called right before every `process_traces` invocation so each tick
/// sees the latest measured coefficients. When neither energy source
/// is configured, returns a clone of the base context with
/// `energy_snapshot = None`.
///
/// Scaphandre entries take precedence over cloud entries for the same
/// service (direct RAPL measurement beats `SPECpower` interpolation).
fn build_tick_ctx(
    base: &score::carbon::CarbonContext,
    scaphandre_state: Option<&ScaphandreState>,
    scaphandre_staleness_ms: u64,
    cloud_state: Option<&CloudEnergyState>,
    cloud_staleness_ms: u64,
    emaps_state: Option<&ElectricityMapsState>,
    emaps_staleness_ms: u64,
) -> score::carbon::CarbonContext {
    let now = score::scaphandre::monotonic_ms();

    let mut merged: std::collections::HashMap<String, score::carbon::EnergyEntry> =
        std::collections::HashMap::new();

    // Cloud entries first (lower precedence).
    if let Some(state) = cloud_state {
        let snap = state.snapshot(now, cloud_staleness_ms);
        for (service, energy_kwh) in snap {
            merged.insert(service, score::carbon::EnergyEntry::cloud(energy_kwh));
        }
    }

    // Scaphandre entries override cloud for the same service.
    if let Some(state) = scaphandre_state {
        let snap = state.snapshot(now, scaphandre_staleness_ms);
        for (service, energy_kwh) in snap {
            merged.insert(service, score::carbon::EnergyEntry::scaphandre(energy_kwh));
        }
    }

    let mut ctx = base.clone();
    ctx.energy_snapshot = if merged.is_empty() {
        None
    } else {
        Some(merged)
    };

    // Electricity Maps real-time intensity (independent of energy snapshot).
    if let Some(state) = emaps_state {
        let snap = state.snapshot(now, emaps_staleness_ms);
        if !snap.is_empty() {
            ctx.real_time_intensity = Some(snap);
        }
    }

    ctx
}

/// Process a batch of completed/expired traces: detect, score, emit NDJSON.
///
/// stamps `confidence` on every finding after detection. The
/// value is derived from `config.daemon_environment` in `run()` and passed
/// here unchanged. `analyze` batch mode does not call this function; it
/// uses `pipeline::analyze_with_traces` which hardcodes
/// `Confidence::CiBatch`.
fn process_traces(
    traces: Vec<(String, Vec<normalize::NormalizedEvent>)>,
    detect_config: &DetectConfig,
    green_enabled: bool,
    carbon_ctx: &score::carbon::CarbonContext,
    metrics: &MetricsState,
    confidence: Confidence,
) {
    if traces.is_empty() {
        return;
    }

    let trace_count = traces.len();
    let trace_structs: Vec<Trace> = traces
        .into_iter()
        .map(|(trace_id, spans)| Trace { trace_id, spans })
        .collect();

    let mut findings = detect::detect(&trace_structs, detect_config);

    // Cross-trace slow percentile analysis. The internal detector
    // requires >= 2 distinct traces per template, so we gate on 2.
    if trace_structs.len() >= 2 {
        findings.extend(detect::slow::detect_slow_cross_trace(
            &trace_structs,
            detect_config.slow_threshold_ms,
            detect_config.slow_min_occurrences,
        ));
    }

    let (mut findings, green_summary) = if green_enabled {
        score::score_green(&trace_structs, findings, Some(carbon_ctx))
    } else {
        let total_io_ops = trace_structs.iter().map(|t| t.spans.len()).sum();
        (findings, GreenSummary::disabled(total_io_ops))
    };

    // stamp the daemon's confidence label. Detectors emitted
    // `Confidence::default()` (= CiBatch); overwrite with the real value
    // captured from Config at daemon startup.
    for finding in &mut findings {
        finding.confidence = confidence;
    }
    let findings = findings;

    // Update Prometheus metrics
    metrics.traces_analyzed_total.inc_by(trace_count as f64);
    metrics
        .total_io_ops
        .inc_by(green_summary.total_io_ops as f64);
    metrics
        .avoidable_io_ops
        .inc_by(green_summary.avoidable_io_ops as f64);
    // Note: io_waste_ratio is a cumulative all-time ratio, not windowed.
    // Users can compute a windowed rate from the raw counters using Prometheus rate().
    let cumulative_total = metrics.total_io_ops.get();
    if cumulative_total > 0.0 {
        metrics
            .io_waste_ratio
            .set(metrics.avoidable_io_ops.get() / cumulative_total);
    }
    // Update exemplar tracking for Grafana click-through
    metrics.record_exemplars(&findings, &green_summary);

    {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        for finding in &findings {
            metrics
                .findings_total
                .with_label_values(&[finding.finding_type.as_str(), finding.severity.as_str()])
                .inc();
            if serde_json::to_writer(&mut lock, finding).is_ok() {
                let _ = writeln!(lock);
            }
        }
    }
}

/// Apply trace-level sampling: cache decisions per `trace_id` to avoid
/// redundant hashing for events sharing a trace. Only clones `trace_id`
/// for the first event of each trace (cache miss), not on hits.
fn apply_sampling(events: Vec<SpanEvent>, rate: f64) -> Vec<SpanEvent> {
    if rate >= 1.0 {
        return events;
    }
    let mut cache = std::collections::HashMap::<String, bool>::new();
    events
        .into_iter()
        .filter(|e| {
            if let Some(&decision) = cache.get(e.trace_id.as_str()) {
                return decision;
            }
            let decision = should_sample(&e.trace_id, rate);
            cache.insert(e.trace_id.clone(), decision);
            decision
        })
        .collect()
}

/// Deterministic per-trace sampling using a simple hash.
///
/// Returns `true` if the trace should be processed, `false` if dropped.
/// Uses a fast hash of the `trace_id` to produce a value in `[0.0, 1.0)`.
fn should_sample(trace_id: &str, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 {
        return false;
    }
    // FNV-1a inspired hash for speed
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in trace_id.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (hash as f64 / u64::MAX as f64) < rate
}

/// Get current time in milliseconds since epoch.
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Run the JSON socket listener on Unix platforms.
///
/// Reads newline-delimited JSON (NDJSON): each line is a JSON array of `SpanEvent`s.
#[cfg(unix)]
async fn run_json_socket(path: &str, tx: mpsc::Sender<Vec<SpanEvent>>, max_payload_size: usize) {
    use tokio::net::UnixListener;

    // Clean up stale socket file
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

    #[test]
    fn process_traces_empty_does_nothing() {
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        process_traces(
            vec![],
            &default_detect_config(),
            true,
            &ctx,
            &metrics,
            Confidence::DaemonStaging,
        );
    }

    #[test]
    fn process_traces_with_n_plus_one() {
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
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            true,
            &ctx,
            &metrics,
            Confidence::DaemonStaging,
        );
    }

    #[test]
    fn process_traces_clean_no_finding() {
        // 2 events with different templates -> no finding
        let events = vec![
            make_normalized("t1", "SELECT * FROM users WHERE id = 1"),
            make_normalized("t1", "SELECT * FROM orders WHERE id = 2"),
        ];
        let metrics = MetricsState::new();
        let ctx = empty_carbon_ctx();
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            true,
            &ctx,
            &metrics,
            Confidence::DaemonStaging,
        );
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

    #[test]
    fn process_traces_updates_metrics() {
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
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            true,
            &ctx,
            &metrics,
            Confidence::DaemonStaging,
        );

        let output = metrics.render();
        assert!(output.contains("perf_sentinel_traces_analyzed_total"));
        assert!(output.contains("perf_sentinel_findings_total"));
    }

    #[test]
    fn process_traces_green_disabled() {
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
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            false, // green_enabled = false
            &ctx,
            &metrics,
            Confidence::DaemonStaging,
        );
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
    fn build_tick_ctx_no_scrapers_yields_none_snapshot() {
        let base = score::carbon::CarbonContext::default();
        let ctx = build_tick_ctx(&base, None, 0, None, 0, None, 0);
        assert!(ctx.energy_snapshot.is_none());
    }

    #[test]
    fn build_tick_ctx_scaphandre_only() {
        let base = score::carbon::CarbonContext::default();
        let scaph = ScaphandreState::new();
        scaph.insert_for_test("svc-a".into(), 1e-7, 100);
        let ctx = build_tick_ctx(&base, Some(&scaph), 500, None, 0, None, 0);
        let snap = ctx.energy_snapshot.unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["svc-a"].model_tag, "scaphandre_rapl");
    }

    #[test]
    fn build_tick_ctx_cloud_only() {
        let base = score::carbon::CarbonContext::default();
        let cloud = CloudEnergyState::new();
        cloud.insert_for_test("svc-b".into(), 2e-7, 100);
        let ctx = build_tick_ctx(&base, None, 0, Some(&cloud), 500, None, 0);
        let snap = ctx.energy_snapshot.unwrap();
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
        let snap = ctx.energy_snapshot.unwrap();
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
}
