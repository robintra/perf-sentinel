#![warn(clippy::pedantic)]
#![allow(clippy::too_many_lines)] // print_colored_report is long but straightforward
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 for elapsed_ms, f64 -> usize for percentile index
#![allow(clippy::cast_precision_loss)] // usize -> f64 for throughput and latency computation
#![allow(clippy::cast_sign_loss)] // i64 (libc::ru_maxrss) -> usize for RSS bytes on macOS
#![allow(clippy::items_after_statements)]
// bench report struct defined near its use
// `Commands` is parsed once at startup and dropped after the dispatch, so the
// gap between `Report` (the flag-heaviest subcommand) and the rest costs one
// stack frame, not memory held for the run.
#![allow(clippy::large_enum_variant)]

// See `docs/design/07-CLI-CONFIG-RELEASE.md` § "Allocator on musl builds".
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "daemon")]
mod ack;
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
mod backend_cmd;
mod bench;
#[cfg(feature = "daemon")]
mod capture;
mod cli;
mod config_load;
mod demo;
mod disclose;
mod hash_bake;
mod limits;
#[cfg(all(feature = "daemon", feature = "tui"))]
mod monitor;
mod mysql_stat;
mod pg_stat;
#[cfg(feature = "daemon")]
mod query;
mod render;
#[cfg(feature = "tui")]
mod tui;
#[cfg(feature = "tui")]
mod tui_launch;
#[cfg(feature = "tui")]
mod tui_resize;
mod verify_hash;

use clap::{CommandFactory, Parser};
pub(crate) use cli::{
    Cli, Commands, ExplainFormat, MySqlStatOutputFormat, OutputFormat, PgStatOutputFormat,
};
#[cfg(feature = "daemon")]
pub(crate) use cli::{QueryAction, QueryOutputFormat};
use config_load::load_config;
#[cfg(feature = "daemon")]
use config_load::load_config_with_flags;
use render::emit_report_and_gate;
use sentinel_core::config::Config;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;
use std::io::Read;
use std::path::PathBuf;
use tracing::info;

/// Exit code for a runtime tooling/internal failure while producing a
/// report: a missing or unreadable input file, malformed
/// trace/config/acknowledgments data, or a failure writing the
/// SARIF/JSON output. Distinct from `1` (a quality-gate breach,
/// `analyze --ci` exceeding a `[thresholds]` limit) and from `2` (a CLI
/// usage error). CI pipelines can branch on this exit code directly
/// instead of inferring the distinction from file existence or step
/// outcome, see docs/CI.md "Exit codes" and "Tooling failures vs
/// quality-gate breaches".
///
/// The value `75` is a fixed sentinel, chosen to match the `|| exit 75`
/// the GitLab CI template already uses at the shell level (its numeric
/// origin is sysexits.h's `EX_TEMPFAIL`). perf-sentinel emits it for
/// permanent failures too, so it is NOT a request to retry: treat it as
/// "perf-sentinel could not run", not "try again later".
pub(crate) const EXIT_TOOLING_ERROR: i32 = 75;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    dispatch_command(Cli::parse().command).await;
}

/// Render the root man page plus one page per subcommand to `out`, so
/// tuning documented only in a subcommand's long help (e.g. the `[daemon]`
/// queue knobs on `watch`) is discoverable from `man` as well as `--help`.
/// The root page alone lists subcommands by short description only.
fn render_man(out: &mut impl std::io::Write) -> std::io::Result<()> {
    let cmd = Cli::command();
    let mut pages = vec![cmd.clone()];
    for sub in cmd.get_subcommands() {
        if sub.get_name() != "help" {
            pages.push(sub.clone());
        }
    }
    for page in pages {
        clap_mangen::Man::new(page).render(out)?;
    }
    Ok(())
}

