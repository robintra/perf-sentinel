#![warn(clippy::pedantic)]
#![allow(clippy::too_many_lines)] // print_colored_report is long but straightforward
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 for elapsed_ms, f64 -> usize for percentile index
#![allow(clippy::cast_precision_loss)] // usize -> f64 for throughput and latency computation
#![allow(clippy::cast_sign_loss)] // i64 (libc::ru_maxrss) -> usize for RSS bytes on macOS
#![allow(clippy::items_after_statements)] // bench report struct defined near its use

#[cfg(feature = "tui")]
mod tui;

use clap::{Parser, Subcommand};
use sentinel_core::config::Config;
use sentinel_core::detect::Severity;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;
use sentinel_core::report::json::JsonReportSink;
use sentinel_core::report::{Report, ReportSink};
use std::io::Read;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "perf-sentinel")]
#[command(about = "Lightweight polyglot performance anti-pattern detector")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Output format for the explain command.
#[derive(Clone, Copy, clap::ValueEnum)]
enum ExplainFormat {
    /// Colored terminal tree view (default).
    Text,
    /// Structured JSON tree.
    Json,
}

/// Output format for the analyze command.
#[derive(Clone, Copy, clap::ValueEnum)]
enum OutputFormat {
    /// Colored terminal report (default for interactive use).
    Text,
    /// Structured JSON report.
    Json,
    /// SARIF v2.1.0 for GitHub/GitLab code scanning.
    Sarif,
}

/// Output format for the pg-stat command.
#[derive(Clone, Copy, clap::ValueEnum)]
enum PgStatOutputFormat {
    /// Colored terminal table (default).
    Text,
    /// Structured JSON report.
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze trace files in batch mode. Reads from stdin if no --input is given.
    Analyze {
        /// Path to a JSON trace file to analyze. If omitted, reads from stdin.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Enable CI quality gate mode (exit 1 if gate fails, JSON output).
        #[arg(long)]
        ci: bool,
        /// Output format: text (colored, default), json, sarif.
        #[arg(long, value_enum)]
        format: Option<OutputFormat>,
    },

