//! OTLP capture: receive spans over OTLP and write them straight back out as
//! an NDJSON trace file, one request per line, without analyzing anything.
//!
//! This is the CI counterpart of the daemon. A test suite exports over the
//! network, exactly as it would in production, and `analyze --ci` then gates
//! on the file. It exists because several runtimes cannot hand a trace file
//! over any other way: Java has no OTLP file exporter, and a forked Maven
//! test JVM cannot even yield its stdout, which Surefire uses as its command
//! channel. See `docs/INSTRUMENTATION.md`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::ingest::otlp::{OtlpGrpcService, OtlpSink, otlp_http_router_with_sink};

/// Requests buffered between the listeners and the writer task. The writer
/// only serialises and appends, so it never falls far behind; this bound is
/// what keeps a flood bounded in memory rather than a promise of throughput.
const CHANNEL_CAPACITY: usize = 256;

/// Per-request decode cap. Larger than the daemon's 1 MB default: a CI suite
/// flushes its whole batch at JVM shutdown, and a rejected batch here means a
/// silently incomplete trace file rather than a dropped window.
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Where and how to capture.
pub struct CaptureConfig {
    pub listen_addr: String,
    pub port_grpc: u16,
    pub port_http: u16,
    pub output: PathBuf,
    /// Stop appending past this size. Prevents a runaway exporter from
    /// filling the CI agent's disk.
    pub max_file_bytes: u64,
    /// How long to keep listening after the shutdown signal, so the
    /// exporter's last flush still lands.
    pub grace: Duration,
}

/// What a capture run produced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CaptureStats {
    pub requests: u64,
    pub spans: u64,
    pub bytes: u64,
    /// True when `max_file_bytes` was hit and spans were dropped. The file
    /// stays valid NDJSON, but it no longer describes the whole run, so a
    /// verdict computed from it would be optimistic.
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("cannot bind {addr}: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write trace file {path}: {source}")]
    Output {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serializing an OTLP request failed: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Serialise one OTLP request as a single NDJSON line, newline included.
///
/// The output is read back by [`crate::ingest::json::JsonIngest`], which
/// parses this exact shape into the same type, so the round trip is symmetric
/// by construction. `ndjson_line_round_trips_through_analyze` is what proves it.
fn encode_request(request: &ExportTraceServiceRequest) -> serde_json::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    Ok(line)
}

/// Total spans across a request, for the run summary.
fn count_spans(request: &ExportTraceServiceRequest) -> u64 {
    request
        .resource_spans
        .iter()
        .flat_map(|rs| rs.scope_spans.iter())
        .map(|ss| ss.spans.len() as u64)
        .sum()
}

/// Append one request, honouring the size cap. Past the cap the run keeps
/// draining but stops writing, so the file stays valid and the caller is told.
async fn write_one<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    request: &ExportTraceServiceRequest,
    stats: &mut CaptureStats,
    path: &Path,
    max_file_bytes: u64,
) -> Result<(), CaptureError> {
    let line = encode_request(request)?;
    if stats.bytes + line.len() as u64 > max_file_bytes {
        if !stats.truncated {
            tracing::warn!(
                max_file_bytes,
                "capture size limit reached, no longer appending: the trace \
                 file is incomplete and any verdict from it understates the run"
            );
        }
        stats.truncated = true;
        return Ok(());
    }
    writer
        .write_all(&line)
        .await
        .map_err(|source| CaptureError::Output {
            path: path.to_path_buf(),
            source,
        })?;
    stats.requests += 1;
    stats.spans += count_spans(request);
    stats.bytes += line.len() as u64;
    Ok(())
}