/// Dispatch a parsed CLI command to its handler. Kept out of `main()`
/// so the binary entry point stays focused on tracing init and parsing.
async fn dispatch_command(command: Commands) {
    match command {
        Commands::Analyze {
            input,
            config,
            ci,
            format,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
            sort,
            #[cfg(feature = "tui")]
            tui,
        } => {
            #[cfg(feature = "tui")]
            if tui {
                tui_launch::cmd_analyze_tui(
                    input.as_deref(),
                    config.as_deref(),
                    acknowledgments.as_deref(),
                    no_acknowledgments,
                    sort,
                );
                return;
            }
            cmd_analyze(
                input.as_deref(),
                config.as_deref(),
                ci,
                format,
                acknowledgments.as_deref(),
                no_acknowledgments,
                show_acknowledged,
                sort,
            );
        }
        Commands::Explain {
            input,
            trace_id,
            config,
            format,
            #[cfg(feature = "tui")]
            tui,
        } => {
            #[cfg(feature = "tui")]
            if tui {
                tui_launch::cmd_explain_tui(&input, &trace_id, config.as_deref());
                return;
            }
            cmd_explain(&input, &trace_id, config.as_deref(), format);
        }
        #[cfg(feature = "daemon")]
        Commands::Watch {
            config,
            listen_address,
            listen_port_http,
            listen_port_grpc,
            max_export_findings,
        } => {
            cmd_watch(
                config.as_deref(),
                listen_address,
                listen_port_http,
                listen_port_grpc,
                max_export_findings,
            )
            .await;
        }
        #[cfg(feature = "daemon")]
        Commands::Capture {
            output,
            listen_address,
            listen_port_grpc,
            listen_port_http,
            max_file_size,
            grace_ms,
            command,
        } => {
            let code = capture::cmd_capture(
                &output,
                listen_address,
                listen_port_grpc,
                listen_port_http,
                max_file_size,
                grace_ms,
                &command,
            )
            .await;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Commands::Demo {
            config,
            html,
            #[cfg(feature = "tui")]
            tui,
        } => demo::cmd_demo(
            config.as_deref(),
            html.as_deref(),
            #[cfg(feature = "tui")]
            tui,
        ),
        Commands::Bench {
            input,
            iterations,
            synthetic_events,
            services,
            seed,
        } => bench::cmd_bench(
            input.as_deref(),
            iterations,
            synthetic_events,
            services,
            seed,
        ),
        #[cfg(feature = "tui")]
        Commands::Inspect {
            input,
            config,
            acknowledgments,
            no_acknowledgments,
        } => tui_launch::cmd_inspect(
            &input,
            config.as_deref(),
            acknowledgments.as_deref(),
            no_acknowledgments,
        ),
        #[cfg(feature = "tempo")]
        Commands::Tempo {
            endpoint,
            trace_id,
            service,
            lookback,
            window,
            max_traces,
            auth_header,
            auth_header_env,
            config,
            sort,
            format,
            ci,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
        } => {
            let resolved_auth = resolve_auth_header_or_exit(auth_header, auth_header_env);
            backend_cmd::cmd_backend_query(
                backend_cmd::QueryBackend::Tempo,
                &endpoint,
                trace_id.as_deref(),
                service.as_deref(),
                &lookback,
                window.from.as_deref(),
                window.to.as_deref(),
                max_traces as usize,
                resolved_auth.as_deref(),
                config.as_deref(),
                sort.sort,
                format,
                ci,
                acknowledgments.as_deref(),
                no_acknowledgments,
                show_acknowledged,
            )
            .await;
        }
        #[cfg(feature = "jaeger-query")]
        Commands::JaegerQuery {
            endpoint,
            trace_id,
            service,
            lookback,
            window,
            max_traces,
            auth_header,
            auth_header_env,
            config,
            sort,
            format,
            ci,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
        } => {
            let resolved_auth = resolve_auth_header_or_exit(auth_header, auth_header_env);
            backend_cmd::cmd_backend_query(
                backend_cmd::QueryBackend::JaegerQuery,
                &endpoint,
                trace_id.as_deref(),
                service.as_deref(),
                &lookback,
                window.from.as_deref(),
                window.to.as_deref(),
                max_traces as usize,
                resolved_auth.as_deref(),
                config.as_deref(),
                sort.sort,
                format,
                ci,
                acknowledgments.as_deref(),
                no_acknowledgments,
                show_acknowledged,
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
            #[cfg(feature = "daemon")]
            auth_header,
            #[cfg(feature = "daemon")]
            metric,
            #[cfg(feature = "daemon")]
            query_label,
            #[cfg(feature = "daemon")]
            calls_metric,
            #[cfg(feature = "daemon")]
            unit,
            top_n,
            traces,
            baseline,
            config,
            format,
        } => {
            pg_stat::dispatch_pg_stat(
                input.as_deref(),
                #[cfg(feature = "daemon")]
                prometheus.as_deref(),
                #[cfg(feature = "daemon")]
                auth_header,
                #[cfg(feature = "daemon")]
                &pg_stat_prometheus_opts(metric, query_label, calls_metric, unit.as_deref()),
                top_n,
                traces.as_deref(),
                baseline.as_deref(),
                config.as_deref(),
                format,
            )
            .await;
        }
        Commands::MySqlStat {
            input,
            #[cfg(feature = "daemon")]
            prometheus,
            #[cfg(feature = "daemon")]
            auth_header,
            #[cfg(feature = "daemon")]
            metric,
            #[cfg(feature = "daemon")]
            query_label,
            #[cfg(feature = "daemon")]
            schema_label,
            #[cfg(feature = "daemon")]
            calls_metric,
            #[cfg(feature = "daemon")]
            rows_sent_metric,
            #[cfg(feature = "daemon")]
            rows_examined_metric,
            #[cfg(feature = "daemon")]
            unit,
            top_n,
            traces,
            config,
            format,
        } => {
            #[cfg(feature = "daemon")]
            let opts = mysql_stat_prometheus_opts(
                metric,
                query_label,
                schema_label,
                MysqlCounterMetrics {
                    calls: calls_metric,
                    rows_sent: rows_sent_metric,
                    rows_examined: rows_examined_metric,
                },
                unit.as_deref(),
            );
            mysql_stat::dispatch_mysql_stat(
                input.as_deref(),
                #[cfg(feature = "daemon")]
                prometheus.as_deref(),
                #[cfg(feature = "daemon")]
                auth_header,
                #[cfg(feature = "daemon")]
                &opts,
                top_n,
                traces.as_deref(),
                config.as_deref(),
                format,
            )
            .await;
        }
        #[cfg(feature = "daemon")]
        Commands::Query { daemon, action } => {
            // `main` is `#[tokio::main]`, so await the async command
            // directly. A nested `Runtime::new().block_on(...)` here
            // panics with "Cannot start a runtime from within a runtime."
            query::cmd_query(&daemon, action).await;
        }
        #[cfg(feature = "daemon")]
        Commands::Ack { daemon, action } => {
            let exit_code = ack::cmd_ack(&daemon, action).await;
            std::process::exit(exit_code);
        }
        Commands::Diff {
            before,
            after,
            config,
            format,
            output,
            acknowledgments,
            no_acknowledgments,
        } => cmd_diff(
            &before,
            &after,
            config.as_deref(),
            format,
            output.as_deref(),
            acknowledgments.as_deref(),
            no_acknowledgments,
        ),
        Commands::Report {
            input,
            config,
            output,
            max_traces_embedded,
            sort,
            pg_stat,
            #[cfg(feature = "daemon")]
            pg_stat_prometheus,
            #[cfg(feature = "daemon")]
            pg_stat_auth_header,
            #[cfg(feature = "daemon")]
            pg_stat_metric,
            #[cfg(feature = "daemon")]
            pg_stat_calls_metric,
            #[cfg(feature = "daemon")]
            pg_stat_unit,
            #[cfg(feature = "daemon")]
            pg_stat_query_label,
            before,
            pg_stat_top,
            mysql_stat,
            #[cfg(feature = "daemon")]
            mysql_stat_prometheus,
            #[cfg(feature = "daemon")]
            mysql_stat_auth_header,
            #[cfg(feature = "daemon")]
            mysql_stat_metric,
            #[cfg(feature = "daemon")]
            mysql_stat_query_label,
            #[cfg(feature = "daemon")]
            mysql_stat_schema_label,
            #[cfg(feature = "daemon")]
            mysql_stat_calls_metric,
            #[cfg(feature = "daemon")]
            mysql_stat_rows_sent_metric,
            #[cfg(feature = "daemon")]
            mysql_stat_rows_examined_metric,
            #[cfg(feature = "daemon")]
            mysql_stat_unit,
            mysql_stat_top,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
            #[cfg(feature = "daemon")]
            daemon_url,
        } => {
            #[cfg(feature = "daemon")]
            let daemon_url = validate_daemon_url_or_exit(daemon_url);
            #[cfg(feature = "daemon")]
            let pg_stat_prom = pg_stat_prometheus_opts(
                pg_stat_metric,
                pg_stat_query_label,
                pg_stat_calls_metric,
                pg_stat_unit.as_deref(),
            );
            #[cfg(feature = "daemon")]
            let mysql_stat_prom = mysql_stat_prometheus_opts(
                mysql_stat_metric,
                mysql_stat_query_label,
                mysql_stat_schema_label,
                MysqlCounterMetrics {
                    calls: mysql_stat_calls_metric,
                    rows_sent: mysql_stat_rows_sent_metric,
                    rows_examined: mysql_stat_rows_examined_metric,
                },
                mysql_stat_unit.as_deref(),
            );
            cmd_report(
                input.as_deref(),
                config.as_deref(),
                &output,
                max_traces_embedded,
                sort,
                pg_stat.as_deref(),
                #[cfg(feature = "daemon")]
                pg_stat_prometheus.as_deref(),
                #[cfg(feature = "daemon")]
                pg_stat_auth_header,
                #[cfg(feature = "daemon")]
                &pg_stat_prom,
                before.as_deref(),
                // `try_from` over `as` so a 16-bit target drops the
                // flag instead of truncating silently. No supported
                // build has `usize < 32` bits, so the only effect is
                // to keep the cast checked.
                pg_stat_top.and_then(|n| usize::try_from(n).ok()),
                mysql_stat.as_deref(),
                #[cfg(feature = "daemon")]
                mysql_stat_prometheus.as_deref(),
                #[cfg(feature = "daemon")]
                mysql_stat_auth_header,
                #[cfg(feature = "daemon")]
                &mysql_stat_prom,
                mysql_stat_top.and_then(|n| usize::try_from(n).ok()),
                acknowledgments.as_deref(),
                no_acknowledgments,
                show_acknowledged,
                #[cfg(feature = "daemon")]
                daemon_url,
            )
            .await;
        }
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "perf-sentinel", &mut std::io::stdout());
        }
        Commands::Man => {
            if let Err(err) = render_man(&mut std::io::stdout()) {
                eprintln!("Error: failed to render man page: {err}");
                std::process::exit(EXIT_TOOLING_ERROR);
            }
        }
        Commands::Disclose {
            intent,
            confidentiality,
            period_type,
            from,
            to,
            input,
            output,
            org_config,
            strict_attribution,
            emit_attestation,
            #[cfg(feature = "tui")]
            tui,
        } => {
            #[cfg(feature = "tui")]
            if tui {
                tui_launch::cmd_disclose_tui(input, &org_config, strict_attribution);
                std::process::exit(0);
            }
            // Canonical path: clap requires these whenever `--tui` is absent.
            let code = disclose::cmd_disclose(
                intent.expect("--intent is required without --tui"),
                confidentiality.expect("--confidentiality is required without --tui"),
                period_type.expect("--period-type is required without --tui"),
                from.expect("--from is required without --tui"),
                to.expect("--to is required without --tui"),
                &input,
                output
                    .as_deref()
                    .expect("--output is required without --tui"),
                &org_config,
                strict_attribution,
                emit_attestation.as_deref(),
            );
            std::process::exit(code);
        }
        Commands::VerifyHash {
            report,
            url,
            attestation,
            bundle,
            verify_binary,
            format,
            expected_identity,
            expected_issuer,
            no_identity_check,
        } => {
            let identity = verify_hash::IdentityOptions {
                expected_identity,
                expected_issuer,
                no_identity_check,
            };
            let code = verify_hash::cmd_verify_hash(
                report.as_deref(),
                url.as_deref(),
                attestation.as_deref(),
                bundle.as_deref(),
                verify_binary.as_deref(),
                format,
                &identity,
            )
            .await;
            std::process::exit(code);
        }
        Commands::HashBake {
            report,
            output,
            allow_signed,
        } => {
            let code = hash_bake::cmd_hash_bake(&report, &output, allow_signed);
            std::process::exit(code);
        }
    }
}

