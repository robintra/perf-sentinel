//! `perf-sentinel pg-stat` subcommand: `pg_stat_statements` ingestion
//! (file or Prometheus scrape), ranking, and terminal/JSON output.
//! Also hosts the `pg_stat` loaders shared with the `report` subcommand.

use sentinel_core::config::Config;

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
    #[cfg(feature = "daemon")] opts: &sentinel_core::ingest::pg_stat::PrometheusPgStat,
    top_n: usize,
    traces: Option<&std::path::Path>,
    baseline: Option<&std::path::Path>,
    config: Option<&std::path::Path>,
    format: PgStatOutputFormat,
) {
    #[cfg(feature = "daemon")]
    if let Some(prom_endpoint) = prometheus {
        let resolved_auth = resolve_pg_stat_auth_header(auth_header);
        let entries = sentinel_core::ingest::pg_stat::fetch_from_prometheus(
            prom_endpoint,
            // Same floor as the `report` path: `rank_pg_stat` emits four
            // rankings and only one of them is keyed on the `topk` metric.
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
        cmd_pg_stat_from_entries(entries, top_n, traces, baseline, config, format);
        return;
    }
    let path = crate::require_input_path(input);
    cmd_pg_stat(path, top_n, traces, baseline, config, format);
}

/// Ingest a `pg_stat_statements` CSV or JSON file and produce the
/// ranking report the HTML dashboard embeds. Exits `EXIT_TOOLING_ERROR`
/// on parse failure: `pg-stat` has no quality gate, so this is never a
/// threshold breach.
pub(crate) fn load_pg_stat_from_file(
    path: &std::path::Path,
    top_n: usize,
    trace_counts: Option<&std::collections::HashMap<String, u64>>,
) -> sentinel_core::ingest::pg_stat::PgStatReport {
    let raw_pg = read_file_capped(
        path,
        u64::try_from(limits::MAX_BATCH_INPUT_BYTES).unwrap_or(u64::MAX),
    );
    match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw_pg, limits::MAX_BATCH_INPUT_BYTES) {
        Ok(mut entries) => rank_with_trace_match(&mut entries, top_n, trace_counts),
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

/// Pick the `pg_stat` source for a `report` run. `--pg-stat` and
/// `--pg-stat-prometheus` are mutually exclusive at the clap level
/// (`conflicts_with`), and the Prometheus branch is gated behind the
/// daemon feature, mirroring the `pg-stat` subcommand surface.
// The only `.await` is the daemon-gated Prometheus fetch, so the
// no-default-features build sees an async fn with no await.
#[cfg_attr(not(feature = "daemon"), allow(clippy::unused_async))]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_pg_stat_source(
    path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] prometheus: Option<&str>,
    #[cfg(feature = "daemon")] auth_header: Option<String>,
    #[cfg(feature = "daemon")] opts: &sentinel_core::ingest::pg_stat::PrometheusPgStat,
    #[cfg(feature = "daemon")] config: &Config,
    top_n: usize,
    trace_counts: Option<&std::collections::HashMap<String, u64>>,
) -> Option<sentinel_core::ingest::pg_stat::PgStatReport> {
    if let Some(path) = path {
        return Some(load_pg_stat_from_file(path, top_n, trace_counts));
    }
    #[cfg(feature = "daemon")]
    {
        let url = prometheus?;
        let resolved_auth = resolve_pg_stat_auth_header(auth_header);
        Some(
            load_pg_stat_from_prometheus(
                url,
                config,
                top_n,
                opts,
                resolved_auth.as_deref(),
                trace_counts,
            )
            .await,
        )
    }
    #[cfg(not(feature = "daemon"))]
    {
        None
    }
}

/// Cross-reference (when trace counts are at hand), rank, and stamp the
/// matched share, so the `report` dashboard and the standalone runner
/// share one wiring.
pub(crate) fn rank_with_trace_match(
    entries: &mut [sentinel_core::ingest::pg_stat::PgStatEntry],
    top_n: usize,
    trace_counts: Option<&std::collections::HashMap<String, u64>>,
) -> sentinel_core::ingest::pg_stat::PgStatReport {
    use sentinel_core::ingest::pg_stat;
    if let Some(counts) = trace_counts {
        pg_stat::cross_reference_templates(entries, counts);
    }
    let mut report = pg_stat::rank_pg_stat(entries, top_n);
    if trace_counts.is_some() {
        report.trace_match = Some(pg_stat::trace_match_summary(entries));
    }
    report
}