    /// Watch for traces in real-time (daemon mode).
    #[cfg(feature = "daemon")]
    Watch {
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Run analysis on an embedded demo dataset.
    Demo {
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Explain a specific trace: tree view with findings annotated inline.
    /// Span-anchored detections (N+1, redundant, slow, fanout) land on
    /// their offending spans; trace-level detections (chatty service,
    /// pool saturation, serialized calls) are rendered in a dedicated
    /// header section above the span tree. Cross-trace percentile
    /// findings from `analyze` are not included.
    Explain {
        /// Path to a JSON trace file.
        #[arg(short, long)]
        input: PathBuf,
        /// Trace ID to explain.
        #[arg(long)]
        trace_id: String,
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: ExplainFormat,
    },

    /// Benchmark perf-sentinel on a trace file.
    Bench {
        /// Path to a JSON trace file. Reads from stdin if omitted.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// Number of iterations (default 10).
        #[arg(long, default_value = "10")]
        iterations: u32,
    },

    /// Interactive TUI to inspect traces and findings.
    #[cfg(feature = "tui")]
    Inspect {
        /// Path to a JSON trace file.
        #[arg(short, long)]
        input: PathBuf,
        /// Path to a `.perf-sentinel.toml` config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Query Grafana Tempo for traces and analyze them.
    #[cfg(feature = "tempo")]
    Tempo {
        /// Tempo HTTP API endpoint (e.g. `http://localhost:3200`).
        #[arg(long)]
        endpoint: String,
        /// Fetch a single trace by ID.
        #[arg(long)]
        trace_id: Option<String>,
        /// Search traces by service name.
        #[arg(long)]
        service: Option<String>,
        /// Lookback window for search (e.g. `1h`, `30m`, `24h`).
        #[arg(long, default_value = "1h")]
        lookback: String,
        /// Maximum number of traces to fetch.
        #[arg(long, default_value = "100")]
        max_traces: usize,
        /// Path to a `.perf-sentinel.toml` config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format: text (colored, default), json, sarif.
        #[arg(long, value_enum)]
        format: Option<OutputFormat>,
        /// Enable CI quality gate mode (exit 1 if gate fails, JSON output).
        #[arg(long)]
        ci: bool,
    },

    /// Calibrate energy coefficients from real measurements.
    Calibrate {
        /// Path to a JSON trace file (same format as analyze input).
        #[arg(long)]
        traces: PathBuf,
        /// Path to a CSV file with energy measurements (`power_watts` or `energy_kwh` format).
        #[arg(long)]
        measured_energy: PathBuf,
        /// Output path for the calibration TOML file.
        #[arg(long, default_value = ".perf-sentinel-calibration.toml")]
        output: PathBuf,
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Analyze `pg_stat_statements` data for SQL hotspot detection.
    PgStat {
        /// Path to `pg_stat_statements` CSV or JSON export.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// Prometheus endpoint to scrape `pg_stat_statements` metrics from.
        #[cfg(feature = "daemon")]
        #[arg(long)]
        prometheus: Option<String>,
        /// Number of top queries per ranking (default 10).
        #[arg(long, default_value = "10")]
        top_n: usize,
        /// Optional: path to a trace file for cross-referencing with trace findings.
        #[arg(long)]
        traces: Option<PathBuf>,
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: PgStatOutputFormat,
    },

    /// Query a running perf-sentinel daemon for findings and status.
    #[cfg(feature = "daemon")]
    Query {
        /// Daemon HTTP endpoint.
        #[arg(long, default_value = "http://localhost:4318")]
        daemon: String,
        #[command(subcommand)]
        action: QueryAction,
    },
}

/// Output format for query sub-actions.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, clap::ValueEnum)]
enum QueryOutputFormat {
    /// Colored terminal output (default).
    Text,
    /// Structured JSON.
    Json,
}

/// Sub-actions for the `query` subcommand.
#[cfg(feature = "daemon")]
#[derive(Subcommand)]
enum QueryAction {
    /// List recent findings from the daemon.
    Findings {
        /// Filter by service name.
        #[arg(long)]
        service: Option<String>,
        /// Filter by finding type (e.g. `n_plus_one_sql`).
        #[arg(long, value_name = "TYPE")]
        finding_type: Option<String>,
        /// Filter by severity (critical, warning, info).
        #[arg(long)]
        severity: Option<String>,
        /// Maximum number of results (default 50).
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
    /// Show the explain tree for a trace from daemon memory.
    Explain {
        /// Trace ID to explain.
        #[arg(long)]
        trace_id: String,
        /// Output format: text (colored tree, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
    /// Interactive TUI with live daemon data.
    #[cfg(feature = "tui")]
    Inspect,
    /// Show active cross-trace correlations.
    Correlations {
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
    /// Show daemon status (uptime, traces, findings count).
    Status {
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Analyze {
            input,
            config,
            ci,
            format,
        } => {
            cmd_analyze(input.as_deref(), config.as_deref(), ci, format);
        }
        Commands::Explain {
            input,
            trace_id,
            config,
            format,
        } => {
            cmd_explain(&input, &trace_id, config.as_deref(), format);
        }
        #[cfg(feature = "daemon")]
        Commands::Watch { config } => cmd_watch(config.as_deref()).await,
        Commands::Demo { config } => cmd_demo(config.as_deref()),
        Commands::Bench { input, iterations } => cmd_bench(input.as_deref(), iterations),
        #[cfg(feature = "tui")]
        Commands::Inspect { input, config } => cmd_inspect(&input, config.as_deref()),
        #[cfg(feature = "tempo")]
        Commands::Tempo {
            endpoint,
            trace_id,
            service,
            lookback,
            max_traces,
            config,
            format,
            ci,
        } => {
            cmd_tempo(
                &endpoint,
                trace_id.as_deref(),
                service.as_deref(),
                &lookback,
                max_traces,
                config.as_deref(),
                format,
                ci,
            )
            .await;
        }
        Commands::Calibrate {
            traces,
            measured_energy,
            output,
            config,
        } => cmd_calibrate(&traces, &measured_energy, &output, config.as_deref()),
        Commands::PgStat {
            input,
            #[cfg(feature = "daemon")]
            prometheus,
            top_n,
            traces,
            config,
            format,
        } => {
            #[cfg(feature = "daemon")]
            if let Some(ref prom_endpoint) = prometheus {
                // `main` is already async (`#[tokio::main]`), so `.await`
                // the fetch directly. Creating a nested `Runtime::new()`
                // here would panic at runtime with "Cannot start a runtime
                // from within a runtime."
                let entries =
                    sentinel_core::ingest::pg_stat::fetch_from_prometheus(prom_endpoint, top_n)
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("Prometheus fetch failed: {e}");
                            std::process::exit(1);
                        });
                cmd_pg_stat_from_entries(
                    entries,
                    top_n,
                    traces.as_deref(),
                    config.as_deref(),
                    format,
                );
            } else if let Some(ref path) = input {
                cmd_pg_stat(path, top_n, traces.as_deref(), config.as_deref(), format);
            } else {
                eprintln!("Either --input or --prometheus is required");
                std::process::exit(1);
            }
            #[cfg(not(feature = "daemon"))]
            if let Some(ref path) = input {
                cmd_pg_stat(path, top_n, traces.as_deref(), config.as_deref(), format);
            } else {
                eprintln!("--input is required");
                std::process::exit(1);
            }
        }
        #[cfg(feature = "daemon")]
        Commands::Query { daemon, action } => {
            // `main` is `#[tokio::main]`, so await the async command
            // directly. A nested `Runtime::new().block_on(...)` here
            // panics with "Cannot start a runtime from within a runtime."
            cmd_query(&daemon, action).await;
        }
    }
}

fn load_config(path: Option<&std::path::Path>) -> Config {
    let config_path = path.map_or_else(
        || PathBuf::from(".perf-sentinel.toml"),
        std::path::Path::to_path_buf,
    );

    match std::fs::read_to_string(&config_path) {
        Ok(content) => match sentinel_core::config::load_from_str(&content) {
            Ok(config) => return config,
            Err(e) => {
                if path.is_some() {
                    eprintln!("Error parsing config {}: {e}", config_path.display());
                    std::process::exit(1);
                }
                eprintln!("Warning: failed to parse {}: {e}", config_path.display());
            }
        },
        Err(e) => {
            if path.is_some() {
                eprintln!("Error reading config {}: {e}", config_path.display());
                std::process::exit(1);
            }
            // .perf-sentinel.toml not found in cwd, use defaults silently
        }
    }
    Config::default()
}

#[allow(clippy::option_if_let_else)] // if/else with process::exit is clearer than map_or_else
fn read_events(input: Option<&std::path::Path>, max_size: usize) -> Vec<u8> {
    if let Some(path) = input {
        info!("Reading trace file: {}", path.display());
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > max_size as u64 => {
                eprintln!(
                    "Error: file {} is {} bytes, exceeds maximum of {max_size} bytes",
                    path.display(),
                    meta.len()
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error reading {}: {e}", path.display());
                std::process::exit(1);
            }
            _ => {}
        }
        match std::fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("Error reading {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    } else {
        info!("Reading traces from stdin");
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin()
            .take(max_size as u64 + 1)
            .read_to_end(&mut buf)
        {
            eprintln!("Error reading stdin: {e}");
            std::process::exit(1);
        }
        if buf.len() > max_size {
            eprintln!("Error: stdin payload exceeds maximum of {max_size} bytes");
            std::process::exit(1);
        }
        buf
    }
}

/// Parse raw bytes as JSON trace events, printing a clear error and
/// exiting with code 1 on failure. Shared across all CLI subcommands
/// that ingest trace files.
fn ingest_json_or_exit(raw: &[u8], max_size: usize) -> Vec<sentinel_core::event::SpanEvent> {
    let ingest = JsonIngest::new(max_size);
    match ingest.ingest(raw) {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error ingesting events: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_analyze(
    input: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    ci: bool,
    format: Option<OutputFormat>,
) {
    let config = load_config(config_path);
    let raw = read_events(input, config.max_payload_size);

    let events = ingest_json_or_exit(&raw, config.max_payload_size);

    let report = pipeline::analyze(events, &config);
    emit_report_and_gate(&report, format, ci, "report");
}

#[cfg(feature = "tempo")]
#[allow(clippy::too_many_arguments)]
async fn cmd_tempo(
    endpoint: &str,
    trace_id: Option<&str>,
    service: Option<&str>,
    lookback: &str,
    max_traces: usize,
    config_path: Option<&std::path::Path>,
    format: Option<OutputFormat>,
    ci: bool,
) {
    if trace_id.is_none() && service.is_none() {
        eprintln!("Error: either --trace-id or --service is required");
        std::process::exit(1);
    }
    if trace_id.is_some() && service.is_some() {
        eprintln!("Error: --trace-id and --service are mutually exclusive");
        std::process::exit(1);
    }

    let lookback_duration = match sentinel_core::ingest::tempo::parse_lookback(lookback) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error parsing lookback: {e}");
            std::process::exit(1);
        }
    };

    let config = load_config(config_path);

    let events = match sentinel_core::ingest::tempo::ingest_from_tempo(
        endpoint,
        service,
        trace_id,
        lookback_duration,
        max_traces,
    )
    .await
    {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error fetching traces from Tempo: {e}");
            std::process::exit(1);
        }
    };

    info!(
        events = events.len(),
        "Ingested events from Tempo, running analysis"
    );

    let report = pipeline::analyze(events, &config);
    emit_report_and_gate(&report, format, ci, "tempo");
}

fn cmd_calibrate(
    traces_path: &std::path::Path,
    energy_path: &std::path::Path,
    output_path: &std::path::Path,
    config_path: Option<&std::path::Path>,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(traces_path), config.max_payload_size);

    let events = ingest_json_or_exit(&raw, config.max_payload_size);

    // Cap the energy CSV size the same way `read_events` caps trace files.
    // A 10 GB CSV passed as `--measured-energy` would otherwise load entirely
    // into RAM (DoS). 64 MiB is generous enough for thousands of RAPL samples
    // per minute while bounding the worst case.
    const MAX_ENERGY_CSV_BYTES: u64 = 64 * 1024 * 1024;
    match std::fs::metadata(energy_path) {
        Ok(meta) if meta.len() > MAX_ENERGY_CSV_BYTES => {
            eprintln!(
                "Error: energy CSV {} is {} bytes, exceeds maximum of {} bytes",
                energy_path.display(),
                meta.len(),
                MAX_ENERGY_CSV_BYTES
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error reading {}: {e}", energy_path.display());
            std::process::exit(1);
        }
        _ => {}
    }
    let energy_content = match std::fs::read_to_string(energy_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading energy CSV {}: {e}", energy_path.display());
            std::process::exit(1);
        }
    };

    let readings = match sentinel_core::calibrate::parse_energy_csv(&energy_content) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error parsing energy CSV: {e}");
            std::process::exit(1);
        }
    };

    let results = match sentinel_core::calibrate::calibrate(&events, &readings) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error during calibration: {e}");
            std::process::exit(1);
        }
    };