/// Resolve the final auth header string from the two mutually
/// exclusive CLI flags. clap already rejects the "both set" case via
/// `conflicts_with`. This helper only handles "neither / one / other"
/// and reads the env var when `--auth-header-env` is used.
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
fn resolve_auth_header(
    direct: Option<String>,
    env_var: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(value) = direct {
        // `--auth-header` is `ps`-visible. Nudge operators toward
        // `--auth-header-env` to match the pg-stat helper UX.
        tracing::warn!(
            "auth header supplied via --auth-header is visible in `ps` and shell history; \
             prefer --auth-header-env <NAME> to read it from an environment variable"
        );
        return Ok(Some(value));
    }
    if let Some(name) = env_var {
        return match std::env::var(&name) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(format!(
                "cannot read --auth-header-env variable '{name}': {e}"
            )),
        };
    }
    Ok(None)
}

/// Build the search window for `tempo` and `jaeger-query` from their flags.
///
/// clap already enforces that `--from` and `--to` arrive as a pair and that
/// neither joins `--lookback`, so only parsing is left. Absolute bounds are
/// preferred when present because they do not drift between the moment a
/// caller decides on a window and the moment the request is issued.
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
fn resolve_search_window_or_exit(
    lookback: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> sentinel_core::ingest::lookback::SearchWindow {
    use sentinel_core::ingest::lookback::{self, SearchWindow};

    let parsed = match (from, to) {
        (Some(from), Some(to)) => SearchWindow::from_iso8601(from, to)
            .map_err(|e| format!("Error parsing --from {from} --to {to}: {e}")),
        (None, None) => lookback::parse(lookback)
            .map(SearchWindow::Lookback)
            .map_err(|e| format!("Error parsing lookback: {e}")),
        // clap's `requires` makes a half pair unreachable, but falling back to
        // the lookback here would silently ignore a flag the operator did pass.
        _ => Err("Error: --from and --to must be given together".to_string()),
    };

    parsed.unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(EXIT_TOOLING_ERROR);
    })
}

/// Resolve the auth header or exit on error. Used by Tempo and
/// Jaeger-Query dispatch arms which both share the same fail-fast
/// shape (`Err(e)` -> `eprintln!("Error: {e}")` -> `EXIT_TOOLING_ERROR`):
/// a malformed `--auth-header`/`--auth-header-env` value is an
/// invocation error, never a quality-gate breach.
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
fn resolve_auth_header_or_exit(direct: Option<String>, env_var: Option<String>) -> Option<String> {
    resolve_auth_header(direct, env_var).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(EXIT_TOOLING_ERROR);
    })
}

/// Assemble the `pg_stat` scrape options. Shared by `pg-stat --prometheus` and
/// `report --pg-stat-prometheus`, which run the same scrape and must not drift
/// on what their flags mean.
///
/// Defaults describe the `postgres_exporter` built-in query. An exporter
/// running its own SQL names its own columns. An empty `calls_metric` means
/// "do not run the second query", which is how an operator opts out of the
/// join when their exporter has no call counter at all.
#[cfg(feature = "daemon")]
fn pg_stat_prometheus_opts(
    metric: Option<String>,
    query_label: Option<String>,
    calls_metric: Option<String>,
    unit: Option<&str>,
) -> sentinel_core::ingest::pg_stat::PrometheusPgStat {
    use sentinel_core::ingest::pg_stat::{PgStatTimeUnit, PrometheusPgStat};
    let mut opts = PrometheusPgStat::with_overrides(metric, query_label);
    if let Some(series) = calls_metric {
        opts.calls_series = (!series.is_empty()).then_some(series);
    }
    if unit == Some("milliseconds") {
        opts.unit = PgStatTimeUnit::Milliseconds;
    }
    opts
}

/// The three counter series `mysql-stat` joins, as an operator named them.
/// Grouped so the two entry points pass one value rather than three in an
/// order nothing checks.
#[cfg(feature = "daemon")]
struct MysqlCounterMetrics {
    calls: Option<String>,
    rows_sent: Option<String>,
    rows_examined: Option<String>,
}

/// Same reasoning for the `mysqld_exporter` collector: a recording rule renames
/// the series, so neither name is ours to assume, and an empty `calls_metric`
/// opts out of the second query.
#[cfg(feature = "daemon")]
fn mysql_stat_prometheus_opts(
    metric: Option<String>,
    query_label: Option<String>,
    schema_label: Option<String>,
    counters: MysqlCounterMetrics,
    unit: Option<&str>,
) -> sentinel_core::ingest::mysql_stat::PrometheusMySqlStat {
    use sentinel_core::ingest::mysql_stat::{MySqlStatTimeUnit, PrometheusMySqlStat};
    let mut opts = PrometheusMySqlStat::with_overrides(metric, query_label);
    if let Some(label) = schema_label {
        opts.schema_label = label;
    }
    // An empty value means "do not run that query", the opt-out for an
    // exporter that publishes no such counter.
    let named = |series: Option<String>| series.map(|s| (!s.is_empty()).then_some(s));
    if let Some(series) = named(counters.calls) {
        opts.calls_series = series;
    }
    if let Some(series) = named(counters.rows_sent) {
        opts.rows_sent_series = series;
    }
    if let Some(series) = named(counters.rows_examined) {
        opts.rows_examined_series = series;
    }
    match unit {
        Some("milliseconds") => opts.unit = MySqlStatTimeUnit::Milliseconds,
        Some("picoseconds") => opts.unit = MySqlStatTimeUnit::Picoseconds,
        _ => {}
    }
    opts
}

/// Validates `report --daemon-url`. Exits `EXIT_TOOLING_ERROR` on a
/// malformed URL: `report` has no quality gate, so an invocation error
/// here is never a threshold breach.
#[cfg(feature = "daemon")]
fn validate_daemon_url_or_exit(raw: Option<String>) -> Option<String> {
    match raw {
        Some(s) => match ack::validate_url(&s) {
            Ok(normalized) => Some(normalized),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(EXIT_TOOLING_ERROR);
            }
        },
        None => None,
    }
}

/// The `--input` path, or exit 2 when the subcommand was given no source.
///
/// Exit 2 is clap's usage-error code: no source at all is a permanent
/// invocation mistake, not the tolerable 75 tooling bucket. Same reasoning as
/// the `--pg-stat-top` pairing check in `cmd_report`. Shared by `pg-stat` and
/// `mysql-stat`, whose message differs only by the flags they accept.
pub(crate) fn require_input_path(input: Option<&std::path::Path>) -> &std::path::Path {
    input.unwrap_or_else(|| {
        #[cfg(feature = "daemon")]
        eprintln!("Error: either --input or --prometheus is required");
        #[cfg(not(feature = "daemon"))]
        eprintln!("Error: --input is required");
        std::process::exit(2);
    })
}

/// Read a file into memory, capping the byte count at `max_size`.
/// Exits with `EXIT_TOOLING_ERROR` on any IO error or if the file exceeds
/// the cap: a missing or oversized file is never a quality-gate breach.
///
/// Uses the `.take(max + 1).read_to_end(&mut buf)` pattern to close the
/// TOCTOU window between `metadata().len()` and `fs::read()`, and to
/// correctly cap special files (FIFOs, `/dev/stdin`-style symlinks,
/// block devices) whose metadata reports 0 bytes. Shared by the trace
/// file reader and the calibrate energy-CSV reader so the capped-read
/// logic lives in one place.
fn read_file_capped(path: &std::path::Path, max_size: u64) -> Vec<u8> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error reading {}: {e}", path.display());
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    };
    // Metadata pre-check: reject oversized regular files without reading
    // them. The take() below stays as the defense for special files
    // whose metadata reports a wrong size (pipes, device files).
    if let Ok(meta) = file.metadata()
        && meta.is_file()
        && meta.len() > max_size
    {
        eprintln!(
            "Error: file {} exceeds maximum of {max_size} bytes",
            path.display()
        );
        std::process::exit(EXIT_TOOLING_ERROR);
    }
    let mut buf = Vec::new();
    if let Err(e) = file.take(max_size + 1).read_to_end(&mut buf) {
        eprintln!("Error reading {}: {e}", path.display());
        std::process::exit(EXIT_TOOLING_ERROR);
    }
    if buf.len() as u64 > max_size {
        eprintln!(
            "Error: file {} exceeds maximum of {max_size} bytes",
            path.display()
        );
        std::process::exit(EXIT_TOOLING_ERROR);
    }
    buf
}

