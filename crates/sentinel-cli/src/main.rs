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
    if !report.green_summary.top_offenders.is_empty() {
        println!();
        println!("  {bold}Top offenders:{reset}");
        for offender in &report.green_summary.top_offenders {
            println!(
                "    - {}: IIS {:.1}, {:.1} I/O ops/req (service: {})",
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
            }],
            false,
            vec![],
        );
        format_colored_report(&report, true);
    }
}