    // Print warnings for extreme factors
    for warning in sentinel_core::calibrate::validate_results(&results) {
        eprintln!("Warning: {warning}");
    }

    // Print human-readable summary
    let window_secs = {
        let min_ts = readings.iter().map(|r| r.timestamp_ms).min().unwrap_or(0);
        let max_ts = readings.iter().map(|r| r.timestamp_ms).max().unwrap_or(0);
        max_ts.saturating_sub(min_ts) as f64 / 1000.0
    };
    let window_label = if window_secs >= 3600.0 {
        format!("{:.0}h", window_secs / 3600.0)
    } else if window_secs >= 60.0 {
        format!("{:.0}min", window_secs / 60.0)
    } else {
        format!("{window_secs:.0}s")
    };
    eprintln!(
        "\nCalibration results ({} services, {} window):",
        results.len(),
        window_label
    );
    for r in &results {
        let per_op_uwh = r.energy_per_op_kwh * 1e9; // kWh to µWh
        let default_uwh = r.default_energy_per_op_kwh * 1e9;
        eprintln!(
            "  {}: {:.1}x default (measured {:.2} \u{00b5}Wh/op vs default {:.2} \u{00b5}Wh/op)",
            r.service, r.factor, per_op_uwh, default_uwh
        );
    }

    // Write calibration TOML
    let toml_content = sentinel_core::calibrate::write_calibration_toml(
        &results,
        &traces_path.display().to_string(),
        &energy_path.display().to_string(),
    );
    match std::fs::write(output_path, &toml_content) {
        Ok(()) => {
            eprintln!("\nWritten to {}", output_path.display());
        }
        Err(e) => {
            eprintln!("Error writing {}: {e}", output_path.display());
            std::process::exit(1);
        }
    }
}

fn cmd_explain(
    input: &std::path::Path,
    trace_id: &str,
    config_path: Option<&std::path::Path>,
    format: ExplainFormat,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), config.max_payload_size);

    let events = ingest_json_or_exit(&raw, config.max_payload_size);

    let normalized = sentinel_core::normalize::normalize_all(events);
    let traces = sentinel_core::correlate::correlate(normalized);

    let Some(trace) = traces.iter().find(|t| t.trace_id == trace_id) else {
        eprintln!("Error: trace ID '{trace_id}' not found");
        let total = traces.len();
        let ids: Vec<&str> = traces
            .iter()
            .take(20)
            .map(|t| t.trace_id.as_str())
            .collect();
        if total > 20 {
            eprintln!(
                "Available trace IDs: {} ... and {} more",
                ids.join(", "),
                total - 20
            );
        } else {
            eprintln!("Available trace IDs: {}", ids.join(", "));
        }
        std::process::exit(1);
    };

    let detect_config = sentinel_core::detect::DetectConfig::from(&config);
    let findings = sentinel_core::detect::detect(std::slice::from_ref(trace), &detect_config);

    let tree = sentinel_core::explain::build_tree(trace, &findings);

    match format {
        ExplainFormat::Text => {
            use std::io::IsTerminal;
            let use_color = std::io::stdout().is_terminal();
            print!(
                "{}",
                sentinel_core::explain::format_tree_text(&tree, use_color)
            );
        }
        ExplainFormat::Json => match sentinel_core::explain::format_tree_json(&tree) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("Error serializing explain tree: {e}");
                std::process::exit(1);
            }
        },
    }
}

#[cfg(feature = "daemon")]
async fn cmd_watch(config_path: Option<&std::path::Path>) {
    let config = load_config(config_path);
    info!(
        "Starting daemon: gRPC={}:{}, HTTP={}:{}",
        config.listen_addr, config.listen_port_grpc, config.listen_addr, config.listen_port,
    );
    if let Err(e) = sentinel_core::daemon::run(config).await {
        eprintln!("Daemon error: {e}");
        std::process::exit(1);
    }
}

fn cmd_demo(config_path: Option<&std::path::Path>) {
    const DEMO_DATA: &str = include_str!("demo_data.json");

    let mut config = load_config(config_path);
    // Default to eu-west-3 for demo CO2 display if no region configured
    if config.green_default_region.is_none() {
        config.green_default_region = Some("eu-west-3".to_string());
    }
    let events = ingest_json_or_exit(DEMO_DATA.as_bytes(), config.max_payload_size);

    let report = pipeline::analyze(events, &config);
    print_colored_report(&report, "demo");
}

fn cmd_bench(input: Option<&std::path::Path>, iterations: u32) {
    if iterations == 0 {
        eprintln!("Error: iterations must be >= 1");
        std::process::exit(1);
    }

    let config = Config::default();
    let raw = read_events(input, config.max_payload_size);

    let events = ingest_json_or_exit(&raw, config.max_payload_size);

    let event_count = events.len();
    if event_count == 0 {
        eprintln!("Error: no events to benchmark");
        std::process::exit(1);
    }

    // Pre-clone all batches so clone cost is excluded from timing
    let batches: Vec<Vec<sentinel_core::event::SpanEvent>> =
        (0..iterations).map(|_| events.clone()).collect();

    let mut durations_ns: Vec<u64> = Vec::with_capacity(iterations as usize);
    let mut rss_peak: Option<usize> = None;

    for batch in batches {
        let start = std::time::Instant::now();
        let _ = pipeline::analyze(batch, &config);
        let elapsed = start.elapsed();
        durations_ns.push(elapsed.as_nanos() as u64);

        if let Some(rss) = current_rss_bytes() {
            rss_peak = Some(rss_peak.map_or(rss, |prev: usize| prev.max(rss)));
        }
    }

    let (p50_us, p99_us) = compute_latency_percentiles(&durations_ns, event_count);
    let (throughput, total_elapsed_ms) = compute_throughput(&durations_ns, event_count, iterations);

    #[derive(serde::Serialize)]
    struct BenchReport {
        iterations: u32,
        events_per_iteration: usize,
        throughput_events_per_sec: f64,
        latency_per_event_us: LatencyPercentiles,
        rss_peak_bytes: Option<usize>,
        total_elapsed_ms: u64,
    }

    #[derive(serde::Serialize)]
    struct LatencyPercentiles {
        p50: f64,
        p99: f64,
    }

    let report = BenchReport {
        iterations,
        events_per_iteration: event_count,
        throughput_events_per_sec: throughput,
        latency_per_event_us: LatencyPercentiles {
            p50: p50_us,
            p99: p99_us,
        },
        rss_peak_bytes: rss_peak,
        total_elapsed_ms,
    };

    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("Error serializing bench report: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "tui")]
fn cmd_inspect(input: &std::path::Path, config_path: Option<&std::path::Path>) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), config.max_payload_size);

    let events = ingest_json_or_exit(&raw, config.max_payload_size);

    let detect_config = sentinel_core::detect::DetectConfig::from(&config);

    let (report, traces) = pipeline::analyze_with_traces(events, &config);

    let mut app = tui::App::new(report.findings, traces, detect_config);
    if let Err(e) = tui::run(&mut app) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}