#[allow(clippy::option_if_let_else)] // if/else with process::exit is clearer than map_or_else
fn read_events(input: Option<&std::path::Path>, max_size: usize) -> Vec<u8> {
    if let Some(path) = input {
        info!("Reading trace file: {}", path.display());
        read_file_capped(path, max_size as u64)
    } else {
        info!("Reading traces from stdin");
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin()
            .take(max_size as u64 + 1)
            .read_to_end(&mut buf)
        {
            eprintln!("Error reading stdin: {e}");
            std::process::exit(EXIT_TOOLING_ERROR);
        }
        if buf.len() > max_size {
            eprintln!("Error: stdin payload exceeds maximum of {max_size} bytes");
            std::process::exit(EXIT_TOOLING_ERROR);
        }
        buf
    }
}

/// Configured grouping attributes as the ingest layer wants them.
pub(crate) fn grouping_keys(config: &Config) -> Vec<std::sync::Arc<str>> {
    config
        .detection
        .grouping_attributes
        .iter()
        .map(|k| std::sync::Arc::from(k.as_str()))
        .collect()
}

/// Parse raw bytes as JSON trace events, printing a clear error and
/// exiting with `EXIT_TOOLING_ERROR` on failure: malformed or corrupted
/// trace input is a tooling problem, not a quality-gate breach. Shared
/// across all CLI subcommands that ingest trace files.
fn ingest_json_or_exit(
    raw: &[u8],
    max_size: usize,
    config: &Config,
) -> (
    Vec<sentinel_core::event::SpanEvent>,
    Option<sentinel_core::ingest::otlp::SpanConversionStats>,
) {
    let ingest = JsonIngest::new(max_size).with_grouping_attributes(grouping_keys(config));
    match ingest.ingest_with_stats(raw) {
        Ok(events_and_stats) => events_and_stats,
        Err(e) => {
            eprintln!("Error ingesting events: {e}");
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    }
}

/// Default location of the user's acknowledgments file.
const DEFAULT_ACKNOWLEDGMENTS_PATH: &str = ".perf-sentinel-acknowledgments.toml";

/// Resolve the acknowledgments path: explicit override wins, otherwise
/// fall back to `./.perf-sentinel-acknowledgments.toml` in the cwd.
fn resolve_acknowledgments_path(override_path: Option<&std::path::Path>) -> PathBuf {
    override_path.map_or_else(
        || PathBuf::from(DEFAULT_ACKNOWLEDGMENTS_PATH),
        std::path::Path::to_path_buf,
    )
}

/// Load the acknowledgments file and apply it to the report. No-op when
/// `no_acknowledgments` is set or when the file is absent. Exits with
/// `EXIT_TOOLING_ERROR` and a clean stderr message on parse failure: a
/// malformed acknowledgments file is a tooling problem, not a
/// quality-gate breach.
fn apply_acknowledgments_or_exit(
    report: &mut sentinel_core::report::Report,
    config: &Config,
    override_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    origin: sentinel_core::acknowledgments::ReportOrigin,
) {
    if no_acknowledgments {
        return;
    }
    let path = resolve_acknowledgments_path(override_path);
    let acks = sentinel_core::acknowledgments::load_from_file(&path).unwrap_or_else(|e| {
        eprintln!("Error loading acknowledgments {}: {e}", path.display());
        std::process::exit(EXIT_TOOLING_ERROR);
    });
    sentinel_core::acknowledgments::apply_to_report(
        report,
        &acks,
        config,
        chrono::Utc::now(),
        origin,
    );
}

#[allow(clippy::too_many_arguments)]
fn cmd_analyze(
    input: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    ci: bool,
    format: Option<OutputFormat>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
    sort: Option<render::FindingsSort>,
) {
    let config = load_config(config_path);
    let raw = read_events(input, limits::MAX_BATCH_INPUT_BYTES);

    let (events, ingest_stats) = ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES, &config);
    // Free the raw bytes before analysis: holding a multi-hundred-MB
    // input buffer through the whole pipeline doubles peak RSS.
    drop(raw);

    let mut report = pipeline::analyze_with_traces(events, &config, ingest_stats).0;
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
        sentinel_core::acknowledgments::ReportOrigin::FreshAnalysis,
    );
    // No embed here, unlike tempo and jaeger-query: their JSON is the only
    // carrier of the spans they fetched. `analyze` reads a local file the
    // user still holds, and `report --input <that file>` draws the trees
    // from it directly. Embedding would also fatten every `--ci`
    // pipeline's stdout on a version bump, for output nothing reads.
    emit_report_and_gate(
        &mut report,
        format,
        ci,
        "report",
        sort,
        None,
        show_acknowledged,
    );
}

fn cmd_diff(
    before: &std::path::Path,
    after: &std::path::Path,
    config_path: Option<&std::path::Path>,
    format: Option<OutputFormat>,
    output: Option<&std::path::Path>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) {
    let config = load_config(config_path);
    // Run analyze on both trace files with the SAME config so per-endpoint
    // counts and severity assignments are comparable.
    let before_raw = read_events(Some(before), limits::MAX_BATCH_INPUT_BYTES);
    let (before_events, before_stats) =
        ingest_json_or_exit(&before_raw, limits::MAX_BATCH_INPUT_BYTES, &config);
    drop(before_raw);
    let mut before_report = pipeline::analyze_with_traces(before_events, &config, before_stats).0;

    let after_raw = read_events(Some(after), limits::MAX_BATCH_INPUT_BYTES);
    let (after_events, after_stats) =
        ingest_json_or_exit(&after_raw, limits::MAX_BATCH_INPUT_BYTES, &config);
    drop(after_raw);
    let mut after_report = pipeline::analyze_with_traces(after_events, &config, after_stats).0;

    // Apply the same ack file to both runs so the diff stays meaningful:
    // an ack present on both sides masks the finding from both, an ack
    // landing between base and PR masks it from the after run only.
    apply_acknowledgments_or_exit(
        &mut before_report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
        sentinel_core::acknowledgments::ReportOrigin::FreshAnalysis,
    );
    apply_acknowledgments_or_exit(
        &mut after_report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
        sentinel_core::acknowledgments::ReportOrigin::FreshAnalysis,
    );

    let diff = sentinel_core::diff::diff_runs(&before_report, &after_report);
    if let Err(e) = render::emit_diff(&diff, format, output) {
        eprintln!("Error writing diff: {e}");
        std::process::exit(EXIT_TOOLING_ERROR);
    }
}

/// Return `true` when the `--input` argument asks for stdin, either
/// explicitly (`--input -`) or implicitly (flag omitted). `analyze`
/// accepts only the omitted form, `report` accepts both for shell
/// composability (`tempo --output - | report --input - --output ...`).
fn is_stdin_input(input: Option<&std::path::Path>) -> bool {
    input.is_none_or(|p| p == "-")
}

/// Best-effort display label for the top bar of the HTML dashboard.
/// Prefers the file name, falls back to the full path, finally to `-`.
fn input_label_for(input: Option<&std::path::Path>, stdin_mode: bool) -> String {
    if stdin_mode {
        return "-".to_string();
    }
    input
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| input.map(|p| p.display().to_string()))
        .unwrap_or_else(|| "-".to_string())
}

/// Strip a UTF-8 BOM prefix if present. Windows editors (Notepad, some
/// VS Code flows) save with a leading `EF BB BF`, and the byte-peek
/// auto-detect below would otherwise reject a valid payload.
fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw)
}

