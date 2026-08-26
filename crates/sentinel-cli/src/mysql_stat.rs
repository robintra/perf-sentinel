//! `perf-sentinel mysql-stat` subcommand: Performance Schema digest
//! ingestion (file), ranking, and terminal/JSON output. Also hosts the
//! `mysql_stat` loader shared with the `report` subcommand.

use sentinel_core::config::Config;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;

use crate::{MySqlStatOutputFormat, limits, load_config, read_events, read_file_capped};

/// Ingest an `events_statements_summary_by_digest` CSV or JSON file and
/// produce the ranking report the HTML dashboard embeds. Exits
/// `EXIT_TOOLING_ERROR` on parse failure: `mysql-stat` has no quality
/// gate, so this is never a threshold breach.
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
            eprintln!(
                "Error parsing --mysql-stat {}: {}",
                path.display(),
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    }
}

/// Pick the `mysql_stat` source for a `report` run. `--mysql-stat` and
/// `--mysql-stat-prometheus` are mutually exclusive at the clap level
/// (`conflicts_with`). Mirrors `resolve_pg_stat_source`.
// The only `.await` is the daemon-gated Prometheus fetch, so the
// no-default-features build sees an async fn with no await.
#[cfg_attr(not(feature = "daemon"), allow(clippy::unused_async))]
pub(crate) async fn resolve_mysql_stat_source(
    path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] prometheus: Option<&str>,
    #[cfg(feature = "daemon")] auth_header: Option<String>,
    #[cfg(feature = "daemon")] opts: &sentinel_core::ingest::mysql_stat::PrometheusMySqlStat,
    top_n: usize,
) -> Option<sentinel_core::ingest::mysql_stat::MySqlStatReport> {
    if let Some(path) = path {
        return Some(load_mysql_stat_from_file(path, top_n));
    }
    #[cfg(feature = "daemon")]
    {
        let url = prometheus?;
        let resolved_auth = resolve_mysql_stat_auth_header(auth_header);
        Some(load_mysql_stat_from_prometheus(url, top_n, opts, resolved_auth.as_deref()).await)
    }
    #[cfg(not(feature = "daemon"))]
    {
        None
    }
}