fn cmd_pg_stat(
    input: &std::path::Path,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    format: PgStatOutputFormat,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), config.max_payload_size);

    let entries = match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw, config.max_payload_size)
    {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Error parsing pg_stat_statements: {e}");
            std::process::exit(1);
        }
    };

    run_pg_stat_pipeline(entries, top_n, traces, &config, format);
}

/// Variant of `cmd_pg_stat` that takes already-parsed entries (from Prometheus scrape).
#[cfg(feature = "daemon")]
fn cmd_pg_stat_from_entries(
    entries: Vec<sentinel_core::ingest::pg_stat::PgStatEntry>,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    format: PgStatOutputFormat,
) {
    let config = load_config(config_path);
    run_pg_stat_pipeline(entries, top_n, traces, &config, format);
}

/// Shared pipeline for the two `pg-stat` entry points (file input and
/// Prometheus scrape): optional trace cross-reference, ranking, then
/// text or JSON output. Extracted to avoid duplicating the 20+ lines
/// between `cmd_pg_stat` and `cmd_pg_stat_from_entries`.
fn run_pg_stat_pipeline(
    mut entries: Vec<sentinel_core::ingest::pg_stat::PgStatEntry>,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config: &Config,
    format: PgStatOutputFormat,
) {
    use sentinel_core::ingest::pg_stat;

    // Cross-reference with trace findings if --traces is provided.
    if let Some(traces_path) = traces {
        let traces_raw = read_events(Some(traces_path), config.max_payload_size);
        let ingest = JsonIngest::new(config.max_payload_size);
        match ingest.ingest(&traces_raw) {
            Ok(events) => {
                let report = pipeline::analyze(events, config);
                pg_stat::cross_reference(&mut entries, &report.findings);
            }
            Err(e) => {
                eprintln!("Warning: failed to ingest trace file for cross-reference: {e}");
            }
        }
    }

    let report = pg_stat::rank_pg_stat(&entries, top_n);

    match format {
        PgStatOutputFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("Error serializing pg_stat report: {e}");
                std::process::exit(1);
            }
        },
        PgStatOutputFormat::Text => print_pg_stat_report(&report),
    }
}