/// Parse a pre-computed `Report` JSON from stdin or a file, used by
/// `report --before <baseline>` to load the baseline for the Diff tab.
/// Enforces the same 32-level nesting cap the trace-event ingest
/// applies. Exits `EXIT_TOOLING_ERROR` on a depth overflow or a serde
/// error, both with a user-readable message: a corrupted or truncated
/// baseline artifact (a flaky download from a previous CI job, say) is
/// a tooling problem, never a quality-gate breach.
fn parse_report_json_or_exit(raw: &[u8], source_label: &str) -> sentinel_core::report::Report {
    if sentinel_core::ingest::json::exceeds_max_depth(raw) {
        eprintln!(
            "Error: {source_label} JSON exceeds maximum nesting depth of {}",
            sentinel_core::ingest::json::MAX_JSON_DEPTH
        );
        std::process::exit(EXIT_TOOLING_ERROR);
    }
    let mut report =
        serde_json::from_slice::<sentinel_core::report::Report>(raw).unwrap_or_else(|e| {
            eprintln!("Error parsing {source_label} as Report JSON: {e}");
            std::process::exit(EXIT_TOOLING_ERROR);
        });
    // Pre-0.5.17 baselines have no signature. Fill them in so ack
    // matching and copy-paste workflows behave the same as on a fresh run.
    sentinel_core::acknowledgments::enrich_with_signatures(&mut report.findings);
    report
}

/// Ingest a trace file and tally its SQL templates for the `pg-stat` /
/// `mysql-stat` cross-reference. `None` (after a warning) when the file
/// does not ingest: the ranking still prints without markers.
fn trace_counts_for_cross_reference(
    traces_path: &std::path::Path,
    config: &Config,
) -> Option<std::collections::HashMap<String, u64>> {
    let traces_raw = read_events(Some(traces_path), limits::MAX_BATCH_INPUT_BYTES);
    let ingest = JsonIngest::new(limits::MAX_BATCH_INPUT_BYTES)
        .with_grouping_attributes(grouping_keys(config));
    match ingest.ingest(&traces_raw) {
        Ok(events) => {
            let (_, analyzed_traces) = pipeline::analyze_with_traces(events, config, None);
            Some(pipeline::trace_sql_template_counts(&analyzed_traces))
        }
        Err(e) => {
            eprintln!(
                "Warning: failed to ingest trace file for cross-reference: {}",
                sentinel_core::text_safety::sanitize_for_terminal(&e.to_string())
            );
            None
        }
    }
}

/// Dispatch the `--input` payload by JSON shape. A top-level array goes
/// through the normalize/correlate/detect/score pipeline (native event
/// streams, Zipkin v2). A top-level object is first tried as a
/// pre-computed `Report` (daemon snapshot, baseline file) and falls
/// back to `JsonIngest`, which auto-detects OTLP/JSON and Jaeger.
/// Report-first guarantees a daemon snapshot is never misrouted to the
/// Jaeger ingest, even when its payload contains a `"data"` literal in
/// the first 4 KB. Report-first costs one extra Report parse on OTLP/Jaeger
/// inputs (rare through this CLI). An OTLP request can never parse
/// as a Report (its required fields are absent). The depth cap is enforced
/// before the Report parse so an over-deep Report does not silently
/// fall through to the ingest fallback. `report` accepts a wider set of
/// shapes than `analyze` and has no quality-gate concept, so every
/// failure branch (including empty input and scalar roots, each with a
/// distinct message) exits with `EXIT_TOOLING_ERROR`, never `1`.
fn load_report_from_input(
    raw: &[u8],
    config: &Config,
) -> (
    sentinel_core::report::Report,
    Vec<sentinel_core::correlate::Trace>,
    sentinel_core::acknowledgments::ReportOrigin,
) {
    use sentinel_core::acknowledgments::ReportOrigin;
    let fresh = |(report, traces)| (report, traces, ReportOrigin::FreshAnalysis);
    let first_byte = raw.iter().find(|b| !b.is_ascii_whitespace()).copied();
    match first_byte {
        Some(b'[') => {
            // A top-level array is native or Zipkin, formats with no OTLP
            // filter tally, so the stats half is always None here.
            let (events, ingest_stats) =
                ingest_json_or_exit(raw, limits::MAX_BATCH_INPUT_BYTES, config);
            fresh(pipeline::analyze_with_traces(events, config, ingest_stats))
        }
        Some(b'{') => {
            if sentinel_core::ingest::json::exceeds_max_depth(raw) {
                eprintln!(
                    "Error: --input JSON exceeds maximum nesting depth of {}",
                    sentinel_core::ingest::json::MAX_JSON_DEPTH
                );
                std::process::exit(EXIT_TOOLING_ERROR);
            }
            if let Ok(mut report) = serde_json::from_slice::<sentinel_core::report::Report>(raw) {
                sentinel_core::acknowledgments::enrich_with_signatures(&mut report.findings);
                return (report, Vec::new(), ReportOrigin::Precomputed);
            }
            let ingest = JsonIngest::new(limits::MAX_BATCH_INPUT_BYTES)
                .with_grouping_attributes(grouping_keys(config));
            match ingest.ingest_with_stats(raw) {
                Ok((events, ingest_stats)) => {
                    fresh(pipeline::analyze_with_traces(events, config, ingest_stats))
                }
                Err(e) => {
                    eprintln!(
                        "Error: --input top-level object is neither a pre-computed Report JSON, an OTLP/JSON export, nor a Jaeger export. Underlying error: {e}"
                    );
                    std::process::exit(EXIT_TOOLING_ERROR);
                }
            }
        }
        None => {
            eprintln!("Error: --input is empty or whitespace-only");
            std::process::exit(EXIT_TOOLING_ERROR);
        }
        Some(_) => {
            eprintln!(
                "Error: --input must be a JSON array of events, a Jaeger \
                 export ({{\"data\": [...]}}) or a pre-computed Report \
                 object (got a scalar or unexpected token at the root)"
            );
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    }
}

/// Default top-N for `pg_stat` rankings inside the `report` subcommand
/// when the user does not set `--pg-stat-top`.
const DEFAULT_PG_STAT_TOP: usize = 10;

/// Lower bound on a Prometheus scrape when only a small `--top-n` is set.
/// `rank_pg_stat` emits four rankings keyed on different columns and only
/// one of them is keyed on the `topk` metric, so scraping just `top_n`
/// biases the others. Shared by the `pg-stat` and `mysql-stat` paths so the
/// two cannot drift.
#[cfg(feature = "daemon")]
pub(crate) const PROMETHEUS_SCRAPE_FLOOR: usize = 200;

/// Parse a saved baseline report and diff it against the current run.
/// Applies the same BOM strip and depth cap as `--input` in Report
/// mode. Exits `EXIT_TOOLING_ERROR` on failure, never a quality-gate
/// breach. The same acknowledgments file is applied to the baseline so
/// a finding acked on both sides drops out of the diff entirely (the
/// alternative would surface every ack as a fake "resolved in PR", a
/// noisy false positive).
fn load_diff_against_baseline(
    before_path: &std::path::Path,
    current: &sentinel_core::report::Report,
    config: &Config,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) -> sentinel_core::diff::DiffReport {
    let raw_before = read_file_capped(
        before_path,
        u64::try_from(limits::MAX_BATCH_INPUT_BYTES).unwrap_or(u64::MAX),
    );
    let slice = strip_bom(&raw_before);
    let source_label = format!("--before {}", before_path.display());
    let mut baseline = parse_report_json_or_exit(slice, &source_label);
    apply_acknowledgments_or_exit(
        &mut baseline,
        config,
        acknowledgments_path,
        no_acknowledgments,
        sentinel_core::acknowledgments::ReportOrigin::Precomputed,
    );
    sentinel_core::diff::diff_runs(&baseline, current)
}

/// Reject a `--*-stat-top` flag passed without the source it ranks.
///
/// Exit 2 matches clap's own usage-error code: this is an unsupported flag
/// combination clap's `requires` cannot express, not a runtime tooling
/// failure. A usage error is a permanent invocation mistake and must
/// always block, never fall into the `75` bucket a pipeline may tolerate.
/// See `docs/CI.md` "Exit codes".
fn require_stat_source_or_exit(top: Option<usize>, has_source: bool, requirement: &str) {
    if top.is_some() && !has_source {
        eprintln!("Error: {requirement}");
        std::process::exit(2);
    }
}

/// Post-parse validation of the `pg_stat` and `mysql_stat` source flags,
/// split out of `cmd_report` to keep it under the Sonar complexity gate.
/// Returns which of the two stat families has a source at all.
fn validate_stat_sources_or_exit(
    pg_stat_top: Option<usize>,
    pg_stat_path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] pg_stat_prometheus: Option<&str>,
    mysql_stat_top: Option<usize>,
    mysql_stat_path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] mysql_stat_prometheus: Option<&str>,
) -> (bool, bool) {
    #[cfg(feature = "daemon")]
    let has_pg_stat_source = pg_stat_path.is_some() || pg_stat_prometheus.is_some();
    #[cfg(not(feature = "daemon"))]
    let has_pg_stat_source = pg_stat_path.is_some();
    require_stat_source_or_exit(
        pg_stat_top,
        has_pg_stat_source,
        if cfg!(feature = "daemon") {
            "--pg-stat-top requires --pg-stat or --pg-stat-prometheus"
        } else {
            "--pg-stat-top requires --pg-stat"
        },
    );
    #[cfg(feature = "daemon")]
    let has_mysql_stat_source = mysql_stat_path.is_some() || mysql_stat_prometheus.is_some();
    #[cfg(not(feature = "daemon"))]
    let has_mysql_stat_source = mysql_stat_path.is_some();
    require_stat_source_or_exit(
        mysql_stat_top,
        has_mysql_stat_source,
        if cfg!(feature = "daemon") {
            "--mysql-stat-top requires --mysql-stat or --mysql-stat-prometheus"
        } else {
            "--mysql-stat-top requires --mysql-stat"
        },
    );
    (has_pg_stat_source, has_mysql_stat_source)
}