/// Scrape a `postgres_exporter` endpoint one-shot and produce the
/// ranking report. Exits `EXIT_TOOLING_ERROR` on transport/parse
/// failure, `pg-stat` has no quality gate to breach.
#[cfg(feature = "daemon")]
pub(crate) async fn load_pg_stat_from_prometheus(
    url: &str,
    _config: &Config,
    top_n: usize,
    opts: &sentinel_core::ingest::pg_stat::PrometheusPgStat,
    auth_header: Option<&str>,
    trace_counts: Option<&std::collections::HashMap<String, u64>>,
) -> sentinel_core::ingest::pg_stat::PgStatReport {
    let scrape_budget = top_n.max(crate::PROMETHEUS_SCRAPE_FLOOR);
    match sentinel_core::ingest::pg_stat::fetch_from_prometheus(
        url,
        scrape_budget,
        auth_header,
        opts,
    )
    .await
    {
        Ok(mut entries) => rank_with_trace_match(&mut entries, top_n, trace_counts),
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
    baseline: Option<&std::path::Path>,
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

    run_pg_stat_pipeline(entries, top_n, traces, baseline, &config, format);
}

/// Variant of `cmd_pg_stat` that takes already-parsed entries (from Prometheus scrape).
#[cfg(feature = "daemon")]
fn cmd_pg_stat_from_entries(
    entries: Vec<sentinel_core::ingest::pg_stat::PgStatEntry>,
    top_n: usize,
    traces: Option<&std::path::Path>,
    baseline: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    format: PgStatOutputFormat,
) {
    let config = load_config(config_path);
    run_pg_stat_pipeline(entries, top_n, traces, baseline, &config, format);
}

/// Shared pipeline for the two `pg-stat` entry points (file input and
/// Prometheus scrape): optional trace cross-reference, ranking, then
/// text or JSON output. Extracted to avoid duplicating the 20+ lines
/// between `cmd_pg_stat` and `cmd_pg_stat_from_entries`.
fn run_pg_stat_pipeline(
    mut entries: Vec<sentinel_core::ingest::pg_stat::PgStatEntry>,
    top_n: usize,
    traces: Option<&std::path::Path>,
    baseline: Option<&std::path::Path>,
    config: &Config,
    format: PgStatOutputFormat,
) {
    // Cross-reference with the traced SQL templates if --traces is
    // provided: every span counts, not only the ones that produced a
    // finding, so a healthy traced query still gets its marker.
    // `--baseline` is `requires = "traces"` at the clap level, so a
    // baseline with no counts means the trace ingest is what failed.
    let trace_counts =
        traces.and_then(|path| crate::trace_counts_for_cross_reference(path, config));
    if trace_counts.is_none() && baseline.is_some() {
        eprintln!("Warning: --baseline ignored, the trace cross-reference failed");
    }

    let mut report = rank_with_trace_match(&mut entries, top_n, trace_counts.as_ref());
    if let Some(counts) = &trace_counts {
        report.trace_coverage = load_trace_coverage(&entries, baseline, counts);
    }

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

/// Parse the `--baseline` snapshot and compute the empirical coverage.
/// A parse failure warns and returns `None`: the ranking is still worth
/// printing without the coverage figure.
fn load_trace_coverage(
    entries: &[sentinel_core::ingest::pg_stat::PgStatEntry],
    baseline: Option<&std::path::Path>,
    trace_counts: &std::collections::HashMap<String, u64>,
) -> Option<sentinel_core::ingest::pg_stat::TraceCoverage> {
    let baseline_path = baseline?;
    let raw = read_file_capped(
        baseline_path,
        u64::try_from(limits::MAX_BATCH_INPUT_BYTES).unwrap_or(u64::MAX),
    );
    match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw, limits::MAX_BATCH_INPUT_BYTES) {
        Ok(baseline_entries) => Some(sentinel_core::ingest::pg_stat::trace_coverage(
            entries,
            &baseline_entries,
            trace_counts,
        )),
        Err(e) => {
            eprintln!(
                "Warning: failed to parse --baseline {}: {}",
                baseline_path.display(),
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            None
        }
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
    if let Some(tm) = &report.trace_match {
        // Spelled out rather than labelled: the share of statements and the
        // share of calls are different figures, and a reader who meets
        // "trace-matched" cold has nothing to anchor either to. Still never
        // "coverage", hence the second line: pg_stat counters are cumulative
        // since the last stats reset while the traces cover one window, so
        // this understates tracing instead of measuring a sampling rate.
        println!(
            "{dim}Also seen in the traces: {} of {} statement(s) here, \
             accounting for {:.1}% of the calls the database counted.{reset}",
            tm.matched_templates,
            tm.total_templates,
            tm.calls_share_percent()
        );
        println!(
            "{dim}A floor, not a sampling rate: the database counts since its \
             last statistics reset, the traces cover one capture window.{reset}"
        );
    }
    if let Some(cov) = &report.trace_coverage {
        match cov.coverage_percent() {
            Some(pct) => println!(
                "{dim}Empirical coverage:{reset} {} of {} executed calls traced ({:.1}%) \
                 across {} template(s)",
                cov.traced_calls, cov.executed_calls, pct, cov.matched_templates
            ),
            None => println!(
                "{dim}Empirical coverage:{reset} no call executed on the traced \
                 templates between the two snapshots"
            ),
        }
        if cov.reset_templates > 0 {
            println!(
                "{yellow}Warning:{reset} {} template(s) skipped, their counters went \
                 backwards between the snapshots (statistics reset), the coverage \
                 figure is unreliable",
                cov.reset_templates
            );
        }
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