fn print_pg_stat_report(report: &sentinel_core::ingest::pg_stat::PgStatReport) {
    use std::io::IsTerminal;

    let is_tty = std::io::stdout().is_terminal();
    let (bold, cyan, yellow, dim, reset) = if is_tty {
        ("\x1b[1m", "\x1b[36m", "\x1b[33m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };

    println!();
    println!("{bold}{cyan}=== pg_stat_statements analysis ==={reset}");
    println!("{dim}Total entries: {}{reset}", report.total_entries);
    println!();

    for ranking in &report.rankings {
        println!("{bold}{cyan}--- {} ---{reset}", ranking.label);
        println!();
        for (i, entry) in ranking.entries.iter().enumerate() {
            let trace_marker = if entry.seen_in_traces {
                format!(" {yellow}[seen in traces]{reset}")
            } else {
                String::new()
            };
            println!(
                "  {bold}#{}{reset} {}{trace_marker}",
                i + 1,
                entry.normalized_template
            );
            println!(
                "    {dim}calls:{reset} {}  {dim}total:{reset} {:.2}ms  {dim}mean:{reset} {:.2}ms  {dim}rows:{reset} {}",
                entry.calls, entry.total_exec_time_ms, entry.mean_exec_time_ms, entry.rows
            );
            println!(
                "    {dim}blks_hit:{reset} {}  {dim}blks_read:{reset} {}",
                entry.shared_blks_hit, entry.shared_blks_read
            );
            println!();
        }
    }
}

/// Get current RSS (Resident Set Size) in bytes. Best-effort, platform-specific.
#[allow(clippy::missing_const_for_fn)] // not const on Linux (reads /proc)
fn compute_latency_percentiles(durations_ns: &[u64], event_count: usize) -> (f64, f64) {
    let mut per_event_ns: Vec<f64> = durations_ns
        .iter()
        .map(|&d| d as f64 / event_count as f64)
        .collect();
    per_event_ns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let len = per_event_ns.len();
    let p50_idx = ((len as f64 * 0.50).ceil() as usize).saturating_sub(1);
    let p99_idx = ((len as f64 * 0.99).ceil() as usize).min(len.saturating_sub(1));
    (
        per_event_ns[p50_idx] / 1000.0,
        per_event_ns[p99_idx] / 1000.0,
    )
}

fn compute_throughput(durations_ns: &[u64], event_count: usize, iterations: u32) -> (f64, u64) {
    let elapsed_nanos: u64 = durations_ns.iter().sum();
    let total_elapsed_ms: u64 = elapsed_nanos / 1_000_000;
    let total_events = event_count as f64 * f64::from(iterations);
    let total_seconds = elapsed_nanos as f64 / 1_000_000_000.0;
    let throughput = if total_seconds > 0.0 {
        total_events / total_seconds
    } else {
        0.0
    };
    (throughput, total_elapsed_ms)
}

fn current_rss_bytes() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| {
                    l.split_whitespace()
                        .nth(1)?
                        .parse::<usize>()
                        .ok()
                        .map(|kb| kb * 1024)
                })
            })
    }
    #[cfg(target_os = "windows")]
    {
        // Windows: use GetProcessMemoryInfo via kernel32
        // Best-effort, returns None if unavailable
        None
    }
    #[cfg(target_os = "macos")]
    {
        use std::mem;
        // SAFETY: libc::rusage is a C struct of numeric fields, zeroing it is valid initialization.
        let mut usage: libc::rusage = unsafe { mem::zeroed() };
        // SAFETY: getrusage is a POSIX syscall that writes into the provided rusage pointer.
        // The pointer is valid (stack-allocated) and the return value is checked below.
        let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, std::ptr::addr_of_mut!(usage)) };
        if ret == 0 {
            // On macOS, ru_maxrss is in bytes
            Some(usage.ru_maxrss as usize)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Emit a report in the requested format and exit 1 if the quality gate
/// fails in CI mode. Shared between `cmd_analyze` and `cmd_tempo`.
fn emit_report_and_gate(report: &Report, format: Option<OutputFormat>, ci: bool, label: &str) {
    let effective_format = format.unwrap_or(if ci {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    });

    match effective_format {
        OutputFormat::Text => {
            print_colored_report(report, label);
        }
        OutputFormat::Json => {
            let sink = JsonReportSink;
            if let Err(e) = sink.emit(report) {
                eprintln!("Error writing report: {e}");
                std::process::exit(1);
            }
        }
        OutputFormat::Sarif => {
            if let Err(e) = sentinel_core::report::sarif::emit_sarif(report) {
                eprintln!("Error writing SARIF report: {e}");
                std::process::exit(1);
            }
        }
    }

    if ci && !report.quality_gate.passed {
        eprintln!("Quality gate FAILED");
        std::process::exit(1);
    }
}

fn print_colored_report(report: &Report, title: &str) {
    format_colored_report(report, title, false);
}

/// ANSI color codes bundled for CLI rendering.
///
/// Named fields avoid the "count underscores in a 7-tuple" pattern that
/// made it easy to misuse the old `AnsiColors` tuple alias. Every field
/// is either a SGR escape sequence or an empty string (when the output
/// is not a terminal).
#[derive(Clone, Copy)]
struct AnsiColors {
    bold: &'static str,
    cyan: &'static str,
    red: &'static str,
    yellow: &'static str,
    green: &'static str,
    dim: &'static str,
    reset: &'static str,
}

fn ansi_colors(force_color: bool) -> AnsiColors {
    use std::io::IsTerminal;
    if force_color || std::io::stdout().is_terminal() {
        AnsiColors {
            bold: "\x1b[1m",
            cyan: "\x1b[36m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            green: "\x1b[32m",
            dim: "\x1b[2m",
            reset: "\x1b[0m",
        }
    } else {
        AnsiColors {
            bold: "",
            cyan: "",
            red: "",
            yellow: "",
            green: "",
            dim: "",
            reset: "",
        }
    }
}

/// Map an [`InterpretationLevel`] to the ANSI color used for CLI
/// rendering. Mirrors the palette used for finding severities:
/// Critical=red, High=yellow, Healthy=green. Moderate returns an empty
/// string (uncolored) to keep it informational without visually competing
/// with High.
///
/// [`InterpretationLevel`]: sentinel_core::InterpretationLevel
fn interpret_color(level: sentinel_core::InterpretationLevel, colors: AnsiColors) -> &'static str {
    use sentinel_core::InterpretationLevel::{Critical, Healthy, High, Moderate};
    match level {
        Critical => colors.red,
        High => colors.yellow,
        Moderate => "",
        Healthy => colors.green,
    }
}

fn format_colored_report(report: &Report, title: &str, force_color: bool) {
    let colors = ansi_colors(force_color);
    let AnsiColors {
        bold,
        cyan,
        green,
        dim,
        reset,
        ..
    } = colors;

    println!();
    println!("{bold}{cyan}=== perf-sentinel {title} ==={reset}");
    println!(
        "{dim}Analyzed {} events across {} traces in {}ms{reset}",
        report.analysis.events_processed,
        report.analysis.traces_analyzed,
        report.analysis.duration_ms
    );
    println!();

    if report.findings.is_empty() {
        println!("{green}No performance anti-patterns detected.{reset}");
    } else {
        print_findings(&report.findings, force_color);
    }

    print_green_summary(&report.green_summary, force_color);
    print_quality_gate(&report.quality_gate, force_color);
}

fn print_findings(findings: &[sentinel_core::detect::Finding], force_color: bool) {
    let AnsiColors {
        bold,
        cyan,
        dim,
        reset,
        ..
    } = ansi_colors(force_color);

    println!("{bold}Found {} issue(s):{reset}", findings.len());
    println!();

    let colors = ansi_colors(force_color);
    for (i, finding) in findings.iter().enumerate() {
        let severity_color = match finding.severity {
            Severity::Critical => colors.red,
            Severity::Warning => colors.yellow,
            Severity::Info => dim,
        };

        let type_label = finding.finding_type.display_label();

        let severity_label = match finding.severity {
            Severity::Critical => "CRITICAL",
            Severity::Warning => "WARNING",
            Severity::Info => "INFO",
        };

        println!(
            "  {bold}{severity_color}[{severity_label}] #{} {type_label}{reset}",
            i + 1,
        );
        println!("    {dim}Trace:{reset}    {}", finding.trace_id);
        println!("    {dim}Service:{reset}  {}", finding.service);
        println!("    {dim}Endpoint:{reset} {}", finding.source_endpoint);
        if let Some(ref loc) = finding.code_location {
            let mut src = String::new();
            if let Some(ref ns) = loc.namespace {
                src.push_str(ns);
                src.push('.');
            }
            if let Some(ref func) = loc.function {
                src.push_str(func);
            }
            let has_name = !src.is_empty();
            if let Some(ref fp) = loc.filepath {
                if has_name {
                    src.push_str(" (");
                }
                src.push_str(fp);
                if let Some(ln) = loc.lineno {
                    src.push(':');
                    src.push_str(&ln.to_string());
                }
                if has_name {
                    src.push(')');
                }
            }
            if !src.is_empty() {
                println!("    {dim}Source:{reset}   {src}");
            }
        }
        println!("    {dim}Template:{reset} {}", finding.pattern.template);
        println!(
            "    {dim}Hits:{reset}     {} occurrences, {} distinct params, {}ms window",
            finding.pattern.occurrences, finding.pattern.distinct_params, finding.pattern.window_ms
        );
        println!(
            "    {dim}Window:{reset}   {} -> {}",
            finding.first_timestamp, finding.last_timestamp
        );
        println!("    {cyan}Suggestion:{reset} {}", finding.suggestion);
        if let Some(ref impact) = finding.green_impact {
            println!(
                "    {dim}Extra I/O:{reset} {} avoidable ops",
                impact.estimated_extra_io_ops
            );
            // Read the pre-computed band from the struct field rather than
            // calling for_iis() again: keeps the CLI rendering in lockstep
            // with the JSON output (both reflect the same `score_green`
            // classification) and prevents silent drift if the thresholds
            // change in one place but not the other.
            let level = impact.io_intensity_band;
            let level_color = interpret_color(level, colors);
            println!(
                "    {dim}IIS:{reset}      {:.1} {level_color}({}){reset}",
                impact.io_intensity_score,
                level.short_label(),
            );
        }
        println!();
    }
}

fn print_green_summary(summary: &sentinel_core::report::GreenSummary, force_color: bool) {
    let colors = ansi_colors(force_color);
    let AnsiColors {
        bold,
        cyan,
        dim,
        reset,
        ..
    } = colors;

    println!("{bold}{cyan}--- GreenOps Summary ---{reset}");
    println!("  Total I/O ops:     {}", summary.total_io_ops);
    println!("  Avoidable I/O ops: {}", summary.avoidable_io_ops);
    // Read the pre-computed band from the struct field (see print_findings).
    let waste_level = summary.io_waste_ratio_band;
    let waste_color = interpret_color(waste_level, colors);
    println!(
        "  I/O waste ratio:   {:.1}% {waste_color}({}){reset}",
        summary.io_waste_ratio * 100.0,
        waste_level.short_label(),
    );

    // Render the structured CO₂ report when present.
    if let Some(carbon) = summary.co2.as_ref() {
        println!(
            "  Est. CO\u{2082}:          {:.6} g (low {:.6}, high {:.6}, model {})",
            carbon.total.mid, carbon.total.low, carbon.total.high, carbon.total.model,
        );
        println!(
            "  Avoidable CO\u{2082}:     {:.6} g (low {:.6}, high {:.6})",
            carbon.avoidable.mid, carbon.avoidable.low, carbon.avoidable.high,
        );
        println!(
            "  Operational:       {:.6} g    Embodied: {:.6} g    Methodology: {}",
            carbon.operational_gco2, carbon.embodied_gco2, carbon.total.methodology,
        );
        if let Some(transport) = carbon.transport_gco2 {
            println!("  Transport:         {transport:.6} g    (cross-region network bytes)");
        }
    }

    // Per-region breakdown when more than one region was resolved.
    if summary.regions.len() > 1 {
        println!();
        println!("  {bold}Per-region breakdown:{reset}");
        for region in &summary.regions {
            println!(
                "    - {}: {} I/O ops, {:.6} gCO\u{2082}",
                region.region, region.io_ops, region.co2_gco2,
            );
        }
    }

    if !summary.top_offenders.is_empty() {
        println!();
        println!("  {bold}Top offenders:{reset}");
        for offender in &summary.top_offenders {
            // Read the pre-computed band from the struct field (see print_findings).
            let level = offender.io_intensity_band;
            let level_color = interpret_color(level, colors);
            let co2_str = offender
                .co2_grams
                .map_or(String::new(), |co2| format!(", {co2:.6} gCO\u{2082}"));
            println!(
                "    - {}: IIS {:.1} {level_color}({}){reset} (service: {}){co2_str}",
                offender.endpoint,
                offender.io_intensity_score,
                level.short_label(),
                offender.service,
            );
        }
    }

    // Mandatory disclaimer: only shown when we actually emitted CO₂
    // estimates, to avoid noise when green scoring is disabled.
    // The "2× multiplicative uncertainty" framing matches the constants:
    // low = mid/2, high = mid×2 (log-symmetric interval, geometric mean = mid).
    if summary.co2.is_some() {
        println!();
        println!(
            "  {dim}Note: CO\u{2082} estimates have ~2\u{00d7} multiplicative uncertainty \
             (low = mid/2, high = mid\u{00d7}2). See docs/LIMITATIONS.md.{reset}"
        );
    }

    // One-liner on the interpret bands: they are anchored on the
    // *default* detector thresholds, not on the user's config. An
    // endpoint still labelled "high" after raising `n_plus_one_threshold`
    // in .perf-sentinel.toml is not a bug; see README "How to read the
    // report" for the full explanation.
    println!(
        "  {dim}Note: `(healthy/moderate/high/critical)` bands use fixed heuristic \
         thresholds, independent of your `n_plus_one_threshold` / \
         `io_waste_ratio_max` overrides. See README \"How to read the report\".{reset}"
    );

    println!();
}

fn print_quality_gate(gate: &sentinel_core::report::QualityGate, force_color: bool) {
    let AnsiColors {
        bold,
        red,
        green,
        reset,
        ..
    } = ansi_colors(force_color);

    let gate_color = if gate.passed { green } else { red };
    let gate_label = if gate.passed { "PASSED" } else { "FAILED" };
    println!("{bold}Quality gate: {gate_color}{gate_label}{reset}");
    println!();
}

/// Fetch `/api/explain/{trace_id}` for each `trace_id` in parallel with
/// bounded concurrency. Returns a map of successfully-parsed trees keyed
/// by `trace_id`. Traces that return an error response (e.g. aged out of
/// the daemon window) are silently skipped.
///
/// Used by `query inspect` to pre-populate the TUI detail panel without
/// the multi-second startup latency a sequential loop would incur.
#[cfg(feature = "daemon")]
async fn fetch_explain_trees(
    client: &sentinel_core::http_client::HttpClient,
    base_url: String,
    timeout: std::time::Duration,
    trace_ids: &std::collections::BTreeSet<String>,
    concurrency: usize,
) -> std::collections::HashMap<String, String> {
    use tokio::task::JoinSet;

    let mut results: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut set: JoinSet<(String, Option<String>)> = JoinSet::new();
    let mut iter = trace_ids.iter();

    // Prime the join set with up to `concurrency` in-flight fetches.
    // `by_ref().take(concurrency)` stops cleanly when either the budget
    // or the trace_ids iterator is exhausted, whichever comes first.
    for tid in iter.by_ref().take(concurrency) {
        spawn_explain_fetch(&mut set, client, &base_url, timeout, tid.clone());
    }

    while let Some(join_result) = set.join_next().await {
        if let Ok((tid, tree_text)) = join_result
            && let Some(text) = tree_text
        {
            results.insert(tid, text);
        }
        // Maintain the concurrency window by launching the next pending
        // fetch as soon as one completes.
        if let Some(tid) = iter.next() {
            spawn_explain_fetch(&mut set, client, &base_url, timeout, tid.clone());
        }
    }

    results
}

#[cfg(feature = "daemon")]
fn spawn_explain_fetch(
    set: &mut tokio::task::JoinSet<(String, Option<String>)>,
    client: &sentinel_core::http_client::HttpClient,
    base_url: &str,
    timeout: std::time::Duration,
    trace_id: String,
) {
    let client = client.clone();
    let base = base_url.to_string();
    set.spawn(async move {
        let Ok(uri) =
            format!("{base}/api/explain/{trace_id}").parse::<sentinel_core::http_client::Uri>()
        else {
            return (trace_id, None);
        };
        let Ok(body) =
            sentinel_core::http_client::fetch_get(&client, &uri, "perf-sentinel-query", timeout)
                .await
        else {
            return (trace_id, None);
        };
        let text = serde_json::from_slice::<sentinel_core::explain::ExplainTree>(&body)
            .ok()
            .map(|tree| sentinel_core::explain::format_tree_text(&tree, false));
        (trace_id, text)
    });
}

#[cfg(feature = "daemon")]
async fn cmd_query(daemon_url: &str, action: QueryAction) {
    let client = sentinel_core::http_client::build_client();
    let timeout = std::time::Duration::from_secs(10);

    // Validate the daemon URL upfront so misconfigurations fail with a
    // clear error before the first request goes out.
    let trimmed = daemon_url.trim_end_matches('/');
    let base_uri: sentinel_core::http_client::Uri = trimmed.parse().unwrap_or_else(|e| {
        eprintln!("Invalid daemon URL `{daemon_url}`: {e}");
        std::process::exit(1);
    });
    if !matches!(base_uri.scheme_str(), Some("http" | "https")) {
        eprintln!("Invalid daemon URL `{daemon_url}`: scheme must be http or https");
        std::process::exit(1);
    }

    let fetch = |path: &str| {
        let uri: sentinel_core::http_client::Uri =
            format!("{trimmed}{path}").parse().unwrap_or_else(|e| {
                eprintln!("Invalid daemon URL path `{path}`: {e}");
                std::process::exit(1);
            });
        let client = &client;
        async move {
            match sentinel_core::http_client::fetch_get(
                client,
                &uri,
                "perf-sentinel-query",
                timeout,
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    eprintln!(
                        "Failed to connect to daemon at {daemon_url}: {e}\n\
                         Is `perf-sentinel watch` running?"
                    );
                    std::process::exit(1);
                }
            }
        }
    };

    match action {
        QueryAction::Findings {
            service,
            finding_type,
            severity,
            limit,
            format,
        } => {
            let mut params = vec![format!("limit={limit}")];
            if let Some(ref s) = service {
                params.push(format!("service={s}"));
            }
            if let Some(ref t) = finding_type {
                params.push(format!("type={t}"));
            }
            if let Some(ref s) = severity {
                params.push(format!("severity={s}"));
            }
            let query_str = params.join("&");
            let body = fetch(&format!("/api/findings?{query_str}")).await;
            match format {
                QueryOutputFormat::Json => {
                    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );
                }
                QueryOutputFormat::Text => {
                    let stored: Vec<sentinel_core::daemon::findings_store::StoredFinding> =
                        serde_json::from_slice(&body).unwrap_or_default();
                    let findings: Vec<sentinel_core::detect::Finding> =
                        stored.into_iter().map(|sf| sf.finding).collect();
                    if findings.is_empty() {
                        let AnsiColors { green, reset, .. } = ansi_colors(false);
                        println!("{green}No findings from daemon.{reset}");
                    } else {
                        let AnsiColors {
                            bold,
                            cyan,
                            dim,
                            reset,
                            ..
                        } = ansi_colors(false);
                        println!();
                        println!(
                            "{bold}{cyan}=== perf-sentinel daemon findings ({} results) ==={reset}",
                            findings.len()
                        );
                        println!("{dim}Source: {daemon_url}{reset}");
                        println!();
                        print_findings(&findings, false);
                    }
                }
            }
        }
        QueryAction::Explain { trace_id, format } => {
            let body = fetch(&format!("/api/explain/{trace_id}")).await;
            match format {
                QueryOutputFormat::Json => {
                    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );
                }
                QueryOutputFormat::Text => {
                    if let Ok(tree) =
                        serde_json::from_slice::<sentinel_core::explain::ExplainTree>(&body)
                    {
                        let text = sentinel_core::explain::format_tree_text(&tree, true);
                        println!("{text}");
                    } else {
                        // May be an error response from the daemon.
                        let json: serde_json::Value =
                            serde_json::from_slice(&body).unwrap_or_default();
                        if let Some(err) = json.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {err}");
                        } else {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&json).unwrap_or_default()
                            );
                        }
                    }
                }
            }
        }
        #[cfg(feature = "tui")]
        QueryAction::Inspect => {
            let body = fetch("/api/findings?limit=10000").await;
            let stored: Vec<sentinel_core::daemon::findings_store::StoredFinding> =
                serde_json::from_slice(&body).unwrap_or_default();
            let findings: Vec<sentinel_core::detect::Finding> =
                stored.into_iter().map(|sf| sf.finding).collect();
            if findings.is_empty() {
                let AnsiColors { green, reset, .. } = ansi_colors(false);
                println!("{green}No findings from daemon. Nothing to inspect.{reset}");
                return;
            }
            // Build minimal Trace stubs from distinct trace_ids in findings.
            // The TUI shows findings in panels. For each trace, fetch the
            // explain tree upfront from the daemon so the detail panel can
            // render real span trees (the daemon has them in its window,
            // but they do not ship with /api/findings).
            //
            // Fetches run in parallel with bounded concurrency via a
            // `JoinSet`. Previously this loop was sequential, which meant
            // 100 traces * 50ms RTT = 5s TUI startup. With concurrency 16
            // the same workload completes in ~300ms. Server-side the
            // query API reads from in-memory state (no I/O), so high
            // client concurrency is safe.
            let trace_ids: std::collections::BTreeSet<String> =
                findings.iter().map(|f| f.trace_id.clone()).collect();
            let pre_rendered_trees =
                fetch_explain_trees(&client, trimmed.to_string(), timeout, &trace_ids, 16).await;
            let traces: Vec<sentinel_core::correlate::Trace> = trace_ids
                .into_iter()
                .map(|tid| sentinel_core::correlate::Trace {
                    trace_id: tid,
                    spans: vec![],
                })
                .collect();
            let detect_config = sentinel_core::detect::DetectConfig {
                n_plus_one_threshold: 5,
                window_ms: 500,
                slow_threshold_ms: 500,
                slow_min_occurrences: 3,
                max_fanout: 20,
                chatty_service_min_calls: 15,
                pool_saturation_concurrent_threshold: 10,
                serialized_min_sequential: 3,
            };
            let mut app = tui::App::new(findings, traces, detect_config)
                .with_pre_rendered_trees(pre_rendered_trees);
            if let Err(e) = tui::run(&mut app) {
                eprintln!("TUI error: {e}");
                std::process::exit(1);
            }
        }
        QueryAction::Correlations { format } => {
            let body = fetch("/api/correlations").await;
            match format {
                QueryOutputFormat::Json => {
                    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );
                }
                QueryOutputFormat::Text => {
                    let correlations: Vec<
                        sentinel_core::detect::correlate_cross::CrossTraceCorrelation,
                    > = serde_json::from_slice(&body).unwrap_or_default();
                    if correlations.is_empty() {
                        let AnsiColors { green, reset, .. } = ansi_colors(false);
                        println!("{green}No active cross-trace correlations.{reset}");
                    } else {
                        let AnsiColors {
                            bold,
                            cyan,
                            red,
                            yellow,
                            dim,
                            reset,
                            ..
                        } = ansi_colors(false);
                        println!();
                        println!(
                            "{bold}{cyan}=== Cross-trace correlations ({} active) ==={reset}",
                            correlations.len()
                        );
                        println!();
                        for (i, c) in correlations.iter().enumerate() {
                            let conf_color = if c.confidence >= 0.8 {
                                red
                            } else if c.confidence >= 0.5 {
                                yellow
                            } else {
                                dim
                            };
                            println!(
                                "  {bold}#{} {}{reset} in {}",
                                i + 1,
                                c.source.finding_type.as_str(),
                                c.source.service
                            );
                            println!(
                                "    {dim}->{reset} {} in {}",
                                c.target.finding_type.as_str(),
                                c.target.service
                            );
                            println!(
                                "    {dim}Observed:{reset} {} times, \
                                 {dim}median lag:{reset} {:.1}ms, \
                                 {conf_color}confidence: {:.0}%{reset}",
                                c.co_occurrence_count,
                                c.median_lag_ms,
                                c.confidence * 100.0
                            );
                            println!(
                                "    {dim}Period:{reset} {} .. {}",
                                c.first_seen, c.last_seen
                            );
                            println!();
                        }
                    }
                }
            }
        }
        QueryAction::Status { format } => {
            let body = fetch("/api/status").await;
            match format {
                QueryOutputFormat::Json => {
                    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json).unwrap_or_default()
                    );
                }
                QueryOutputFormat::Text => {
                    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    let AnsiColors {
                        bold,
                        cyan,
                        green,
                        dim,
                        reset,
                        ..
                    } = ansi_colors(false);
                    println!();
                    println!("{bold}{cyan}=== perf-sentinel daemon status ==={reset}");
                    println!();
                    if let Some(v) = json.get("version").and_then(serde_json::Value::as_str) {
                        println!("  {dim}Version:{reset}          {green}{v}{reset}");
                    }
                    if let Some(u) = json
                        .get("uptime_seconds")
                        .and_then(serde_json::Value::as_u64)
                    {
                        let h = u / 3600;
                        let m = (u % 3600) / 60;
                        let s = u % 60;
                        println!("  {dim}Uptime:{reset}           {h}h {m}m {s}s");
                    }
                    if let Some(t) = json
                        .get("active_traces")
                        .and_then(serde_json::Value::as_u64)
                    {
                        println!("  {dim}Active traces:{reset}    {t}");
                    }
                    if let Some(f) = json
                        .get("stored_findings")
                        .and_then(serde_json::Value::as_u64)
                    {
                        println!("  {dim}Stored findings:{reset}  {f}");
                    }
                    println!();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::detect::{Confidence, Finding, FindingType, GreenImpact, Pattern, Severity};
    use sentinel_core::report::{
        Analysis, GreenSummary, QualityGate, QualityRule, Report, TopOffender,
    };

    fn make_report(
        findings: Vec<Finding>,
        top_offenders: Vec<TopOffender>,
        gate_passed: bool,
        rules: Vec<QualityRule>,
    ) -> Report {
        let event_count = if findings.is_empty() { 4 } else { 10 };
        Report {
            analysis: Analysis {
                duration_ms: 1,
                events_processed: event_count,
                traces_analyzed: 1,
            },
            findings,
            green_summary: GreenSummary {
                total_io_ops: event_count,
                avoidable_io_ops: 0,
                io_waste_ratio: 0.0,
                io_waste_ratio_band: sentinel_core::InterpretationLevel::Healthy,
                top_offenders,
                co2: None,
                regions: vec![],
                transport_gco2: None,
            },
            quality_gate: QualityGate {
                passed: gate_passed,
                rules,
            },
        }
    }

    fn make_finding(finding_type: FindingType, severity: Severity) -> Finding {
        Finding {
            finding_type,
            severity,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 6,
                window_ms: 200,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: Some(GreenImpact {
                estimated_extra_io_ops: 5,
                io_intensity_score: 6.0,
                io_intensity_band: sentinel_core::InterpretationLevel::for_iis(6.0),
            }),
            confidence: Confidence::default(),
            code_location: None,
        }
    }

    #[test]
    fn report_no_findings() {
        let report = make_report(vec![], vec![], true, vec![]);
        // Should not panic and should print "No performance anti-patterns detected."
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_critical_severity() {
        let report = make_report(
            vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_info_severity() {
        let report = make_report(
            vec![make_finding(FindingType::RedundantSql, Severity::Info)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_redundant_http_type() {
        let report = make_report(
            vec![make_finding(FindingType::RedundantHttp, Severity::Warning)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_slow_sql_type() {
        let report = make_report(
            vec![make_finding(FindingType::SlowSql, Severity::Warning)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_slow_http_type() {
        let report = make_report(
            vec![make_finding(FindingType::SlowHttp, Severity::Critical)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_quality_gate_failed() {
        let report = make_report(
            vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)],
            vec![],
            false,
            vec![QualityRule {
                rule: "n_plus_one_sql_critical_max".to_string(),
                threshold: 0.0,
                actual: 1.0,
                passed: false,
            }],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_with_top_offenders() {
        let report = make_report(
            vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)],
            vec![TopOffender {
                endpoint: "POST /api/orders/{id}/submit".to_string(),
                service: "order-svc".to_string(),
                io_intensity_score: 8.2,
                io_intensity_band: sentinel_core::InterpretationLevel::for_iis(8.2),
                co2_grams: None,
            }],
            true,
            vec![],
        );
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_with_ansi_colors() {
        // Test the TTY=true branch (force_color=true)
        let report = make_report(
            vec![
                make_finding(FindingType::NPlusOneSql, Severity::Critical),
                make_finding(FindingType::NPlusOneHttp, Severity::Warning),
                make_finding(FindingType::RedundantSql, Severity::Info),
                make_finding(FindingType::RedundantHttp, Severity::Info),
            ],
            vec![TopOffender {
                endpoint: "POST /api/orders/{id}/submit".to_string(),
                service: "order-svc".to_string(),
                io_intensity_score: 8.2,
                io_intensity_band: sentinel_core::InterpretationLevel::for_iis(8.2),
                co2_grams: None,
            }],
            false,
            vec![],
        );
        format_colored_report(&report, "report", true);
    }

    #[test]
    fn report_with_co2_data() {
        let report = Report {
            analysis: Analysis {
                duration_ms: 1,
                events_processed: 10,
                traces_analyzed: 1,
            },
            findings: vec![],
            green_summary: GreenSummary {
                total_io_ops: 10,
                avoidable_io_ops: 5,
                io_waste_ratio: 0.5,
                io_waste_ratio_band: sentinel_core::InterpretationLevel::for_waste_ratio(0.5),
                top_offenders: vec![TopOffender {
                    endpoint: "POST /api/orders/{id}/submit".to_string(),
                    service: "order-svc".to_string(),
                    io_intensity_score: 8.2,
                    io_intensity_band: sentinel_core::InterpretationLevel::for_iis(8.2),
                    co2_grams: Some(0.001),
                }],
                co2: None,
                regions: vec![],
                transport_gco2: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
        };
        format_colored_report(&report, "report", false);
    }

    #[test]
    fn load_config_returns_default_when_no_file() {
        // No .perf-sentinel.toml in the test working directory
        let config = load_config(None);
        assert_eq!(config.n_plus_one_threshold, 5);
        assert_eq!(config.max_payload_size, 1_048_576);
    }

    #[test]
    fn load_config_reads_valid_file() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let config_path = dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            "[detection]\nn_plus_one_min_occurrences = 15\n",
        )
        .expect("failed to write config");

        let config = load_config(Some(&config_path));
        assert_eq!(config.n_plus_one_threshold, 15);
    }
}