#[allow(clippy::too_many_arguments)]
// optional flags, each adds a dedicated ingestion path
async fn cmd_report(
    input: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    output: &std::path::Path,
    max_traces_embedded: Option<usize>,
    sort: Option<render::FindingsSort>,
    pg_stat_path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] pg_stat_prometheus: Option<&str>,
    #[cfg(feature = "daemon")] pg_stat_auth_header: Option<String>,
    #[cfg(feature = "daemon")] pg_stat_prom: &sentinel_core::ingest::pg_stat::PrometheusPgStat,
    before_path: Option<&std::path::Path>,
    pg_stat_top: Option<usize>,
    mysql_stat_path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] mysql_stat_prometheus: Option<&str>,
    #[cfg(feature = "daemon")] mysql_stat_auth_header: Option<String>,
    #[cfg(feature = "daemon")]
    mysql_stat_prom: &sentinel_core::ingest::mysql_stat::PrometheusMySqlStat,
    mysql_stat_top: Option<usize>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
    #[cfg(feature = "daemon")] daemon_url: Option<String>,
) {
    let config = load_config(config_path);

    let stdin_mode = is_stdin_input(input);
    let effective_input = if stdin_mode { None } else { input };
    let raw_bytes = read_events(effective_input, limits::MAX_BATCH_INPUT_BYTES);
    let raw = strip_bom(&raw_bytes);

    let (mut report, traces, origin) = load_report_from_input(raw, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
        origin,
    );
    // The HTML JS template does not yet visually distinguish ack rows, so
    // keep `acknowledged_findings` in the embedded payload only when the
    // operator opted in via --show-acknowledged. Downstream tooling that
    // greps the embedded JSON for ack metadata stays gated on the flag.
    if !show_acknowledged {
        report.acknowledged_findings.clear();
    }
    // After the acks so a masked finding does not weigh in the aggregate,
    // and before the sink so `--max-traces-embedded` keeps the trees the
    // top findings point at rather than the ones the producer wrote first.
    // Impact when the caller said nothing, because that is what the
    // dashboard opens on. Leaving the two out of step would embed the
    // trees of one ranking and show the other, so the top row would open
    // without a tree for no reason a reader could see.
    render::sort_findings(&mut report.findings, sort.unwrap_or_default());
    let input_label = input_label_for(input, stdin_mode);

    // Clap's `requires` does not express an OR-of-flags, so validate the
    // stat source requirements post-parse. Both are checked before any
    // load below: a usage error must not cost a Prometheus scrape first.
    let (has_pg_stat_source, has_mysql_stat_source) = validate_stat_sources_or_exit(
        pg_stat_top,
        pg_stat_path,
        #[cfg(feature = "daemon")]
        pg_stat_prometheus,
        mysql_stat_top,
        mysql_stat_path,
        #[cfg(feature = "daemon")]
        mysql_stat_prometheus,
    );

    // Trace-side template counts for the pg_stat / mysql_stat
    // cross-reference: the report's own traces are already analyzed, so
    // the dashboard panels get the same trace-matched share as the
    // standalone subcommands. A precomputed Report input carries no
    // traces, and claiming "0 of N matched" there would read as a
    // tracing gap rather than as the absence of any trace to match.
    let trace_sql_counts = (!traces.is_empty() && (has_pg_stat_source || has_mysql_stat_source))
        .then(|| pipeline::trace_sql_template_counts(&traces));

    let top_n = pg_stat_top.unwrap_or(DEFAULT_PG_STAT_TOP);
    let pg_stat = pg_stat::resolve_pg_stat_source(
        pg_stat_path,
        #[cfg(feature = "daemon")]
        pg_stat_prometheus,
        #[cfg(feature = "daemon")]
        pg_stat_auth_header,
        #[cfg(feature = "daemon")]
        pg_stat_prom,
        #[cfg(feature = "daemon")]
        &config,
        top_n,
        trace_sql_counts.as_ref(),
    )
    .await;

    let mysql_top_n = mysql_stat_top.unwrap_or(DEFAULT_PG_STAT_TOP);
    let mysql_stat = mysql_stat::resolve_mysql_stat_source(
        mysql_stat_path,
        #[cfg(feature = "daemon")]
        mysql_stat_prometheus,
        #[cfg(feature = "daemon")]
        mysql_stat_auth_header,
        #[cfg(feature = "daemon")]
        mysql_stat_prom,
        mysql_top_n,
        trace_sql_counts.as_ref(),
    )
    .await;

    let diff = before_path.map(|path| {
        load_diff_against_baseline(
            path,
            &report,
            &config,
            acknowledgments_path,
            no_acknowledgments,
        )
    });

    // Field-by-field on a Default: RenderOptions is #[non_exhaustive], so
    // cross-crate struct literals do not compile.
    let mut options = sentinel_core::report::html::RenderOptions::default();
    options.input_label = input_label;
    options.max_traces_embedded = max_traces_embedded;
    // The dashboard opens on the same key the embed followed, or the top
    // rows of the list the reader lands on would be missing their trees.
    options.initial_sort = Some(
        match sort.unwrap_or_default() {
            render::FindingsSort::Impact => "impact",
            render::FindingsSort::Severity => "severity",
        }
        .to_string(),
    );
    options.pg_stat = pg_stat;
    options.mysql_stat = mysql_stat;
    options.diff = diff;
    #[cfg(feature = "daemon")]
    {
        options.daemon_url = daemon_url;
    }

    let (html, stats) = sentinel_core::report::html::render(&report, &traces, &options);
    if let Err(e) = write_file_no_follow(output, html.as_bytes()) {
        eprintln!("Error writing HTML report to {}: {e}", output.display());
        std::process::exit(EXIT_TOOLING_ERROR);
    }
    info!("HTML report written to {}", output.display());
    log_embed_trim(&stats, max_traces_embedded.is_some());
}

/// Stderr notice when the render kept fewer trees than the report holds.
/// Names the cap that applied: prescribing the flag to the operator
/// who just set it reads as the sink overriding them.
fn log_embed_trim(stats: &sentinel_core::report::html::RenderStats, explicit_cap: bool) {
    if stats.kept >= stats.total {
        return;
    }
    let trimmed = stats.total - stats.kept;
    if explicit_cap {
        info!(
            "Embedded {} of {} traces in the dashboard ({} past the --max-traces-embedded cap).",
            stats.kept, stats.total, trimmed
        );
    } else {
        info!(
            "Embedded {} of {} traces in the dashboard ({} trimmed for file size). Use --max-traces-embedded <higher> to keep more.",
            stats.kept, stats.total, trimmed
        );
    }
}

