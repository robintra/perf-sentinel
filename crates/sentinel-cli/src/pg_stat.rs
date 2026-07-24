//! `perf-sentinel pg-stat` subcommand: `pg_stat_statements` ingestion
//! (file or Prometheus scrape), ranking, and terminal/JSON output.
//! Also hosts the `pg_stat` loaders shared with the `report` subcommand.

use sentinel_core::config::Config;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;

use crate::{PgStatOutputFormat, limits, load_config, read_events, read_file_capped};

/// Run the `pg-stat` command with prometheus-or-input branching
/// extracted out of the main dispatch so it does not inflate the
/// match's cognitive complexity.
#[allow(clippy::too_many_arguments)]
// The only `.await` is the daemon-gated Prometheus fetch below, so the
// no-default-features build sees an async fn with no await.
#[cfg_attr(not(feature = "daemon"), allow(clippy::unused_async))]
pub(crate) async fn dispatch_pg_stat(
    input: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] prometheus: Option<&str>,
    #[cfg(feature = "daemon")] auth_header: Option<String>,
    top_n: usize,
    traces: Option<&std::path::Path>,
    config: Option<&std::path::Path>,
    format: PgStatOutputFormat,
) {
    #[cfg(feature = "daemon")]
    if let Some(prom_endpoint) = prometheus {
        let resolved_auth = resolve_pg_stat_auth_header(auth_header);
        let entries = sentinel_core::ingest::pg_stat::fetch_from_prometheus(
            prom_endpoint,
            top_n,
            resolved_auth.as_deref(),
        )
        .await
        .unwrap_or_else(|e| {
            eprintln!(
                "Prometheus fetch failed: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        });
        cmd_pg_stat_from_entries(entries, top_n, traces, config, format);
        return;
    }
    let Some(path) = input else {
        #[cfg(feature = "daemon")]
        eprintln!("Either --input or --prometheus is required");
        #[cfg(not(feature = "daemon"))]
        eprintln!("--input is required");
        std::process::exit(crate::EXIT_TOOLING_ERROR);
    };
    cmd_pg_stat(path, top_n, traces, config, format);
}

/// Lower bound on the Prometheus scrape size when only a small
/// `--pg-stat-top` is set. `rank_pg_stat` emits four rankings keyed on
/// different columns; feeding it only the `top_n` by `seconds_total`
/// (the upstream `topk` metric) biases the three non-time rankings.
/// Always scrape at least this many rows so the secondary rankings see
/// the full hot-spot distribution.
#[cfg(feature = "daemon")]
const PROMETHEUS_SCRAPE_FLOOR: usize = 200;

/// Ingest a `pg_stat_statements` CSV or JSON file and produce the
/// ranking report the HTML dashboard embeds. Exits `EXIT_TOOLING_ERROR`
/// on parse failure: `pg-stat` has no quality gate, so this is never a
/// threshold breach.
pub(crate) fn load_pg_stat_from_file(
    path: &std::path::Path,
    top_n: usize,
) -> sentinel_core::ingest::pg_stat::PgStatReport {
    let raw_pg = read_file_capped(
        path,
        u64::try_from(limits::MAX_BATCH_INPUT_BYTES).unwrap_or(u64::MAX),
    );
    match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw_pg, limits::MAX_BATCH_INPUT_BYTES) {
        Ok(entries) => sentinel_core::ingest::pg_stat::rank_pg_stat(&entries, top_n),
        Err(e) => {
            eprintln!(
                "Error parsing --pg-stat {}: {}",
                path.display(),
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    }
}

/// Scrape a `postgres_exporter` endpoint one-shot and produce the
/// ranking report. Exits `EXIT_TOOLING_ERROR` on transport/parse
/// failure, `pg-stat` has no quality gate to breach.
#[cfg(feature = "daemon")]
pub(crate) async fn load_pg_stat_from_prometheus(
    url: &str,
    _config: &Config,
    top_n: usize,
    auth_header: Option<&str>,
) -> sentinel_core::ingest::pg_stat::PgStatReport {
    let scrape_budget = top_n.max(PROMETHEUS_SCRAPE_FLOOR);
    match sentinel_core::ingest::pg_stat::fetch_from_prometheus(url, scrape_budget, auth_header)
        .await
    {
        Ok(entries) => sentinel_core::ingest::pg_stat::rank_pg_stat(&entries, top_n),
        Err(e) => {
            eprintln!(
                "Error scraping --pg-stat-prometheus {url}: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            std::process::exit(crate::EXIT_TOOLING_ERROR);
        }
    }
}

/// Resolve the `pg_stat` auth header value from the `PERF_SENTINEL_PGSTAT_AUTH_HEADER`
/// env var plus the CLI flag value. Env wins, flag is fallback, matching the
/// precedence of `PERF_SENTINEL_EMAPS_TOKEN` for Electricity Maps.
#[cfg(feature = "daemon")]
pub(crate) fn resolve_pg_stat_auth_header(flag_value: Option<String>) -> Option<String> {
    resolve_pg_stat_auth_header_with_env(flag_value, || {
        std::env::var("PERF_SENTINEL_PGSTAT_AUTH_HEADER").ok()
    })
}

/// Test-friendly inner form: takes the env-var lookup as a closure so
/// tests can exercise the precedence branch without mutating the
/// global process env.
#[cfg(feature = "daemon")]
pub(crate) fn resolve_pg_stat_auth_header_with_env(
    flag_value: Option<String>,
    env_lookup: impl FnOnce() -> Option<String>,
) -> Option<String> {
    match (env_lookup(), flag_value) {
        (Some(from_env), _) => Some(from_env),
        (None, Some(from_flag)) => {
            tracing::warn!(
                "pg-stat auth header supplied via a CLI flag. \
                 Prefer the PERF_SENTINEL_PGSTAT_AUTH_HEADER environment variable \
                 to avoid exposing the credential through the process argument list \
                 or shell history."
            );
            Some(from_flag)
        }
        (None, None) => None,
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
    let raw = read_events(Some(input), limits::MAX_BATCH_INPUT_BYTES);

    let entries =
        match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw, limits::MAX_BATCH_INPUT_BYTES) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!(
                    "Error parsing pg_stat_statements: {}",
                    sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
                );
                std::process::exit(crate::EXIT_TOOLING_ERROR);
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
        let traces_raw = read_events(Some(traces_path), limits::MAX_BATCH_INPUT_BYTES);
        let ingest = JsonIngest::new(limits::MAX_BATCH_INPUT_BYTES);
        match ingest.ingest(&traces_raw) {
            Ok(events) => {
                let report = pipeline::analyze(events, config);
                pg_stat::cross_reference(&mut entries, &report.findings);
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to ingest trace file for cross-reference: {}",
                    sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
                );
            }
        }
    }

    let report = pg_stat::rank_pg_stat(&entries, top_n);

    match format {
        PgStatOutputFormat::Json => {
            // A derive-`Serialize` report over owned scalars never fails to
            // serialize, so fall back to an empty string rather than a
            // dead error branch (matches query.rs / verify_hash.rs).
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
        }
        PgStatOutputFormat::Text => print_pg_stat_report(&report),
    }
}

fn print_pg_stat_report(report: &sentinel_core::ingest::pg_stat::PgStatReport) {
    use sentinel_core::text_safety::sanitize_for_terminal;
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
            // pg_stat exports are untrusted input reaching a terminal:
            // strip control bytes per the text_safety convention.
            println!(
                "  {bold}#{}{reset} {}{trace_marker}",
                i + 1,
                sanitize_for_terminal(&entry.normalized_template)
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
