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
        input: PathBuf,
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
            top_n,
            traces,
            config,
            format,
        } => cmd_pg_stat(&input, top_n, traces.as_deref(), config.as_deref(), format),
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

    // Determine output format: explicit --format takes priority, --ci defaults to json
    let effective_format = format.unwrap_or(if ci {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    });

    match effective_format {
        OutputFormat::Text => {
            print_colored_report(&report, "report");
        }
        OutputFormat::Json => {
            let sink = JsonReportSink;
            if let Err(e) = sink.emit(&report) {
                eprintln!("Error writing report: {e}");
                std::process::exit(1);
            }
        }
        OutputFormat::Sarif => {
            if let Err(e) = sentinel_core::report::sarif::emit_sarif(&report) {
                eprintln!("Error writing SARIF report: {e}");
                std::process::exit(1);
            }
        }
    }

    // In CI mode, exit 1 if quality gate fails (regardless of format)
    if ci && !report.quality_gate.passed {
        eprintln!("Quality gate FAILED");
        std::process::exit(1);
    }
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

    let effective_format = format.unwrap_or(if ci {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    });

    match effective_format {
        OutputFormat::Text => {
            print_colored_report(&report, "tempo");
        }
        OutputFormat::Json => {
            let sink = JsonReportSink;
            if let Err(e) = sink.emit(&report) {
                eprintln!("Error writing report: {e}");
                std::process::exit(1);
            }
        }
        OutputFormat::Sarif => {
            if let Err(e) = sentinel_core::report::sarif::emit_sarif(&report) {
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
    use sentinel_core::ingest::pg_stat;

    let config = load_config(config_path);
    let raw = read_events(Some(input), config.max_payload_size);

    let mut entries = match pg_stat::parse_pg_stat(&raw, config.max_payload_size) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Error parsing pg_stat_statements: {e}");
            std::process::exit(1);
        }
    };

    // Cross-reference with trace findings if --traces is provided
    if let Some(traces_path) = traces {
        let traces_raw = read_events(Some(traces_path), config.max_payload_size);
        let ingest = JsonIngest::new(config.max_payload_size);
        match ingest.ingest(&traces_raw) {
            Ok(events) => {
                let report = pipeline::analyze(events, &config);
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

fn print_colored_report(report: &Report, title: &str) {
    format_colored_report(report, title, false);
}

/// ANSI color codes tuple: (bold, cyan, red, yellow, green, dim, reset).
type AnsiColors = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

fn ansi_colors(force_color: bool) -> AnsiColors {
    use std::io::IsTerminal;
    if force_color || std::io::stdout().is_terminal() {
        (
            "\x1b[1m", "\x1b[36m", "\x1b[31m", "\x1b[33m", "\x1b[32m", "\x1b[2m", "\x1b[0m",
        )
    } else {
        ("", "", "", "", "", "", "")
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
    let (_bold, _cyan, red, yellow, green, _dim, _reset) = colors;
    use sentinel_core::InterpretationLevel::{Critical, Healthy, High, Moderate};
    match level {
        Critical => red,
        High => yellow,
        Moderate => "",
        Healthy => green,
    }
}

fn format_colored_report(report: &Report, title: &str, force_color: bool) {
    let (bold, cyan, _red, _yellow, green, dim, reset) = ansi_colors(force_color);

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
    let (bold, cyan, _red, _yellow, _green, dim, reset) = ansi_colors(force_color);

    println!("{bold}Found {} issue(s):{reset}", findings.len());
    println!();

    let colors = ansi_colors(force_color);
    for (i, finding) in findings.iter().enumerate() {
        let (_bold, _cyan, red, yellow, _green, _dim, _reset) = colors;
        let severity_color = match finding.severity {
            Severity::Critical => red,
            Severity::Warning => yellow,
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
    let (bold, cyan, _red, _yellow, _green, dim, reset) = colors;

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
    let (bold, _cyan, red, _yellow, green, _dim, reset) = ansi_colors(force_color);

    let gate_color = if gate.passed { green } else { red };
    let gate_label = if gate.passed { "PASSED" } else { "FAILED" };
    println!("{bold}Quality gate: {gate_color}{gate_label}{reset}");
    println!();
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