fn cmd_calibrate(
    traces_path: &std::path::Path,
    energy_path: &std::path::Path,
    output_path: &std::path::Path,
    config_path: Option<&std::path::Path>,
) {
    // Load (and so validate) --config even though calibrate only needs
    // the trace file: a broken config should fail loudly here too.
    let config = load_config(config_path);
    let raw = read_events(Some(traces_path), limits::MAX_BATCH_INPUT_BYTES);

    let (events, _ingest_stats) = ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES, &config);

    // Cap the energy CSV size the same way `read_events` caps trace files.
    // A 10 GB CSV passed as `--measured-energy` would otherwise load
    // entirely into RAM (DoS). 64 MiB is generous enough for thousands
    // of RAPL samples per minute while bounding the worst case. The
    // shared `read_file_capped` helper handles the TOCTOU + special-file
    // edge cases.
    const MAX_ENERGY_CSV_BYTES: u64 = 64 * 1024 * 1024;
    let energy_bytes = read_file_capped(energy_path, MAX_ENERGY_CSV_BYTES);
    let energy_content = match String::from_utf8(energy_bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Error: energy CSV {} is not valid UTF-8: {e}",
                energy_path.display()
            );
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    };

    let readings = match sentinel_core::calibrate::parse_energy_csv(&energy_content) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error parsing energy CSV: {e}");
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    };

    let results = match sentinel_core::calibrate::calibrate(&events, &readings) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error during calibration: {e}");
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    };

    for warning in sentinel_core::calibrate::validate_results(&results) {
        eprintln!("Warning: {warning}");
    }

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

    let toml_content = sentinel_core::calibrate::write_calibration_toml(
        &results,
        &traces_path.display().to_string(),
        &energy_path.display().to_string(),
    );
    match write_file_no_follow(output_path, toml_content.as_bytes()) {
        Ok(()) => {
            eprintln!("\nWritten to {}", output_path.display());
        }
        Err(e) => {
            eprintln!("Error writing {}: {e}", output_path.display());
            std::process::exit(EXIT_TOOLING_ERROR);
        }
    }
}

/// Print `trace not found` plus up to 20 available trace IDs (with an
/// `... and N more` tail), then exit `EXIT_TOOLING_ERROR`. Shared by
/// `explain` and `explain --tui` so both give the operator the same
/// recovery hint. `explain` has no quality gate, so a missing trace is a
/// tooling error, never a threshold breach.
fn trace_not_found_exit<'a>(trace_id: &str, available: impl Iterator<Item = &'a str>) -> ! {
    eprintln!("Error: trace ID '{trace_id}' not found");
    let ids: Vec<&str> = available.collect();
    let total = ids.len();
    let shown = ids.iter().take(20).copied().collect::<Vec<_>>().join(", ");
    if total > 20 {
        eprintln!("Available trace IDs: {shown} ... and {} more", total - 20);
    } else {
        eprintln!("Available trace IDs: {shown}");
    }
    std::process::exit(EXIT_TOOLING_ERROR);
}

fn cmd_explain(
    input: &std::path::Path,
    trace_id: &str,
    config_path: Option<&std::path::Path>,
    format: ExplainFormat,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), limits::MAX_BATCH_INPUT_BYTES);

    // A Report JSON (a daemon snapshot) carries no raw events, but since
    // 0.10.0 it can carry masked span trees: rebuild the tree from them,
    // the same source the dashboard and the TUI draw from. Detected on the
    // top-level `findings` key, which no trace-export format has, so a
    // report that fails to deserialize surfaces its own error instead of
    // falling through to a misleading "unrecognized input" from the
    // event path.
    let looks_like_report = serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("findings").map(|_| ()))
        .is_some();
    let tree = if looks_like_report {
        let report = match serde_json::from_slice::<sentinel_core::report::Report>(&raw) {
            Ok(report) => report,
            Err(e) => {
                eprintln!(
                    "Error: this looks like a Report JSON but could not be read: {e}\n\
                     A report written by a newer perf-sentinel can carry values this \
                     binary does not know."
                );
                std::process::exit(EXIT_TOOLING_ERROR);
            }
        };
        let Some(embedded) = report
            .embedded_traces
            .iter()
            .find(|t| t.trace_id == trace_id)
        else {
            if report.embedded_traces.is_empty() {
                eprintln!(
                    "Error: this Report JSON carries no span trees. A daemon snapshot embeds \
                     them when [daemon] max_retained_traces > 0. A batch report never does, \
                     rerun explain against the original trace file."
                );
                std::process::exit(EXIT_TOOLING_ERROR);
            }
            trace_not_found_exit(
                trace_id,
                report.embedded_traces.iter().map(|t| t.trace_id.as_str()),
            );
        };
        let trace = embedded.to_trace();
        let findings: Vec<sentinel_core::detect::Finding> = report
            .findings
            .iter()
            .filter(|f| f.trace_id == trace_id)
            .cloned()
            .collect();
        sentinel_core::explain::build_tree(&trace, &findings)
    } else {
        let (events, _ingest_stats) =
            ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES, &config);

        let normalized = sentinel_core::normalize::normalize_all(events);
        let traces = sentinel_core::correlate::correlate(normalized);

        let Some(trace) = traces.iter().find(|t| t.trace_id == trace_id) else {
            trace_not_found_exit(trace_id, traces.iter().map(|t| t.trace_id.as_str()));
        };

        let detect_config = sentinel_core::detect::DetectConfig::from(&config);
        let findings = sentinel_core::detect::detect(std::slice::from_ref(trace), &detect_config);

        sentinel_core::explain::build_tree(trace, &findings)
    };

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
                std::process::exit(EXIT_TOOLING_ERROR);
            }
        },
    }
}

/// The `watch` flags as a `[daemon]` TOML table, `None` when none is set.
/// A `Debug`-quoted string cannot break out of its TOML basic string. A tab,
/// a newline or a quote round-trips, while any character `Debug` writes as
/// `\u{..}` (other control and non-printable characters) fails the parse.
#[cfg(feature = "daemon")]
fn watch_flags_toml(
    listen_address: Option<&str>,
    listen_port_http: Option<u16>,
    listen_port_grpc: Option<u16>,
    max_export_findings: Option<usize>,
) -> Option<String> {
    use std::fmt::Write as _;
    let mut table = String::new();
    if let Some(addr) = listen_address {
        let _ = writeln!(table, "listen_address = {addr:?}");
    }
    if let Some(port) = listen_port_http {
        let _ = writeln!(table, "listen_port_http = {port}");
    }
    if let Some(port) = listen_port_grpc {
        let _ = writeln!(table, "listen_port_grpc = {port}");
    }
    if let Some(n) = max_export_findings {
        let _ = writeln!(table, "max_export_findings = {n}");
    }
    (!table.is_empty()).then(|| format!("[daemon]\n{table}"))
}

#[cfg(feature = "daemon")]
async fn cmd_watch(
    config_path: Option<&std::path::Path>,
    listen_address: Option<String>,
    listen_port_http: Option<u16>,
    listen_port_grpc: Option<u16>,
    max_export_findings: Option<usize>,
) {
    let flags = watch_flags_toml(
        listen_address.as_deref(),
        listen_port_http,
        listen_port_grpc,
        max_export_findings,
    );
    let config = load_config_with_flags(config_path, flags.as_deref());
    info!(
        "Starting daemon: gRPC={}:{}, HTTP={}:{}",
        config.daemon.listen_addr,
        config.daemon.listen_port_grpc,
        config.daemon.listen_addr,
        config.daemon.listen_port,
    );
    if let Err(e) = sentinel_core::daemon::run(config).await {
        eprintln!("Daemon error: {e}");
        // Walk the source chain. The top-level variants name the failing
        // resource, and the cause underneath tells a missing file from a
        // refused symlink. A FROM scratch image has no shell to
        // investigate with.
        let mut cause = std::error::Error::source(&e);
        while let Some(err) = cause {
            eprintln!("  caused by: {err}");
            cause = err.source();
        }
        std::process::exit(1);
    }
}

