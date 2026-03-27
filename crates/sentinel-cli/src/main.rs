#![warn(clippy::pedantic)]
#![allow(clippy::too_many_lines)] // print_colored_report is long but straightforward
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::items_after_statements)] // bench report struct defined near its use

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
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Enable CI quality gate mode (exit 1 if gate fails).
        #[arg(long)]
        ci: bool,
    },

    /// Watch for traces in real-time (daemon mode).
    Watch {
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Run analysis on an embedded demo dataset.
    Demo,

    /// Benchmark perf-sentinel on a trace file.
    Bench {
        /// Path to a JSON trace file. Reads from stdin if omitted.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// Number of iterations (default 10).
        #[arg(long, default_value = "10")]
        iterations: u32,
    },
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
        Commands::Analyze { input, config, ci } => {
            cmd_analyze(input.as_deref(), config.as_deref(), ci);
        }
        Commands::Watch { config } => cmd_watch(config.as_deref()).await,
        Commands::Demo => cmd_demo(),
        Commands::Bench { input, iterations } => cmd_bench(input.as_deref(), iterations),
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

fn cmd_analyze(input: Option<&std::path::Path>, config_path: Option<&std::path::Path>, ci: bool) {
    let config = load_config(config_path);
    let raw = read_events(input, config.max_payload_size);

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

    if ci && !report.quality_gate.passed {
        eprintln!("Quality gate FAILED");
        std::process::exit(1);
    }
}

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

fn cmd_bench(input: Option<&std::path::Path>, iterations: u32) {
    if iterations == 0 {
        eprintln!("Error: iterations must be >= 1");
        std::process::exit(1);
    }

    let config = Config::default();
    let raw = read_events(input, config.max_payload_size);

    let ingest = JsonIngest::new(config.max_payload_size);
    let events = match ingest.ingest(&raw) {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error ingesting events: {e}");
            std::process::exit(1);
        }
    };

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

    // Compute per-event latencies
    let mut per_event_ns: Vec<f64> = durations_ns
        .iter()
        .map(|&d| d as f64 / event_count as f64)
        .collect();
    per_event_ns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let p50_idx = (per_event_ns.len() as f64 * 0.50) as usize;
    let p99_idx = ((per_event_ns.len() as f64 * 0.99).ceil() as usize).min(per_event_ns.len() - 1);
    let p50_us = per_event_ns[p50_idx] / 1000.0;
    let p99_us = per_event_ns[p99_idx] / 1000.0;

    let total_elapsed_ms: u64 = durations_ns.iter().sum::<u64>() / 1_000_000;
    let total_events = event_count as f64 * f64::from(iterations);
    let total_seconds = total_elapsed_ms as f64 / 1000.0;
    let throughput = if total_seconds > 0.0 {
        total_events / total_seconds
    } else {
        0.0
    };

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

/// Get current RSS (Resident Set Size) in bytes. Best-effort, platform-specific.
#[allow(clippy::missing_const_for_fn)] // not const on Linux (reads /proc)
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
        // Best-effort — returns None if unavailable
        None
    }
    #[cfg(target_os = "macos")]
    {
        // macOS: could use mach_task_info but keeping it simple
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

fn print_colored_report(report: &Report) {
    format_colored_report(report, false);
}

fn format_colored_report(report: &Report, force_color: bool) {
    use std::io::IsTerminal;

    let is_tty = force_color || std::io::stdout().is_terminal();
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
                FindingType::SlowSql => "Slow SQL",
                FindingType::SlowHttp => "Slow HTTP",
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
            println!(
                "    {dim}Window:{reset}   {} → {}",
                finding.first_timestamp, finding.last_timestamp
            );
            println!("    {cyan}Suggestion:{reset} {}", finding.suggestion);
            if let Some(ref impact) = finding.green_impact {
                println!(
                    "    {dim}Extra I/O:{reset} {} avoidable ops",
                    impact.estimated_extra_io_ops
                );
                println!("    {dim}IIS:{reset}      {:.1}", impact.io_intensity_score);
            }
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
    if let Some(co2) = report.green_summary.estimated_co2_grams {
        println!("  Est. CO\u{2082}:          {co2:.6} g");
    }
    if let Some(co2) = report.green_summary.avoidable_co2_grams {
        println!("  Avoidable CO\u{2082}:     {co2:.6} g");
    }
    if !report.green_summary.top_offenders.is_empty() {
        println!();
        println!("  {bold}Top offenders:{reset}");
        for offender in &report.green_summary.top_offenders {
            let co2_str = offender
                .co2_grams
                .map_or(String::new(), |co2| format!(", {co2:.6} gCO\u{2082}"));
            println!(
                "    - {}: IIS {:.1}, {:.1} I/O ops/req (service: {}){co2_str}",
                offender.endpoint,
                offender.io_intensity_score,
                offender.io_ops_per_request,
                offender.service,
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::detect::{Finding, FindingType, GreenImpact, Pattern, Severity};
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
                top_offenders,
                estimated_co2_grams: None,
                avoidable_co2_grams: None,
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
            service: "game".to_string(),
            source_endpoint: "POST /api/game/42/start".to_string(),
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
            }),
        }
    }

    #[test]
    fn report_no_findings() {
        let report = make_report(vec![], vec![], true, vec![]);
        // Should not panic and should print "No performance anti-patterns detected."
        format_colored_report(&report, false);
    }

    #[test]
    fn report_critical_severity() {
        let report = make_report(
            vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, false);
    }

    #[test]
    fn report_info_severity() {
        let report = make_report(
            vec![make_finding(FindingType::RedundantSql, Severity::Info)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, false);
    }

    #[test]
    fn report_redundant_http_type() {
        let report = make_report(
            vec![make_finding(FindingType::RedundantHttp, Severity::Warning)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, false);
    }

    #[test]
    fn report_slow_sql_type() {
        let report = make_report(
            vec![make_finding(FindingType::SlowSql, Severity::Warning)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, false);
    }

    #[test]
    fn report_slow_http_type() {
        let report = make_report(
            vec![make_finding(FindingType::SlowHttp, Severity::Critical)],
            vec![],
            true,
            vec![],
        );
        format_colored_report(&report, false);
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
        format_colored_report(&report, false);
    }

    #[test]
    fn report_with_top_offenders() {
        let report = make_report(
            vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)],
            vec![TopOffender {
                endpoint: "POST /api/game/{id}/start".to_string(),
                service: "game".to_string(),
                io_intensity_score: 8.2,
                io_ops_per_request: 8.2,
                co2_grams: None,
            }],
            true,
            vec![],
        );
        format_colored_report(&report, false);
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
                endpoint: "POST /api/game/{id}/start".to_string(),
                service: "game".to_string(),
                io_intensity_score: 8.2,
                io_ops_per_request: 8.2,
                co2_grams: None,
            }],
            false,
            vec![],
        );
        format_colored_report(&report, true);
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
                top_offenders: vec![TopOffender {
                    endpoint: "POST /api/game/{id}/start".to_string(),
                    service: "game".to_string(),
                    io_intensity_score: 8.2,
                    io_ops_per_request: 8.2,
                    co2_grams: Some(0.001),
                }],
                estimated_co2_grams: Some(0.002),
                avoidable_co2_grams: Some(0.001),
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
        };
        format_colored_report(&report, false);
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
