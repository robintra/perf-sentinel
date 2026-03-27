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
use crate::detect::DetectConfig;
use crate::event::SpanEvent;
use crate::normalize;
use crate::report::metrics::MetricsState;
use crate::score;

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
#[allow(clippy::too_many_lines)]
pub async fn run(config: Config) -> Result<(), DaemonError> {
    let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(1024);

    let window = Arc::new(Mutex::new(TraceWindow::new(WindowConfig {
        max_events_per_trace: config.max_events_per_trace,
        trace_ttl_ms: config.trace_ttl_ms,
        max_active_traces: config.max_active_traces,
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

    // Spawn OTLP gRPC server (listener already bound)
    let grpc_service = crate::ingest::otlp::OtlpGrpcService::new(tx.clone());
    tokio::spawn(async move {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer;
        tracing::info!("OTLP gRPC listening on {grpc_addr}");
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(
                TraceServiceServer::new(grpc_service).max_decoding_message_size(max_payload),
            )
            .serve_with_incoming(grpc_incoming)
            .await
        {
            tracing::error!("gRPC server error: {e}");
        }
    });

    // Spawn OTLP HTTP server with metrics endpoint merged
    let otlp_router = crate::ingest::otlp::otlp_http_router(tx.clone(), max_payload);
    let metrics_router = crate::report::metrics::metrics_route(metrics.clone());
    let http_router = otlp_router.merge(metrics_router);
    tokio::spawn(async move {
        tracing::info!("OTLP HTTP listening on {http_addr}");
        if let Err(e) = axum::serve(http_listener, http_router).await {
            tracing::error!("HTTP server error: {e}");
        }
    });

    // Spawn JSON socket listener (Unix only)
    #[cfg(unix)]
    {
        let socket_path = config.json_socket.clone();
        let socket_tx = tx.clone();
        let max_payload = config.max_payload_size;
        tokio::spawn(async move {
            run_json_socket(&socket_path, socket_tx, max_payload).await;
        });
    }
    #[cfg(not(unix))]
    {
        tracing::warn!("JSON socket ingestion not available on this platform; use OTLP HTTP/gRPC");
    }

    let detect_config = DetectConfig {
        n_plus_one_threshold: config.n_plus_one_threshold,
        window_ms: config.window_duration_ms,
        slow_threshold_ms: config.slow_query_threshold_ms,
        slow_min_occurrences: config.slow_query_min_occurrences,
    };
    let green_region = config.green_region.clone();
    let evict_ms = config.trace_ttl_ms / 2;

    // Main event loop
    let mut ticker = interval(Duration::from_millis(evict_ms.max(100)));

    loop {
        tokio::select! {
            Some(events) = rx.recv() => {
                let event_count = events.len();
                // Normalize OUTSIDE the lock to minimize lock hold time.
                let normalized: Vec<_> = events
                    .into_iter()
                    .map(normalize::normalize)
                    .collect();
                let now_ms = current_time_ms();
                let mut lru_evicted = Vec::new();
                {
                    let mut w = window.lock().await;
                    for event in normalized {
                        if let Some(evicted) = w.push(event, now_ms) {
                            lru_evicted.push(evicted);
                        }
                    }
                    metrics.active_traces.set(w.active_traces() as f64);
                }
                metrics.events_processed_total.inc_by(event_count as f64);
                // Process LRU-evicted traces so their findings are not lost
                if !lru_evicted.is_empty() {
                    process_traces(
                        lru_evicted,
                        &detect_config,
                        green_region.as_deref(),
                        &metrics,
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
                process_traces(
                    expired,
                    &detect_config,
                    green_region.as_deref(),
                    &metrics,
                );
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Shutting down daemon, processing remaining traces...");
                let remaining = {
                    let mut w = window.lock().await;
                    w.drain_all()
                };
                process_traces(
                    remaining,
                    &detect_config,
                    green_region.as_deref(),
                    &metrics,
                );
                break;
            }
        }
    }

    Ok(())
}

/// Process a batch of completed/expired traces: detect, score, emit NDJSON.
fn process_traces(
    traces: Vec<(String, Vec<crate::normalize::NormalizedEvent>)>,
    detect_config: &DetectConfig,
    green_region: Option<&str>,
    metrics: &MetricsState,
) {
    if traces.is_empty() {
        return;
    }

    let trace_count = traces.len();
    let trace_structs: Vec<Trace> = traces
        .into_iter()
        .map(|(trace_id, spans)| Trace { trace_id, spans })
        .collect();

    let findings = detect::detect(&trace_structs, detect_config);
    let (findings, green_summary) = score::score_green(&trace_structs, findings, green_region);

    // Update Prometheus metrics
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
    for finding in &findings {
        metrics
            .findings_total
            .with_label_values(&[finding.finding_type.as_str(), finding.severity.as_str()])
            .inc();
    }

    for finding in &findings {
        if let Ok(json) = serde_json::to_string(finding) {
            println!("{json}");
        }
    }
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
    use tokio::io::AsyncBufReadExt;
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
    tracing::info!("JSON socket listening on {path}");

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let tx = tx.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    // Bound total bytes per connection to prevent OOM from
                    // a single huge line without a newline.
                    let limited = stream.take(max_payload_size as u64 * 16);
                    let reader = tokio::io::BufReader::new(limited);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if line.len() > max_payload_size {
                            tracing::warn!("JSON socket: line exceeds max payload size, skipping");
                            continue;
                        }
                        let ingest = crate::ingest::json::JsonIngest::new(max_payload_size);
                        if let Ok(events) =
                            crate::ingest::IngestSource::ingest(&ingest, line.as_bytes())
                            && !events.is_empty()
                            && tx.send(events).await.is_err()
                        {
                            tracing::warn!("JSON socket: event channel closed");
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                tracing::error!("Unix socket accept error: {e}");
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

    fn make_normalized(trace_id: &str, target: &str) -> crate::normalize::NormalizedEvent {
        normalize::normalize(SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "s1".to_string(),
            service: "test".to_string(),
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
        })
    }

    fn default_detect_config() -> DetectConfig {
        DetectConfig {
            n_plus_one_threshold: 5,
            window_ms: 500,
            slow_threshold_ms: 500,
            slow_min_occurrences: 3,
        }
    }

    #[test]
    fn process_traces_empty_does_nothing() {
        let metrics = MetricsState::new();
        process_traces(vec![], &default_detect_config(), None, &metrics);
    }

    #[test]
    fn process_traces_with_n_plus_one() {
        // 6 events with different params -> N+1 finding
        let events: Vec<_> = (1..=6)
            .map(|i| make_normalized("t1", &format!("SELECT * FROM player WHERE game_id = {i}")))
            .collect();
        let metrics = MetricsState::new();
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            None,
            &metrics,
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
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            None,
            &metrics,
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

        let event = crate::normalize::normalize(crate::event::SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: "t1".to_string(),
            span_id: "s1".to_string(),
            service: "test".to_string(),
            event_type: crate::event::EventType::Sql,
            operation: "SELECT".to_string(),
            target: "SELECT 1".to_string(),
            duration_us: 100,
            source: crate::event::EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
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
            .map(|i| make_normalized("t1", &format!("SELECT * FROM player WHERE game_id = {i}")))
            .collect();
        let metrics = MetricsState::new();
        process_traces(
            vec![("t1".to_string(), events)],
            &default_detect_config(),
            None,
            &metrics,
        );

        let output = metrics.render();
        assert!(output.contains("perf_sentinel_traces_analyzed_total"));
        assert!(output.contains("perf_sentinel_findings_total"));
    }
}