/// Write `contents` to `path`, refusing to follow a symlink at the
/// target on Unix. Mirrors the daemon ack store hardening so a hostile
/// pre-planted symlink cannot redirect the write outside its tree.
fn write_file_no_follow(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write as _;

    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bench::compute_latency_percentiles;
    #[cfg(feature = "daemon")]
    use crate::config_load::load_config_files;
    #[cfg(feature = "daemon")]
    use crate::pg_stat::resolve_pg_stat_auth_header_with_env;
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
        // `Analysis` is `#[non_exhaustive]`, so a sibling crate fills it
        // field by field rather than with a struct literal.
        let mut analysis = Analysis::default();
        analysis.duration_ms = 1;
        analysis.events_processed = event_count;
        analysis.traces_analyzed = 1;
        Report {
            analysis,
            findings,
            green_summary: GreenSummary {
                total_io_ops: event_count,
                top_offenders,
                ..GreenSummary::disabled(0)
            },
            quality_gate: QualityGate {
                passed: gate_passed,
                rules,
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            embedded_traces: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: String::new(),
            detection_config: None,
            disclosure_waste: None,
        }
    }

    fn make_finding(finding_type: FindingType, severity: Severity) -> Finding {
        Finding {
            finding_type,
            severity,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            grouping: Vec::new(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 6,
                window_ms: 200,
                distinct_params: 6,
                ..Default::default()
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
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: String::new(),
        }
    }

    #[test]
    fn report_no_findings() {
        let report = make_report(vec![], vec![], true, vec![]);
        // Should not panic and should print "No performance anti-patterns detected."
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_critical_severity() {
        let report = make_report(
            vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)],
            vec![],
            true,
            vec![],
        );
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_info_severity() {
        let report = make_report(
            vec![make_finding(FindingType::RedundantSql, Severity::Info)],
            vec![],
            true,
            vec![],
        );
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_redundant_http_type() {
        let report = make_report(
            vec![make_finding(FindingType::RedundantHttp, Severity::Warning)],
            vec![],
            true,
            vec![],
        );
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_slow_sql_type() {
        let report = make_report(
            vec![make_finding(FindingType::SlowSql, Severity::Warning)],
            vec![],
            true,
            vec![],
        );
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn report_slow_http_type() {
        let report = make_report(
            vec![make_finding(FindingType::SlowHttp, Severity::Critical)],
            vec![],
            true,
            vec![],
        );
        render::format_colored_report(&report, "report", false);
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
        render::format_colored_report(&report, "report", false);
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
        render::format_colored_report(&report, "report", false);
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
        render::format_colored_report(&report, "report", true);
    }

    #[test]
    fn report_with_co2_data() {
        let mut analysis = Analysis::default();
        analysis.duration_ms = 1;
        analysis.events_processed = 10;
        analysis.traces_analyzed = 1;
        let report = Report {
            analysis,
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
                ..GreenSummary::disabled(0)
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            embedded_traces: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: String::new(),
            detection_config: None,
            disclosure_waste: None,
        };
        render::format_colored_report(&report, "report", false);
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn watch_flags_override_the_config_files() {
        assert_eq!(watch_flags_toml(None, None, None, None), None);
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join(".perf-sentinel.toml");
        std::fs::write(
            &main,
            "[daemon]\nlisten_address = \"127.0.0.1\"\nlisten_port_http = 4318\nmax_export_findings = 100\n",
        )
        .unwrap();
        let flags = watch_flags_toml(Some("[::1]"), Some(14318), Some(14317), Some(250));
        let config = load_config_files(&main, false, flags.as_deref()).unwrap();
        assert_eq!(config.daemon.listen_addr, "[::1]");
        assert_eq!(config.daemon.listen_port, 14318);
        assert_eq!(config.daemon.listen_port_grpc, 14317);
        assert_eq!(config.daemon.max_export_findings, 250);
        // A flag goes through the same bounds as the file.
        let flags = watch_flags_toml(None, Some(0), None, None);
        let error = load_config_files(&main, false, flags.as_deref()).unwrap_err();
        assert!(error.starts_with("in command-line flags: "), "{error}");
        // A quote or a backslash stays inside the TOML string.
        let flags = watch_flags_toml(Some("a\"b\\c"), None, None, None);
        let config = load_config_files(&main, false, flags.as_deref()).unwrap();
        assert_eq!(config.daemon.listen_addr, "a\"b\\c");
    }

    #[test]
    fn bench_percentiles_follow_nearest_rank_indices() {
        let durations_ns: Vec<u64> = (1..=100).map(|n| n * 1_000).collect();
        let (p50_us, p99_us) = compute_latency_percentiles(&durations_ns, 1);

        assert!((p50_us - 50.0).abs() < f64::EPSILON);
        assert!((p99_us - 99.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bench_percentiles_handle_single_sample() {
        // n = 1: both percentiles collapse to the only value.
        let (p50_us, p99_us) = compute_latency_percentiles(&[7_000], 1);
        assert!((p50_us - 7.0).abs() < f64::EPSILON);
        assert!((p99_us - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bench_percentiles_handle_two_samples() {
        // n = 2: ceil(2*0.50)=1 → p50_idx = 0, ceil(2*0.99)=2 → p99_idx = 1.
        let (p50_us, p99_us) = compute_latency_percentiles(&[1_000, 3_000], 1);
        assert!((p50_us - 1.0).abs() < f64::EPSILON);
        assert!((p99_us - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bench_percentiles_handle_sample_size_just_past_hundred() {
        // n = 101: ceil(101*0.99)=100 → p99_idx = 99 → value 100µs.
        let durations_ns: Vec<u64> = (1..=101).map(|n| n * 1_000).collect();
        let (_, p99_us) = compute_latency_percentiles(&durations_ns, 1);
        assert!((p99_us - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bench_percentiles_return_zeros_on_empty_slice() {
        // Guards against indexing panic when no samples were recorded.
        let (p50_us, p99_us) = compute_latency_percentiles(&[], 1);
        assert!((p50_us - 0.0).abs() < f64::EPSILON);
        assert!((p99_us - 0.0).abs() < f64::EPSILON);
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn pg_stat_auth_header_env_var_takes_precedence_over_flag() {
        // When the env lookup returns a header, it wins over the
        // --auth-header flag value, matching the Electricity Maps precedence.
        let resolved = resolve_pg_stat_auth_header_with_env(
            Some("Authorization: Bearer from-flag".to_string()),
            || Some("Authorization: Bearer from-env".to_string()),
        );
        assert_eq!(
            resolved.as_deref(),
            Some("Authorization: Bearer from-env"),
            "env var must take precedence over the CLI flag value"
        );
    }

    #[cfg(feature = "daemon")]
    #[test]
    fn pg_stat_auth_header_falls_back_to_flag_when_env_unset() {
        let resolved = resolve_pg_stat_auth_header_with_env(
            Some("Authorization: Bearer from-flag".to_string()),
            || None,
        );
        assert_eq!(
            resolved.as_deref(),
            Some("Authorization: Bearer from-flag"),
            "flag value is used when the env var is unset"
        );
    }

    #[test]
    fn completions_subcommand_accepts_known_shells() {
        for shell_arg in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let cli = Cli::try_parse_from(["perf-sentinel", "completions", shell_arg])
                .unwrap_or_else(|e| panic!("failed to parse 'completions {shell_arg}': {e}"));
            match cli.command {
                Commands::Completions { .. } => {}
                _ => panic!("expected Commands::Completions for '{shell_arg}'"),
            }
        }
    }

    #[test]
    fn completions_subcommand_rejects_unknown_shell() {
        let result = Cli::try_parse_from(["perf-sentinel", "completions", "tcsh"]);
        assert!(
            result.is_err(),
            "tcsh is not a clap_complete::Shell variant"
        );
    }

    #[test]
    fn man_subcommand_parses() {
        let cli = Cli::try_parse_from(["perf-sentinel", "man"]).expect("failed to parse 'man'");
        assert!(matches!(cli.command, Commands::Man));
    }

    #[test]
    fn man_subcommand_renders_roff() {
        let mut buf: Vec<u8> = Vec::new();
        render_man(&mut buf).expect("man render should succeed");
        let out = String::from_utf8(buf).expect("man output is utf-8");
        assert!(
            out.contains(".TH"),
            "man page should carry a .TH roff header"
        );
        assert!(
            out.to_uppercase().contains("PERF-SENTINEL"),
            "man page should name the binary"
        );
        // Root page plus one page per subcommand: several .TH headers, and
        // tunables documented only in a subcommand long_about must surface.
        assert!(
            out.matches(".TH").count() > 1,
            "expected a man page per subcommand, not just the root"
        );
        // `analysis_queue_capacity` lives in the `watch` long_about, which
        // only exists with the daemon feature.
        #[cfg(feature = "daemon")]
        assert!(
            out.contains("analysis_queue_capacity"),
            "watch tunables should appear in the man output"
        );
    }
}