/// Drain the channel into the trace file, one line per request. Ordering is
/// the arrival order because a single task owns the file, no lock involved.
///
/// The loop ends on `stop`, then drains what is already queued. It does not
/// wait for the senders to be dropped: tonic spawns a task per connection and
/// aborting the accept loop leaves those tasks, and their sender clones,
/// alive. Closing the receiver is what makes shutdown deterministic.
async fn write_loop(
    mut rx: mpsc::Receiver<ExportTraceServiceRequest>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    path: &Path,
    max_file_bytes: u64,
) -> Result<CaptureStats, CaptureError> {
    let file = tokio::fs::File::create(path)
        .await
        .map_err(|source| CaptureError::Output {
            path: path.to_path_buf(),
            source,
        })?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut stats = CaptureStats::default();

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Some(request) => {
                    write_one(&mut writer, &request, &mut stats, path, max_file_bytes).await?;
                }
                None => break,
            },
            _ = &mut stop => {
                rx.close();
                while let Some(request) = rx.recv().await {
                    write_one(&mut writer, &request, &mut stats, path, max_file_bytes).await?;
                }
                break;
            }
        }
    }

    writer
        .flush()
        .await
        .map_err(|source| CaptureError::Output {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(stats)
}

/// Bind both OTLP ports up front, before anything else starts.
///
/// The wrapper mode depends on this ordering: the command it runs must never
/// be able to export into a port that is not listening yet.
async fn bind_listeners(
    cfg: &CaptureConfig,
) -> Result<(tokio::net::TcpListener, tokio::net::TcpListener), CaptureError> {
    let grpc_addr = format!("{}:{}", cfg.listen_addr, cfg.port_grpc);
    let http_addr = format!("{}:{}", cfg.listen_addr, cfg.port_http);
    let grpc = tokio::net::TcpListener::bind(&grpc_addr)
        .await
        .map_err(|source| CaptureError::Bind {
            addr: grpc_addr,
            source,
        })?;
    let http = tokio::net::TcpListener::bind(&http_addr)
        .await
        .map_err(|source| CaptureError::Bind {
            addr: http_addr,
            source,
        })?;
    Ok((grpc, http))
}

fn spawn_grpc(
    listener: tokio::net::TcpListener,
    tx: mpsc::Sender<ExportTraceServiceRequest>,
) -> tokio::task::JoinHandle<()> {
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer;
    let service = OtlpGrpcService::new_raw(tx, None);
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(
                TraceServiceServer::new(service).max_decoding_message_size(MAX_PAYLOAD_BYTES),
            )
            .serve_with_incoming(incoming)
            .await
        {
            tracing::error!("capture gRPC server error: {e}");
        }
    })
}

fn spawn_http(
    listener: tokio::net::TcpListener,
    tx: mpsc::Sender<ExportTraceServiceRequest>,
) -> tokio::task::JoinHandle<()> {
    let router = otlp_http_router_with_sink(OtlpSink::Raw(tx), MAX_PAYLOAD_BYTES, None);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("capture HTTP server error: {e}");
        }
    })
}