/// Scrape a `mysqld_exporter` endpoint one-shot and produce the ranking
/// report the HTML dashboard embeds. Exits `EXIT_TOOLING_ERROR` on
/// transport or parse failure, `mysql-stat` has no quality gate to breach.
/// Mirrors `load_pg_stat_from_prometheus`.
#[cfg(feature = "daemon")]
pub(crate) async fn load_mysql_stat_from_prometheus(
    url: &str,
    top_n: usize,
    opts: &sentinel_core::ingest::mysql_stat::PrometheusMySqlStat,
    auth_header: Option<&str>,
) -> sentinel_core::ingest::mysql_stat::MySqlStatReport {
    let scrape_budget = top_n.max(crate::PROMETHEUS_SCRAPE_FLOOR);
    match sentinel_core::ingest::mysql_stat::fetch_from_prometheus(
        url,
        scrape_budget,
        auth_header,
        opts,
    )
    .await
    {
        Ok(entries) => sentinel_core::ingest::mysql_stat::rank_mysql_stat(&entries, top_n),
        Err(e) => {
            eprintln!(
                "Error scraping --mysql-stat-prometheus {url}: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    }
}

/// Run the `mysql-stat` command with prometheus-or-input branching, kept
/// out of the main dispatch so the match stays flat. Mirrors
/// `dispatch_pg_stat`.
#[allow(clippy::too_many_arguments)]
// The only `.await` is the daemon-gated Prometheus fetch below, so the
// no-default-features build sees an async fn with no await.
#[cfg_attr(not(feature = "daemon"), allow(clippy::unused_async))]
pub(crate) async fn dispatch_mysql_stat(
    input: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] prometheus: Option<&str>,
    #[cfg(feature = "daemon")] auth_header: Option<String>,
    #[cfg(feature = "daemon")] opts: &sentinel_core::ingest::mysql_stat::PrometheusMySqlStat,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config: Option<&std::path::Path>,
    format: MySqlStatOutputFormat,
) {
    #[cfg(feature = "daemon")]
    if let Some(prom_endpoint) = prometheus {
        let resolved_auth = resolve_mysql_stat_auth_header(auth_header);
        let entries = sentinel_core::ingest::mysql_stat::fetch_from_prometheus(
            prom_endpoint,
            top_n.max(crate::PROMETHEUS_SCRAPE_FLOOR),
            resolved_auth.as_deref(),
            opts,
        )
        .await
        .unwrap_or_else(|e| {
            eprintln!(
                "Prometheus fetch failed: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        });
        let config = load_config(config);
        run_mysql_stat_pipeline(entries, top_n, traces, &config, format);
        return;
    }
    let path = crate::require_input_path(input);
    cmd_mysql_stat(path, top_n, traces, config, format);
}

/// Resolve the auth header from the flag, falling back to the env var so a
/// token never has to appear in a shell history or a process list. Warns on
/// the flag path for the same reason `pg-stat` does: an argument is visible
/// in the process list.
#[cfg(feature = "daemon")]
pub(crate) fn resolve_mysql_stat_auth_header(flag: Option<String>) -> Option<String> {
    if flag.is_some() {
        tracing::warn!(
            "mysql-stat auth header supplied via a CLI flag. \
             Prefer the PERF_SENTINEL_MYSQLSTAT_AUTH_HEADER environment variable \
             to avoid exposing the credential through the process argument list \
             or shell history."
        );
    }
    flag.or_else(|| std::env::var("PERF_SENTINEL_MYSQLSTAT_AUTH_HEADER").ok())
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
            eprintln!(
                "Error parsing performance_schema digest export: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
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

    // Every traced SQL template counts, not only the ones that produced
    // a finding, so a healthy traced query still gets its marker.
    let mut cross_referenced = false;
    if let Some(traces_path) = traces {
        let traces_raw = read_events(Some(traces_path), limits::MAX_BATCH_INPUT_BYTES);
        let ingest = JsonIngest::new(limits::MAX_BATCH_INPUT_BYTES)
            .with_grouping_attributes(crate::grouping_keys(config));
        match ingest.ingest(&traces_raw) {
            Ok(events) => {
                let (_, analyzed_traces) = pipeline::analyze_with_traces(events, config, None);
                let templates = pipeline::trace_sql_templates(&analyzed_traces);
                mysql_stat::cross_reference_templates(&mut entries, &templates);
                cross_referenced = true;
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to ingest trace file for cross-reference: {}",
                    sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
                );
            }
        }
    }

    let mut report = mysql_stat::rank_mysql_stat(&entries, top_n);
    if cross_referenced {
        report.trace_match = Some(mysql_stat::trace_match_summary(&entries));
    }

    match format {
        MySqlStatOutputFormat::Json => {
            // A derive-`Serialize` report over owned scalars never fails to
            // serialize, so fall back to an empty string rather than a
            // dead error branch (matches query.rs / verify_hash.rs).
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
        }
        MySqlStatOutputFormat::Text => print_mysql_stat_report(&report),
    }
}

fn print_mysql_stat_report(report: &sentinel_core::ingest::mysql_stat::MySqlStatReport) {
    use sentinel_core::text_safety::sanitize_for_terminal;
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
    if let Some(tm) = &report.trace_match {
        // "Matched share", not "coverage": digest counters are cumulative
        // since the last stats reset while the traces cover one window.
        println!(
            "{dim}Trace-matched:{reset} {}/{} templates, {:.1}% of calls",
            tm.matched_templates,
            tm.total_templates,
            tm.calls_share_percent()
        );
    }
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
            // Digest exports are untrusted input reaching a terminal:
            // strip control bytes per the text_safety convention.
            println!(
                "  {bold}#{}{reset} {}{trace_marker}",
                i + 1,
                sanitize_for_terminal(&entry.normalized_template)
            );
            if let Some(schema) = &entry.schema_name {
                println!("    {dim}schema:{reset} {}", sanitize_for_terminal(schema));
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
