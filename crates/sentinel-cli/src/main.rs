use clap::{Parser, Subcommand};
use sentinel_core::config::Config;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;
use sentinel_core::report::ReportSink;
use sentinel_core::report::json::JsonReportSink;
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

#[derive(Subcommand)]
enum Commands {
    /// Analyze trace files in batch mode. Reads from stdin if no --input is given.
    Analyze {
        /// Path to a JSON trace file to analyze. If omitted, reads from stdin.
        #[arg(short, long)]
        input: Option<PathBuf>,
    },

    /// Watch for traces in real-time (daemon mode).
    Watch {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1")]
        addr: String,
        /// Port to listen on.
        #[arg(long, default_value_t = 4318)]
        port: u16,
    },

    /// Run analysis on an embedded demo dataset.
    Demo,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Analyze { input } => cmd_analyze(input.as_deref()),
        Commands::Watch { addr, port } => cmd_watch(&addr, port).await,
        Commands::Demo => cmd_demo(),
    }
}

fn cmd_analyze(input: Option<&std::path::Path>) {
    let config = Config::default();
    let max_size = config.max_payload_size;
    let raw = match input {
        Some(path) => {
            info!("Analyzing trace file: {}", path.display());
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
        }
        None => {
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
    };

    let ingest = JsonIngest::new(config.max_payload_size);
    let events = match ingest.ingest(&raw) {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error ingesting events: {e}");
            std::process::exit(1);
        }
    };

    let report = pipeline::analyze(events, &config);
    let sink = JsonReportSink;
    if let Err(e) = sink.emit(&report) {
        eprintln!("Error writing report: {e}");
        std::process::exit(1);
    }
}

async fn cmd_watch(addr: &str, port: u16) {
    info!("Starting daemon on {addr}:{port}");
    // TODO: implement daemon mode with OTLP receiver
    eprintln!("Watch mode is not yet implemented.");
}

fn cmd_demo() {
    info!("Running demo analysis");
    // TODO: embed a demo dataset and analyze it
    let config = Config::default();
    let report = pipeline::analyze(vec![], &config);
    let sink = JsonReportSink;
    if let Err(e) = sink.emit(&report) {
        eprintln!("Error writing report: {e}");
        std::process::exit(1);
    }
}