/// Listen for OTLP until `shutdown` resolves, writing every request received
/// to `cfg.output` as NDJSON.
///
/// # Errors
///
/// [`CaptureError::Bind`] when a port is taken, [`CaptureError::Output`] when
/// the trace file cannot be written, [`CaptureError::Encode`] on a request
/// that fails to serialise.
pub async fn run(
    cfg: &CaptureConfig,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<CaptureStats, CaptureError> {
    let (grpc_listener, http_listener) = bind_listeners(cfg).await?;
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (stop_writer, stop_writer_rx) = tokio::sync::oneshot::channel();

    let output = cfg.output.clone();
    let max_file_bytes = cfg.max_file_bytes;
    let writer =
        tokio::spawn(async move { write_loop(rx, stop_writer_rx, &output, max_file_bytes).await });

    let grpc = spawn_grpc(grpc_listener, tx.clone());
    let http = spawn_http(http_listener, tx.clone());
    tracing::info!(
        "capture listening on {}:{} (gRPC) and {}:{} (HTTP), writing {}",
        cfg.listen_addr,
        cfg.port_grpc,
        cfg.listen_addr,
        cfg.port_http,
        cfg.output.display()
    );

    shutdown.await;

    // Grace window: an exporter flushes its last batch when the application
    // shuts down, which is often the same moment we are asked to stop.
    tokio::time::sleep(cfg.grace).await;

    // Aborting rather than draining connections: past the grace window there
    // is nothing legitimate left in flight, and a CI step must not hang on a
    // client that keeps its connection open.
    grpc.abort();
    http.abort();
    drop(tx);
    // Tell the writer to drain and stop. Dropping senders is not enough, see
    // `write_loop`.
    let _ = stop_writer.send(());

    writer.await.unwrap_or_else(|e| {
        Err(CaptureError::Output {
            path: cfg.output.clone(),
            source: std::io::Error::other(format!("writer task failed: {e}")),
        })
    })
}

/// [`run`] stopping on SIGINT or SIGTERM, the shape a CI job uses when the
/// capture runs alongside its test step.
///
/// # Errors
///
/// Same as [`run`].
pub async fn run_until_signal(cfg: &CaptureConfig) -> Result<CaptureStats, CaptureError> {
    run(cfg, crate::shutdown::shutdown_signal()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::IngestSource;

    /// One SQL CLIENT span, the shape an OTLP exporter puts on the wire.
    const SAMPLE: &str = r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"svc"}}]},"scopeSpans":[{"spans":[{"traceId":"0af7651916cd43dd8448eb211c80319c","spanId":"eee19b7ec3c1b174","name":"db-query","kind":3,"startTimeUnixNano":"1720621921000000000","endTimeUnixNano":"1720621921000500000","attributes":[{"key":"db.statement","value":{"stringValue":"SELECT 1"}},{"key":"db.system","value":{"stringValue":"postgresql"}}]}]}]}]}"#;

    fn sample_request() -> ExportTraceServiceRequest {
        serde_json::from_str(SAMPLE).unwrap()
    }

    /// One request holding `n` identical SQL spans under one trace, the shape
    /// an N+1 loop produces in a test suite.
    fn n_plus_one_request(n: usize) -> ExportTraceServiceRequest {
        const BASE_NS: u64 = 1_720_621_921_000_000_000;
        let spans: Vec<String> = (0..n)
            .map(|i| {
                let start = BASE_NS + i as u64 * 1_000_000;
                let end = start + 500_000;
                format!(
                    r#"{{"traceId":"0af7651916cd43dd8448eb211c80319c","spanId":"eee19b7ec3c1b1{i:02x}","name":"db-query","kind":3,"startTimeUnixNano":"{start}","endTimeUnixNano":"{end}","attributes":[{{"key":"db.statement","value":{{"stringValue":"SELECT * FROM order_item WHERE order_id = {i}"}}}},{{"key":"db.system","value":{{"stringValue":"postgresql"}}}}]}}"#
                )
            })
            .collect();
        let json = format!(
            r#"{{"resourceSpans":[{{"resource":{{"attributes":[{{"key":"service.name","value":{{"stringValue":"svc"}}}}]}},"scopeSpans":[{{"spans":[{}]}}]}}]}}"#,
            spans.join(",")
        );
        serde_json::from_str(&json).unwrap()
    }

    #[tokio::test]
    async fn captured_file_analyzes_into_the_expected_finding() {
        // End to end in one process: what capture writes must let the batch
        // pipeline reach the same verdict the daemon would on the same spans.
        // Comparing the occurrence count, not just the finding type, is what
        // catches a capture that silently loses spans.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.json");
        let (tx, rx) = mpsc::channel(4);
        tx.send(n_plus_one_request(15)).await.unwrap();
        drop(tx);

        let (_stop, stop_rx) = tokio::sync::oneshot::channel();
        let stats = write_loop(rx, stop_rx, &path, u64::MAX).await.unwrap();
        assert_eq!(stats.spans, 15);

        let raw = std::fs::read(&path).unwrap();
        let events = crate::ingest::json::JsonIngest::new(4_194_304)
            .ingest(&raw)
            .unwrap();
        let report = crate::pipeline::analyze(events, &crate::config::Config::default());

        let n_plus_one: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.finding_type == crate::detect::FindingType::NPlusOneSql)
            .collect();
        assert_eq!(n_plus_one.len(), 1, "one N+1 finding expected");
        assert_eq!(n_plus_one[0].pattern.occurrences, 15);
    }

    #[test]
    fn ndjson_line_round_trips_through_analyze() {
        // The whole feature rests on this: what capture writes must produce
        // the same events as converting the received request directly. A
        // codec that is not symmetric (bytes vs hex trace ids, notably)
        // would silently yield a file that analyzes differently.
        let request = sample_request();
        let expected = crate::ingest::otlp::convert_otlp_request(&request);
        assert!(
            !expected.is_empty(),
            "fixture must yield at least one event"
        );

        let line = encode_request(&request).unwrap();
        let events = crate::ingest::json::JsonIngest::new(1_048_576)
            .ingest(&line)
            .unwrap();

        assert_eq!(events, expected);

        // Ids must be hex, not base64: the file is canonical OTLP/JSON that
        // any consumer reads, not a perf-sentinel dialect.
        let text = String::from_utf8(line).unwrap();
        assert!(text.contains(r#""traceId":"0af7651916cd43dd8448eb211c80319c""#));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn encoded_lines_concatenate_into_ndjson() {
        // Two requests, two lines, read back as one stream.
        let request = sample_request();
        let mut file = encode_request(&request).unwrap();
        file.extend_from_slice(&encode_request(&request).unwrap());

        let events = crate::ingest::json::JsonIngest::new(1_048_576)
            .ingest(&file)
            .unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn counts_spans_across_resources_and_scopes() {
        assert_eq!(count_spans(&sample_request()), 1);
        assert_eq!(count_spans(&ExportTraceServiceRequest::default()), 0);
    }

    #[tokio::test]
    async fn write_loop_appends_one_line_per_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.json");
        let (tx, rx) = mpsc::channel(4);
        tx.send(sample_request()).await.unwrap();
        tx.send(sample_request()).await.unwrap();
        drop(tx);

        // Sender kept alive so the loop ends by channel close, not by stop.
        let (_stop, stop_rx) = tokio::sync::oneshot::channel();
        let stats = write_loop(rx, stop_rx, &path, u64::MAX).await.unwrap();
        assert_eq!(stats.requests, 2);
        assert_eq!(stats.spans, 2);
        assert!(!stats.truncated);

        let raw = std::fs::read(&path).unwrap();
        assert_eq!(
            raw.split(|b| *b == b'\n').filter(|l| !l.is_empty()).count(),
            2
        );
        let events = crate::ingest::json::JsonIngest::new(1_048_576)
            .ingest(&raw)
            .unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn write_loop_stops_at_the_size_limit_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.json");
        let one_line = encode_request(&sample_request()).unwrap().len() as u64;

        let (tx, rx) = mpsc::channel(4);
        for _ in 0..3 {
            tx.send(sample_request()).await.unwrap();
        }
        drop(tx);

        // Room for exactly one line.
        let (_stop, stop_rx) = tokio::sync::oneshot::channel();
        let stats = write_loop(rx, stop_rx, &path, one_line).await.unwrap();
        assert!(stats.truncated, "hitting the cap must be reported");
        assert_eq!(stats.requests, 1);

        // What did land stays parseable, a truncated capture is still a
        // usable file rather than a corrupt one.
        let raw = std::fs::read(&path).unwrap();
        let events = crate::ingest::json::JsonIngest::new(1_048_576)
            .ingest(&raw)
            .unwrap();
        assert_eq!(events.len(), 1);
    }

    /// A port nobody is listening on, by binding and releasing one.
    fn free_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    /// Fail loudly instead of hanging a CI run when a step never completes.
    async fn within<T>(label: &str, f: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(10), f)
            .await
            .unwrap_or_else(|_| panic!("capture test step timed out: {label}"))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_captures_from_both_transports() {
        use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.json");
        let cfg = CaptureConfig {
            listen_addr: "127.0.0.1".to_string(),
            port_grpc: free_port(),
            port_http: free_port(),
            output: path.clone(),
            max_file_bytes: u64::MAX,
            grace: Duration::from_millis(50),
        };
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let grpc_url = format!("http://127.0.0.1:{}", cfg.port_grpc);
        let http_url = format!("http://127.0.0.1:{}/v1/traces", cfg.port_http);

        let handle = tokio::spawn(async move {
            run(&cfg, async {
                let _ = stop_rx.await;
            })
            .await
        });
        // The listeners are bound inside run(); poll the gRPC one instead of
        // sleeping a fixed delay, which is what makes this test not flaky.
        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = TraceServiceClient::connect(grpc_url.clone()).await {
                client = Some(c);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut client = client.expect("capture gRPC listener never came up");
        within("grpc export", client.export(sample_request()))
            .await
            .unwrap();

        // Same request over OTLP/HTTP, protobuf-encoded as the spec requires.
        let body = <ExportTraceServiceRequest as prost::Message>::encode_to_vec(&sample_request());
        within("http export", post_protobuf(&http_url, &body)).await;

        stop_tx.send(()).unwrap();
        let stats = within("shutdown", handle).await.unwrap().unwrap();

        assert_eq!(stats.requests, 2, "one gRPC request plus one HTTP request");
        assert_eq!(stats.spans, 2);

        let raw = std::fs::read(&path).unwrap();
        let events = crate::ingest::json::JsonIngest::new(1_048_576)
            .ingest(&raw)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.target == "SELECT 1"));
    }

    /// Minimal hand-rolled OTLP/HTTP POST, matching the project convention of
    /// not pulling an HTTP client into tests.
    async fn post_protobuf(url: &str, body: &[u8]) {
        use tokio::io::AsyncReadExt;
        let rest = url.strip_prefix("http://").unwrap();
        let (authority, path) = rest.split_once('/').unwrap();
        let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
        let head = format!(
            "POST /{path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "OTLP HTTP export rejected: {response}"
        );
    }

    #[tokio::test]
    async fn output_error_surfaces_with_the_path() {
        let (tx, rx) = mpsc::channel::<ExportTraceServiceRequest>(1);
        drop(tx);
        let path = Path::new("/nonexistent-dir-for-capture-test/traces.json");
        let (_stop, stop_rx) = tokio::sync::oneshot::channel();
        let err = write_loop(rx, stop_rx, path, u64::MAX).await.unwrap_err();
        assert!(matches!(err, CaptureError::Output { .. }));
        assert!(err.to_string().contains("traces.json"));
    }
}
