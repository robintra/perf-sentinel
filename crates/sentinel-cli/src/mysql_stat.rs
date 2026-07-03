//! `perf-sentinel mysql-stat` subcommand: Performance Schema digest
//! ingestion (file), ranking, and terminal/JSON output. Also hosts the
//! `mysql_stat` loader shared with the `report` subcommand.

use sentinel_core::config::Config;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;

use crate::{MySqlStatOutputFormat, limits, load_config, read_events, read_file_capped};

/// Ingest an `events_statements_summary_by_digest` CSV or JSON file and
/// produce the ranking report the HTML dashboard embeds. Exits 1 on
/// parse failure.
pub(crate) fn load_mysql_stat_from_file(
    path: &std::path::Path,
    top_n: usize,
) -> sentinel_core::ingest::mysql_stat::MySqlStatReport {
    let raw = read_file_capped(
        path,
        u64::try_from(limits::MAX_BATCH_INPUT_BYTES).unwrap_or(u64::MAX),
    );
    match sentinel_core::ingest::mysql_stat::parse_mysql_stat(&raw, limits::MAX_BATCH_INPUT_BYTES) {
        Ok(entries) => sentinel_core::ingest::mysql_stat::rank_mysql_stat(&entries, top_n),
        Err(e) => {
            eprintln!("Error parsing --mysql-stat {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}

/// Run the `mysql-stat` subcommand: parse the digest export, optionally
/// cross-reference against trace findings, rank, and print.
pub(crate) fn cmd_mysql_stat(
    input: &std::path::Path,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    format: MySqlStatOutputFormat,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), limits::MAX_BATCH_INPUT_BYTES);

    let entries = match sentinel_core::ingest::mysql_stat::parse_mysql_stat(
        &raw,
        limits::MAX_BATCH_INPUT_BYTES,
    ) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Error parsing performance_schema digest export: {e}");
            std::process::exit(1);
        }
    };

    run_mysql_stat_pipeline(entries, top_n, traces, &config, format);
}

/// Optional trace cross-reference, ranking, then text or JSON output.
/// Mirrors `run_pg_stat_pipeline`: a trace-ingest failure is a warning,
/// not a fatal error, so the digest report still prints.
fn run_mysql_stat_pipeline(
    mut entries: Vec<sentinel_core::ingest::mysql_stat::MySqlStatEntry>,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config: &Config,
    format: MySqlStatOutputFormat,
) {
    use sentinel_core::ingest::mysql_stat;

    if let Some(traces_path) = traces {
        let traces_raw = read_events(Some(traces_path), limits::MAX_BATCH_INPUT_BYTES);
        let ingest = JsonIngest::new(limits::MAX_BATCH_INPUT_BYTES);
        match ingest.ingest(&traces_raw) {
            Ok(events) => {
                let report = pipeline::analyze(events, config);
                mysql_stat::cross_reference(&mut entries, &report.findings);
            }
            Err(e) => {
                eprintln!("Warning: failed to ingest trace file for cross-reference: {e}");
            }
        }
    }

    let report = mysql_stat::rank_mysql_stat(&entries, top_n);

    match format {
        MySqlStatOutputFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("Error serializing mysql_stat report: {e}");
                std::process::exit(1);
            }
        },
        MySqlStatOutputFormat::Text => print_mysql_stat_report(&report),
    }
}

fn print_mysql_stat_report(report: &sentinel_core::ingest::mysql_stat::MySqlStatReport) {
    use std::io::IsTerminal;

    let is_tty = std::io::stdout().is_terminal();
    let (bold, cyan, yellow, dim, reset) = if is_tty {
        ("\x1b[1m", "\x1b[36m", "\x1b[33m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };

    println!();
    println!("{bold}{cyan}=== performance_schema digest analysis ==={reset}");
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
            if let Some(schema) = &entry.schema_name {
                println!("    {dim}schema:{reset} {schema}");
            }
            println!(
                "    {dim}calls:{reset} {}  {dim}total:{reset} {:.2}ms  {dim}mean:{reset} {:.2}ms",
                entry.calls, entry.total_exec_time_ms, entry.mean_exec_time_ms
            );
            println!(
                "    {dim}rows_sent:{reset} {}  {dim}rows_examined:{reset} {}",
                entry.rows_sent, entry.rows_examined
            );
            println!();
        }
    }
}
