#![warn(clippy::pedantic)]
#![allow(clippy::too_many_lines)] // print_colored_report is long but straightforward

use clap::{Parser, Subcommand};
use sentinel_core::config::Config;
use sentinel_core::detect::{FindingType, Severity};
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
        Commands::Watch { addr, port } => cmd_watch(&addr, port),
        Commands::Demo => cmd_demo(),
    }
}

fn cmd_analyze(input: Option<&std::path::Path>) {
    let config = Config::default();
    let max_size = config.max_payload_size;
    let raw = if let Some(path) = input {
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

fn cmd_watch(addr: &str, port: u16) {
    info!("Starting daemon on {addr}:{port}");
    // TODO: implement daemon mode with OTLP receiver
    eprintln!("Watch mode is not yet implemented.");
}

fn cmd_demo() {
    const DEMO_DATA: &str = include_str!("demo_data.json");

    let config = Config::default();
    let ingest = JsonIngest::new(config.max_payload_size);
    let events = match ingest.ingest(DEMO_DATA.as_bytes()) {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error loading demo data: {e}");
            std::process::exit(1);
        }
    };

    let report = pipeline::analyze(events, &config);
    print_colored_report(&report);
}

fn print_colored_report(report: &Report) {
    use std::io::IsTerminal;

    let is_tty = std::io::stdout().is_terminal();
    let (bold, cyan, red, yellow, green, dim, reset) = if is_tty {
        (
            "\x1b[1m", "\x1b[36m", "\x1b[31m", "\x1b[33m", "\x1b[32m", "\x1b[2m", "\x1b[0m",
        )
    } else {
        ("", "", "", "", "", "", "")
    };

    println!();
    println!("{bold}{cyan}=== perf-sentinel demo ==={reset}");
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
        println!("{bold}Found {} issue(s):{reset}", report.findings.len());
        println!();

        for (i, finding) in report.findings.iter().enumerate() {
            let severity_color = match finding.severity {
                Severity::Critical => red,
                Severity::Warning => yellow,
                Severity::Info => dim,
            };

            let type_label = match finding.finding_type {
                FindingType::NPlusOneSql => "N+1 SQL",
                FindingType::NPlusOneHttp => "N+1 HTTP",
                FindingType::RedundantSql => "Redundant SQL",
                FindingType::RedundantHttp => "Redundant HTTP",
            };

            println!(
                "  {bold}{severity_color}[{severity}] #{num} {type_label}{reset}",
                severity = match finding.severity {
                    Severity::Critical => "CRITICAL",
                    Severity::Warning => "WARNING",
                    Severity::Info => "INFO",
                },
                num = i + 1,
            );
            println!("    {dim}Trace:{reset}    {}", finding.trace_id);
            println!("    {dim}Service:{reset}  {}", finding.service);
            println!("    {dim}Endpoint:{reset} {}", finding.source_endpoint);
            println!("    {dim}Template:{reset} {}", finding.pattern.template);
            println!(
                "    {dim}Hits:{reset}     {} occurrences, {} distinct params, {}ms window",
                finding.pattern.occurrences,
                finding.pattern.distinct_params,
                finding.pattern.window_ms
            );
            println!("    {cyan}Suggestion:{reset} {}", finding.suggestion);
            println!();
        }
    }

    // Green summary
    println!("{bold}{cyan}--- GreenOps Summary ---{reset}");
    println!("  Total I/O ops:     {}", report.green_summary.total_io_ops);
    println!(
        "  Avoidable I/O ops: {}",
        report.green_summary.avoidable_io_ops
    );
    println!(
        "  I/O waste ratio:   {:.1}%",
        report.green_summary.io_waste_ratio * 100.0
    );
    println!();

    // Quality gate
    let gate_color = if report.quality_gate.passed {
        green
    } else {
        red
    };
    let gate_label = if report.quality_gate.passed {
        "PASSED"
    } else {
        "FAILED"
    };
    println!("{bold}Quality gate: {gate_color}{gate_label}{reset}");
    println!();
}
