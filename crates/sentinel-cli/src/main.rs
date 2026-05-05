#![warn(clippy::pedantic)]
#![allow(clippy::too_many_lines)] // print_colored_report is long but straightforward
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 for elapsed_ms, f64 -> usize for percentile index
#![allow(clippy::cast_precision_loss)] // usize -> f64 for throughput and latency computation
#![allow(clippy::cast_sign_loss)] // i64 (libc::ru_maxrss) -> usize for RSS bytes on macOS
#![allow(clippy::items_after_statements)] // bench report struct defined near its use

// See `docs/design/07-CLI-CONFIG-RELEASE.md` § "Allocator on musl builds".
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "daemon")]
mod ack;
#[cfg(feature = "daemon")]
mod query;
mod render;
#[cfg(feature = "tui")]
mod tui;

use clap::{Parser, Subcommand};
use render::{emit_report_and_gate, print_colored_report};
use sentinel_core::config::Config;
use sentinel_core::ingest::IngestSource;
use sentinel_core::ingest::json::JsonIngest;
use sentinel_core::pipeline;
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
    ///
    /// Cross-trace correlations are computed by the daemon's rolling
    /// window correlator and are not available in batch analyze. Use
    /// `perf-sentinel watch` then `perf-sentinel query correlations`
    /// for cross-trace findings.
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
        /// Path to `.perf-sentinel-acknowledgments.toml`. Defaults to that
        /// filename in the current working directory.
        #[arg(long, value_name = "PATH")]
        acknowledgments: Option<PathBuf>,
        /// Disable acknowledgment filtering (full audit view).
        #[arg(long)]
        no_acknowledgments: bool,
        /// Include acknowledged findings in the output, alongside ack metadata.
        #[arg(long)]
        show_acknowledged: bool,
    },

    /// Watch for traces in real-time (daemon mode).
    #[cfg(feature = "daemon")]
    Watch {
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Override the daemon listen address (e.g. `0.0.0.0` for container deployments).
        /// Takes precedence over `[daemon] listen_address` in the config file.
        #[arg(long)]
        listen_address: Option<String>,
        /// Override the daemon HTTP (OTLP + API) listen port.
        #[arg(long)]
        listen_port_http: Option<u16>,
        /// Override the daemon gRPC (OTLP) listen port.
        #[arg(long)]
        listen_port_grpc: Option<u16>,
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
    ///
    /// `--input` accepts either a raw events JSON (auto-detected:
    /// native, Jaeger or Zipkin) or a pre-computed Report JSON
    /// (e.g. a daemon snapshot from `/api/export/report`). With a
    /// Report input the Findings and Correlations panels light up
    /// fully; the Detail panel falls back to per-trace stubs because
    /// Reports do not carry raw spans.
    #[cfg(feature = "tui")]
    Inspect {
        /// Path to a JSON trace file or a pre-computed Report JSON.
        #[arg(short, long)]
        input: PathBuf,
        /// Path to a `.perf-sentinel.toml` config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Path to `.perf-sentinel-acknowledgments.toml`. Defaults to that
        /// filename in the current working directory.
        #[arg(long, value_name = "PATH")]
        acknowledgments: Option<PathBuf>,
        /// Disable acknowledgment filtering (full audit view).
        #[arg(long)]
        no_acknowledgments: bool,
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
        /// Optional auth header in curl format to attach to every Tempo request.
        /// Example: --auth-header "Authorization: Bearer ${TOKEN}".
        #[arg(long, conflicts_with = "auth_header_env")]
        auth_header: Option<String>,
        /// Read the auth header value from the named environment variable,
        /// avoiding the `ps`-visibility of --auth-header. The env var value
        /// must already be in `Name: Value` curl format.
        #[arg(long, conflicts_with = "auth_header")]
        auth_header_env: Option<String>,
        /// Path to a `.perf-sentinel.toml` config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format: text (colored, default), json, sarif.
        #[arg(long, value_enum)]
        format: Option<OutputFormat>,
        /// Enable CI quality gate mode (exit 1 if gate fails, JSON output).
        #[arg(long)]
        ci: bool,
        /// Path to `.perf-sentinel-acknowledgments.toml`. Defaults to that
        /// filename in the current working directory.
        #[arg(long, value_name = "PATH")]
        acknowledgments: Option<PathBuf>,
        /// Disable acknowledgment filtering (full audit view).
        #[arg(long)]
        no_acknowledgments: bool,
        /// Include acknowledged findings in the output, alongside ack metadata.
        #[arg(long)]
        show_acknowledged: bool,
    },

    /// Query a Jaeger query API backend (Jaeger or Victoria Traces) for traces and analyze them.
    #[cfg(feature = "jaeger-query")]
    JaegerQuery {
        /// Jaeger query API endpoint (e.g. `http://localhost:16686` or `http://victoria:10428`).
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
        /// Maximum number of traces to fetch (1..=10000).
        #[arg(long, default_value = "100", value_parser = clap::value_parser!(u32).range(1..=10_000))]
        max_traces: u32,
        /// Optional auth header in curl format to attach to every backend request.
        /// Example: --auth-header "Authorization: Bearer ${TOKEN}".
        #[arg(long, conflicts_with = "auth_header_env")]
        auth_header: Option<String>,
        /// Read the auth header value from the named environment variable,
        /// avoiding the `ps`-visibility of --auth-header. The env var value
        /// must already be in `Name: Value` curl format.
        #[arg(long, conflicts_with = "auth_header")]
        auth_header_env: Option<String>,
        /// Path to a `.perf-sentinel.toml` config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format: text (colored, default), json, sarif.
        #[arg(long, value_enum)]
        format: Option<OutputFormat>,
        /// Enable CI quality gate mode (exit 1 if gate fails, JSON output).
        #[arg(long)]
        ci: bool,
        /// Path to `.perf-sentinel-acknowledgments.toml`. Defaults to that
        /// filename in the current working directory.
        #[arg(long, value_name = "PATH")]
        acknowledgments: Option<PathBuf>,
        /// Disable acknowledgment filtering (full audit view).
        #[arg(long)]
        no_acknowledgments: bool,
        /// Include acknowledged findings in the output, alongside ack metadata.
        #[arg(long)]
        show_acknowledged: bool,
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
        input: Option<PathBuf>,
        /// Prometheus endpoint to scrape `pg_stat_statements` metrics from.
        #[cfg(feature = "daemon")]
        #[arg(long)]
        prometheus: Option<String>,
        /// Optional auth header for --prometheus. Example:
        /// --auth-header "Authorization: Bearer ${TOKEN}". Falls back to
        /// the `PERF_SENTINEL_PGSTAT_AUTH_HEADER` env var when unset.
        #[cfg(feature = "daemon")]
        #[arg(long)]
        auth_header: Option<String>,
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

    /// Query a running perf-sentinel daemon for findings and status.
    #[cfg(feature = "daemon")]
    Query {
        /// Daemon HTTP endpoint.
        #[arg(long, default_value = "http://localhost:4318")]
        daemon: String,
        #[command(subcommand)]
        action: QueryAction,
    },

    /// Acknowledge findings via the daemon API (since 0.5.20).
    ///
    /// Three subactions: `create`, `revoke`, `list`. Auth via the
    /// `PERF_SENTINEL_DAEMON_API_KEY` environment variable,
    /// `--api-key-file <path>`, or interactive prompt on 401 when stdin
    /// is a TTY. TOML CI acks (`.perf-sentinel-acknowledgments.toml`)
    /// are out of scope, edit the file and ship via PR review instead.
    #[cfg(feature = "daemon")]
    Ack {
        /// Daemon HTTP endpoint.
        #[arg(
            long,
            default_value = "http://localhost:4318",
            env = "PERF_SENTINEL_DAEMON_URL"
        )]
        daemon: String,
        #[command(subcommand)]
        action: ack::AckAction,
    },

    /// Produce a single-file HTML dashboard for post-mortem exploration.
    ///
    /// Pipeline identical to `analyze`, output is a self-contained HTML
    /// file (vanilla JS, no external resources, works offline). Exits 0
    /// even when the quality gate fails (the gate status is rendered as
    /// a badge in the HTML top bar, not as a CI signal). Use `analyze
    /// --ci` for the exit-code semantics.
    Report {
        /// Path to a JSON trace file. Omit or pass `-` to read from stdin.
        /// Same format auto-detection as `analyze --input` (native JSON,
        /// Jaeger, Zipkin v2). A pre-computed Report JSON (e.g. a daemon
        /// snapshot from `/api/export/report`) is also accepted and
        /// rendered without re-analysis.
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// Path to a .perf-sentinel.toml config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// HTML output file path. Overwritten if it already exists.
        #[arg(short, long)]
        output: PathBuf,
        /// Maximum number of traces to embed for the Explain tab. When
        /// unset, the sink trims to target a ~5 MB HTML file size.
        #[arg(long, value_name = "N")]
        max_traces_embedded: Option<usize>,
        /// Path to a `pg_stat_statements` CSV or JSON export. When set,
        /// the dashboard shows a `pg_stat` tab and enables the
        /// Explain-to-`pg_stat` cross-navigation for matching SQL templates.
        #[arg(long, value_name = "FILE")]
        pg_stat: Option<PathBuf>,
        /// Prometheus endpoint to scrape `pg_stat_statements` metrics from
        /// (one-shot HTTP GET, not streaming). Mutually exclusive with
        /// `--pg-stat`.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "URL", conflicts_with = "pg_stat")]
        pg_stat_prometheus: Option<String>,
        /// Optional auth header for --pg-stat-prometheus. Example:
        /// --pg-stat-auth-header "Authorization: Bearer ${TOKEN}". Falls
        /// back to the `PERF_SENTINEL_PGSTAT_AUTH_HEADER` env var when unset.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "NAME_VALUE", requires = "pg_stat_prometheus")]
        pg_stat_auth_header: Option<String>,
        /// Path to a baseline report JSON, as produced by `analyze
        /// --format json`. When set, the dashboard shows a Diff tab
        /// comparing the current run against the baseline.
        #[arg(long, value_name = "FILE")]
        before: Option<PathBuf>,
        /// Override the number of top entries per `pg_stat` ranking
        /// (default: 10). Only meaningful with --pg-stat or
        /// --pg-stat-prometheus.
        ///
        /// Accepts values in `[1, 10000]`. Values above ~1000 rarely
        /// add insight and stress the upstream exporter; the
        /// `postgres_exporter` default query timeout is 30s.
        /// Supplying this flag without a `pg_stat` source errors
        /// with a message pointing at the required companion flag.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
        pg_stat_top: Option<u32>,
        /// Path to `.perf-sentinel-acknowledgments.toml`. Defaults to that
        /// filename in the current working directory.
        #[arg(long, value_name = "PATH")]
        acknowledgments: Option<PathBuf>,
        /// Disable acknowledgment filtering (full audit view).
        #[arg(long)]
        no_acknowledgments: bool,
        /// Retain acknowledged findings in the embedded JSON payload.
        #[arg(long)]
        show_acknowledged: bool,
        /// Daemon URL for the HTML live mode. When set, the generated
        /// HTML connects to the daemon at runtime: per-finding
        /// Ack/Revoke buttons, an Acknowledgments panel, a connection
        /// status indicator, and a manual refresh button. The
        /// document origin must be in the daemon's
        /// `[daemon.cors] allowed_origins` whitelist. Without this
        /// flag, the report is purely static (default).
        ///
        /// Example: `--daemon-url http://localhost:4318`. Path,
        /// query string, userinfo (`user@host`) and trailing slashes
        /// are rejected at parse time.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "URL")]
        daemon_url: Option<String>,
    },

    /// Compare two trace sets and emit a delta report (regressions and improvements).
    Diff {
        /// Path to the baseline trace file (e.g. base branch, last release).
        #[arg(long)]
        before: PathBuf,
        /// Path to the candidate trace file (e.g. PR branch, current build).
        #[arg(long)]
        after: PathBuf,
        /// Path to a .perf-sentinel.toml config file. Applied to both runs.
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format: text (default), json, sarif.
        /// SARIF emits only `new_findings` (resolved findings have no SARIF concept).
        #[arg(long, value_enum)]
        format: Option<OutputFormat>,
        /// Optional output file. Defaults to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Path to `.perf-sentinel-acknowledgments.toml`. Defaults to that
        /// filename in the current working directory. Applied to both runs.
        #[arg(long, value_name = "PATH")]
        acknowledgments: Option<PathBuf>,
        /// Disable acknowledgment filtering on both runs (full audit view).
        #[arg(long)]
        no_acknowledgments: bool,
    },
}

/// Output format for query sub-actions.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, clap::ValueEnum)]
enum QueryOutputFormat {
    /// Colored terminal output (default).
    Text,
    /// Structured JSON.
    Json,
}

/// Sub-actions for the `query` subcommand.
#[cfg(feature = "daemon")]
#[derive(Subcommand)]
enum QueryAction {
    /// List recent findings from the daemon.
    Findings {
        /// Filter by service name.
        #[arg(long)]
        service: Option<String>,
        /// Filter by finding type (e.g. `n_plus_one_sql`).
        #[arg(long, value_name = "TYPE")]
        finding_type: Option<String>,
        /// Filter by severity (critical, warning, info).
        #[arg(long)]
        severity: Option<String>,
        /// Maximum number of results (default 50).
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
    /// Show the explain tree for a trace from daemon memory.
    Explain {
        /// Trace ID to explain.
        #[arg(long)]
        trace_id: String,
        /// Output format: text (colored tree, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
    /// Interactive TUI with live daemon data.
    #[cfg(feature = "tui")]
    Inspect,
    /// Show active cross-trace correlations.
    Correlations {
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
    },
    /// Show daemon status (uptime, traces, findings count).
    Status {
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
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
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
        } => {
            cmd_analyze(
                input.as_deref(),
                config.as_deref(),
                ci,
                format,
                acknowledgments.as_deref(),
                no_acknowledgments,
                show_acknowledged,
            );
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
        Commands::Watch {
            config,
            listen_address,
            listen_port_http,
            listen_port_grpc,
        } => {
            cmd_watch(
                config.as_deref(),
                listen_address,
                listen_port_http,
                listen_port_grpc,
            )
            .await;
        }
        Commands::Demo { config } => cmd_demo(config.as_deref()),
        Commands::Bench { input, iterations } => cmd_bench(input.as_deref(), iterations),
        #[cfg(feature = "tui")]
        Commands::Inspect {
            input,
            config,
            acknowledgments,
            no_acknowledgments,
        } => cmd_inspect(
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
            max_traces,
            auth_header,
            auth_header_env,
            config,
            format,
            ci,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
        } => {
            let resolved_auth = match resolve_auth_header(auth_header, auth_header_env) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            cmd_tempo(
                &endpoint,
                trace_id.as_deref(),
                service.as_deref(),
                &lookback,
                max_traces,
                resolved_auth.as_deref(),
                config.as_deref(),
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
            max_traces,
            auth_header,
            auth_header_env,
            config,
            format,
            ci,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
        } => {
            let resolved_auth = match resolve_auth_header(auth_header, auth_header_env) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            cmd_jaeger_query(
                &endpoint,
                trace_id.as_deref(),
                service.as_deref(),
                &lookback,
                max_traces as usize,
                resolved_auth.as_deref(),
                config.as_deref(),
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
            top_n,
            traces,
            config,
            format,
        } => {
            #[cfg(feature = "daemon")]
            if let Some(ref prom_endpoint) = prometheus {
                // `main` is already async (`#[tokio::main]`), so `.await`
                // the fetch directly. Creating a nested `Runtime::new()`
                // here would panic at runtime with "Cannot start a runtime
                // from within a runtime."
                let resolved_auth = resolve_pg_stat_auth_header(auth_header);
                let entries = sentinel_core::ingest::pg_stat::fetch_from_prometheus(
                    prom_endpoint,
                    top_n,
                    resolved_auth.as_deref(),
                )
                .await
                .unwrap_or_else(|e| {
                    eprintln!("Prometheus fetch failed: {e}");
                    std::process::exit(1);
                });
                cmd_pg_stat_from_entries(
                    entries,
                    top_n,
                    traces.as_deref(),
                    config.as_deref(),
                    format,
                );
            } else if let Some(ref path) = input {
                cmd_pg_stat(path, top_n, traces.as_deref(), config.as_deref(), format);
            } else {
                eprintln!("Either --input or --prometheus is required");
                std::process::exit(1);
            }
            #[cfg(not(feature = "daemon"))]
            if let Some(ref path) = input {
                cmd_pg_stat(path, top_n, traces.as_deref(), config.as_deref(), format);
            } else {
                eprintln!("--input is required");
                std::process::exit(1);
            }
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
            pg_stat,
            #[cfg(feature = "daemon")]
            pg_stat_prometheus,
            #[cfg(feature = "daemon")]
            pg_stat_auth_header,
            before,
            pg_stat_top,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
            #[cfg(feature = "daemon")]
            daemon_url,
        } => {
            #[cfg(feature = "daemon")]
            let daemon_url = match daemon_url {
                Some(raw) => match ack::validate_url(&raw) {
                    Ok(normalized) => Some(normalized),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                },
                None => None,
            };
            cmd_report(
                input.as_deref(),
                config.as_deref(),
                &output,
                max_traces_embedded,
                pg_stat.as_deref(),
                #[cfg(feature = "daemon")]
                pg_stat_prometheus.as_deref(),
                #[cfg(feature = "daemon")]
                pg_stat_auth_header,
                before.as_deref(),
                // `try_from` over `as` so a 16-bit target drops the
                // flag instead of truncating silently. No supported
                // build has `usize < 32` bits, so the only effect is
                // to keep the cast honest.
                pg_stat_top.and_then(|n| usize::try_from(n).ok()),
                acknowledgments.as_deref(),
                no_acknowledgments,
                show_acknowledged,
                #[cfg(feature = "daemon")]
                daemon_url,
            )
            .await;
        }
    }
}

/// Resolve the final auth header string from the two mutually
/// exclusive CLI flags. clap already rejects the "both set" case via
/// `conflicts_with`; this helper only handles "neither / one / other"
/// and reads the env var when `--auth-header-env` is used.
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
fn resolve_auth_header(
    direct: Option<String>,
    env_var: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(value) = direct {
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

/// Read a file into memory, capping the byte count at `max_size`.
/// Exits with code 1 on any IO error or if the file exceeds the cap.
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
            std::process::exit(1);
        }
    };
    let mut buf = Vec::new();
    if let Err(e) = file.take(max_size + 1).read_to_end(&mut buf) {
        eprintln!("Error reading {}: {e}", path.display());
        std::process::exit(1);
    }
    if buf.len() as u64 > max_size {
        eprintln!(
            "Error: file {} exceeds maximum of {max_size} bytes",
            path.display()
        );
        std::process::exit(1);
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
/// `no_acknowledgments` is set or when the file is absent. Exits 1 with
/// a clean stderr message on parse failure.
fn apply_acknowledgments_or_exit(
    report: &mut sentinel_core::report::Report,
    config: &Config,
    override_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) {
    if no_acknowledgments {
        return;
    }
    let path = resolve_acknowledgments_path(override_path);
    let acks = sentinel_core::acknowledgments::load_from_file(&path).unwrap_or_else(|e| {
        eprintln!("Error loading acknowledgments {}: {e}", path.display());
        std::process::exit(1);
    });
    sentinel_core::acknowledgments::apply_to_report(report, &acks, config, chrono::Utc::now());
}

fn cmd_analyze(
    input: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    ci: bool,
    format: Option<OutputFormat>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
) {
    let config = load_config(config_path);
    let raw = read_events(input, config.max_payload_size);

    let events = ingest_json_or_exit(&raw, config.max_payload_size);

    let mut report = pipeline::analyze(events, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    emit_report_and_gate(&mut report, format, ci, "report", show_acknowledged);
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
    let before_raw = read_events(Some(before), config.max_payload_size);
    let before_events = ingest_json_or_exit(&before_raw, config.max_payload_size);
    let mut before_report = pipeline::analyze(before_events, &config);

    let after_raw = read_events(Some(after), config.max_payload_size);
    let after_events = ingest_json_or_exit(&after_raw, config.max_payload_size);
    let mut after_report = pipeline::analyze(after_events, &config);

    // Apply the same ack file to both runs so the diff stays meaningful:
    // an ack present on both sides masks the finding from both, an ack
    // landing between base and PR masks it from the after run only.
    apply_acknowledgments_or_exit(
        &mut before_report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    apply_acknowledgments_or_exit(
        &mut after_report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );

    let diff = sentinel_core::diff::diff_runs(&before_report, &after_report);
    if let Err(e) = render::emit_diff(&diff, format, output) {
        eprintln!("Error writing diff: {e}");
        std::process::exit(1);
    }
}

/// Return `true` when the `--input` argument asks for stdin, either
/// explicitly (`--input -`) or implicitly (flag omitted). `analyze`
/// accepts only the omitted form, `report` accepts both for shell
/// composability (`tempo --output - | report --input - --output ...`).
fn is_stdin_input(input: Option<&std::path::Path>) -> bool {
    input.is_none_or(|p| p == std::path::Path::new("-"))
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
/// auto-detect below would otherwise reject a perfectly valid payload.
fn strip_bom(raw: &[u8]) -> &[u8] {
    raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw)
}

/// Parse a pre-computed `Report` JSON from stdin or a file. Enforces the
/// same 32-level nesting cap the trace-event ingest applies. Exits 1 on
/// a depth overflow or a serde error, both with a user-readable message.
fn parse_report_json_or_exit(raw: &[u8], source_label: &str) -> sentinel_core::report::Report {
    if sentinel_core::ingest::json::exceeds_max_depth(raw) {
        eprintln!(
            "Error: {source_label} JSON exceeds maximum nesting depth of {}",
            sentinel_core::ingest::json::MAX_JSON_DEPTH
        );
        std::process::exit(1);
    }
    let mut report =
        serde_json::from_slice::<sentinel_core::report::Report>(raw).unwrap_or_else(|e| {
            eprintln!("Error parsing {source_label} as Report JSON: {e}");
            std::process::exit(1);
        });
    // Pre-0.5.17 baselines have no signature, fill them in so ack
    // matching and copy-paste workflows behave the same as on a fresh run.
    sentinel_core::acknowledgments::enrich_with_signatures(&mut report.findings);
    report
}

/// Dispatch the `--input` payload based on its JSON shape. A top-level
/// array is pipelined through normalize/correlate/detect/score (covers
/// native event streams and Zipkin v2, auto-detected by `JsonIngest`).
/// A top-level object is first tried as a pre-computed `Report` (daemon
/// snapshot from `/api/export/report`, baseline file); on Report parse
/// failure it falls back to `JsonIngest` which auto-detects Jaeger via
/// `detect_format`. Trying Report first guarantees a daemon snapshot is
/// never misrouted to the Jaeger ingest, even when its payload happens
/// to contain a `"data"` field literal somewhere in the first 4 KB. The
/// trade-off is one extra full Report parse on Jaeger inputs (rare
/// through this CLI, the normal Jaeger path is `tempo` / `jaeger-query`
/// / `analyze`). The depth cap is enforced explicitly before the Report
/// parse so an over-deep Report does not silently fall through to the
/// ingest fallback. Empty input and scalar roots exit 1 with distinct
/// messages.
fn load_report_from_input(
    raw: &[u8],
    config: &Config,
) -> (
    sentinel_core::report::Report,
    Vec<sentinel_core::correlate::Trace>,
) {
    let first_byte = raw.iter().find(|b| !b.is_ascii_whitespace()).copied();
    match first_byte {
        Some(b'[') => {
            let events = ingest_json_or_exit(raw, config.max_payload_size);
            pipeline::analyze_with_traces(events, config)
        }
        Some(b'{') => {
            if sentinel_core::ingest::json::exceeds_max_depth(raw) {
                eprintln!(
                    "Error: --input JSON exceeds maximum nesting depth of {}",
                    sentinel_core::ingest::json::MAX_JSON_DEPTH
                );
                std::process::exit(1);
            }
            if let Ok(mut report) = serde_json::from_slice::<sentinel_core::report::Report>(raw) {
                sentinel_core::acknowledgments::enrich_with_signatures(&mut report.findings);
                return (report, Vec::new());
            }
            let ingest = JsonIngest::new(config.max_payload_size);
            match ingest.ingest(raw) {
                Ok(events) => pipeline::analyze_with_traces(events, config),
                Err(e) => {
                    eprintln!(
                        "Error: --input top-level object is neither a pre-computed Report JSON nor a Jaeger export. Underlying error: {e}"
                    );
                    std::process::exit(1);
                }
            }
        }
        None => {
            eprintln!("Error: --input is empty or whitespace-only");
            std::process::exit(1);
        }
        Some(_) => {
            eprintln!(
                "Error: --input must be a JSON array of events, a Jaeger \
                 export ({{\"data\": [...]}}) or a pre-computed Report \
                 object (got a scalar or unexpected token at the root)"
            );
            std::process::exit(1);
        }
    }
}

/// Default top-N for `pg_stat` rankings inside the `report` subcommand
/// when the user does not set `--pg-stat-top`.
const DEFAULT_PG_STAT_TOP: usize = 10;

/// Lower bound on the Prometheus scrape size when only a small
/// `--pg-stat-top` is set. `rank_pg_stat` emits four rankings keyed on
/// different columns; feeding it only the `top_n` by `seconds_total`
/// (the upstream `topk` metric) biases the three non-time rankings.
/// Always scrape at least this many rows so the secondary rankings see
/// the full hot-spot distribution.
const PROMETHEUS_SCRAPE_FLOOR: usize = 200;

/// Ingest a `pg_stat_statements` CSV or JSON file and produce the
/// ranking report the HTML dashboard embeds. Exits 1 on parse failure.
fn load_pg_stat_from_file(
    path: &std::path::Path,
    config: &Config,
    top_n: usize,
) -> sentinel_core::ingest::pg_stat::PgStatReport {
    let raw_pg = read_file_capped(
        path,
        u64::try_from(config.max_payload_size).unwrap_or(u64::MAX),
    );
    match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw_pg, config.max_payload_size) {
        Ok(entries) => sentinel_core::ingest::pg_stat::rank_pg_stat(&entries, top_n),
        Err(e) => {
            eprintln!("Error parsing --pg-stat {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}

/// Scrape a `postgres_exporter` endpoint one-shot and produce the
/// ranking report. Exits 1 on transport/parse failure.
#[cfg(feature = "daemon")]
async fn load_pg_stat_from_prometheus(
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
            eprintln!("Error scraping --pg-stat-prometheus {url}: {e}");
            std::process::exit(1);
        }
    }
}

/// Resolve the `pg_stat` auth header value from the `PERF_SENTINEL_PGSTAT_AUTH_HEADER`
/// env var plus the CLI flag value. Env wins, flag is fallback, matching the
/// precedence of `PERF_SENTINEL_EMAPS_TOKEN` for Electricity Maps.
#[cfg(feature = "daemon")]
fn resolve_pg_stat_auth_header(flag_value: Option<String>) -> Option<String> {
    resolve_pg_stat_auth_header_with_env(flag_value, || {
        std::env::var("PERF_SENTINEL_PGSTAT_AUTH_HEADER").ok()
    })
}

/// Test-friendly inner form: takes the env-var lookup as a closure so
/// tests can exercise the precedence branch without mutating the
/// global process env.
#[cfg(feature = "daemon")]
fn resolve_pg_stat_auth_header_with_env(
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

/// Parse a saved baseline report and diff it against the current run.
/// Applies the same BOM strip and depth cap as `--input` in Report
/// mode. Exits 1 on failure. The same acknowledgments file is applied to
/// the baseline so a finding acked on both sides drops out of the diff
/// entirely (the alternative would surface every ack as a fake "resolved
/// in PR", a noisy false positive).
fn load_diff_against_baseline(
    before_path: &std::path::Path,
    current: &sentinel_core::report::Report,
    config: &Config,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) -> sentinel_core::diff::DiffReport {
    let raw_before = read_file_capped(
        before_path,
        u64::try_from(config.max_payload_size).unwrap_or(u64::MAX),
    );
    let slice = strip_bom(&raw_before);
    let source_label = format!("--before {}", before_path.display());
    let mut baseline = parse_report_json_or_exit(slice, &source_label);
    apply_acknowledgments_or_exit(
        &mut baseline,
        config,
        acknowledgments_path,
        no_acknowledgments,
    );
    sentinel_core::diff::diff_runs(&baseline, current)
}

#[allow(clippy::too_many_arguments)] // optional flags, each adds a dedicated ingestion path
async fn cmd_report(
    input: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
    output: &std::path::Path,
    max_traces_embedded: Option<usize>,
    pg_stat_path: Option<&std::path::Path>,
    #[cfg(feature = "daemon")] pg_stat_prometheus: Option<&str>,
    #[cfg(feature = "daemon")] pg_stat_auth_header: Option<String>,
    before_path: Option<&std::path::Path>,
    pg_stat_top: Option<usize>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
    #[cfg(feature = "daemon")] daemon_url: Option<String>,
) {
    let config = load_config(config_path);

    let stdin_mode = is_stdin_input(input);
    let effective_input = if stdin_mode { None } else { input };
    let raw_bytes = read_events(effective_input, config.max_payload_size);
    let raw = strip_bom(&raw_bytes);

    let (mut report, traces) = load_report_from_input(raw, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    // The HTML JS template does not yet visually distinguish ack rows, so
    // keep `acknowledged_findings` in the embedded payload only when the
    // operator opted in via --show-acknowledged. Downstream tooling that
    // greps the embedded JSON for ack metadata stays gated on the flag.
    if !show_acknowledged {
        report.acknowledged_findings.clear();
    }
    let input_label = input_label_for(input, stdin_mode);

    // Clap's `requires` does not express an OR-of-flags, so validate
    // the pg_stat source requirement post-parse.
    #[cfg(feature = "daemon")]
    let has_pg_stat_source = pg_stat_path.is_some() || pg_stat_prometheus.is_some();
    #[cfg(not(feature = "daemon"))]
    let has_pg_stat_source = pg_stat_path.is_some();
    if pg_stat_top.is_some() && !has_pg_stat_source {
        #[cfg(feature = "daemon")]
        eprintln!("Error: --pg-stat-top requires --pg-stat or --pg-stat-prometheus");
        #[cfg(not(feature = "daemon"))]
        eprintln!("Error: --pg-stat-top requires --pg-stat");
        std::process::exit(2);
    }
    let top_n = pg_stat_top.unwrap_or(DEFAULT_PG_STAT_TOP);

    // --pg-stat / --pg-stat-prometheus are mutually exclusive at the
    // clap level (conflicts_with). The Prometheus branch is gated
    // behind the daemon feature, mirroring the existing pg-stat
    // subcommand surface.
    let pg_stat = if let Some(path) = pg_stat_path {
        Some(load_pg_stat_from_file(path, &config, top_n))
    } else {
        #[cfg(feature = "daemon")]
        {
            match pg_stat_prometheus {
                Some(url) => {
                    let resolved_auth = resolve_pg_stat_auth_header(pg_stat_auth_header);
                    Some(
                        load_pg_stat_from_prometheus(url, &config, top_n, resolved_auth.as_deref())
                            .await,
                    )
                }
                None => None,
            }
        }
        #[cfg(not(feature = "daemon"))]
        {
            None
        }
    };

    let diff = before_path.map(|path| {
        load_diff_against_baseline(
            path,
            &report,
            &config,
            acknowledgments_path,
            no_acknowledgments,
        )
    });

    let options = sentinel_core::report::html::RenderOptions {
        input_label,
        max_traces_embedded,
        pg_stat,
        diff,
        #[cfg(feature = "daemon")]
        daemon_url,
        #[cfg(not(feature = "daemon"))]
        daemon_url: None,
    };

    let (html, stats) = sentinel_core::report::html::render(&report, &traces, &options);
    if let Err(e) = std::fs::write(output, &html) {
        eprintln!("Error writing HTML report to {}: {e}", output.display());
        std::process::exit(1);
    }
    info!("HTML report written to {}", output.display());
    if stats.kept < stats.total {
        let trimmed = stats.total - stats.kept;
        info!(
            "Embedded {} of {} traces in the dashboard ({} trimmed for file size). Use --max-traces-embedded <higher> to keep more.",
            stats.kept, stats.total, trimmed
        );
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
    auth_header: Option<&str>,
    config_path: Option<&std::path::Path>,
    format: Option<OutputFormat>,
    ci: bool,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
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
        auth_header,
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

    let mut report = pipeline::analyze(events, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    emit_report_and_gate(&mut report, format, ci, "tempo", show_acknowledged);
}

#[cfg(feature = "jaeger-query")]
#[allow(clippy::too_many_arguments)]
async fn cmd_jaeger_query(
    endpoint: &str,
    trace_id: Option<&str>,
    service: Option<&str>,
    lookback: &str,
    max_traces: usize,
    auth_header: Option<&str>,
    config_path: Option<&std::path::Path>,
    format: Option<OutputFormat>,
    ci: bool,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
    show_acknowledged: bool,
) {
    if trace_id.is_none() && service.is_none() {
        eprintln!("Error: either --trace-id or --service is required");
        std::process::exit(1);
    }
    if trace_id.is_some() && service.is_some() {
        eprintln!("Error: --trace-id and --service are mutually exclusive");
        std::process::exit(1);
    }

    let lookback_duration = match sentinel_core::ingest::jaeger_query::parse_lookback(lookback) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error parsing lookback: {e}");
            std::process::exit(1);
        }
    };

    let config = load_config(config_path);

    let events = match sentinel_core::ingest::jaeger_query::ingest_from_jaeger_query(
        endpoint,
        service,
        trace_id,
        lookback_duration,
        max_traces,
        auth_header,
    )
    .await
    {
        Ok(events) => events,
        Err(e) => {
            eprintln!("Error fetching traces from Jaeger query API: {e}");
            std::process::exit(1);
        }
    };

    info!(
        events = events.len(),
        "Ingested events from Jaeger query API, running analysis"
    );

    let mut report = pipeline::analyze(events, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    emit_report_and_gate(&mut report, format, ci, "jaeger-query", show_acknowledged);
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
async fn cmd_watch(
    config_path: Option<&std::path::Path>,
    listen_address: Option<String>,
    listen_port_http: Option<u16>,
    listen_port_grpc: Option<u16>,
) {
    let mut config = load_config(config_path);
    if let Some(addr) = listen_address {
        config.listen_addr = addr;
    }
    if let Some(port) = listen_port_http {
        config.listen_port = port;
    }
    if let Some(port) = listen_port_grpc {
        config.listen_port_grpc = port;
    }
    // Re-run validation so a CLI override to a non-loopback address still
    // emits the security warning from `validate_listen_addr`.
    if let Err(e) = config.validate() {
        eprintln!("Error: invalid daemon configuration after CLI overrides: {e}");
        std::process::exit(1);
    }
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
fn cmd_inspect(
    input: &std::path::Path,
    config_path: Option<&std::path::Path>,
    acknowledgments_path: Option<&std::path::Path>,
    no_acknowledgments: bool,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), config.max_payload_size);
    let detect_config = sentinel_core::detect::DetectConfig::from(&config);

    // Auto-detect events array vs pre-computed Report object, same shape
    // contract as `report --input`. A Report payload (e.g. a daemon
    // snapshot dumped via /api/export/report) lights up the Findings and
    // Correlations panels. The Detail panel falls back to a per-trace
    // stub with no spans because Reports don't carry raw spans.
    let (mut report, mut traces) = load_report_from_input(&raw, &config);
    apply_acknowledgments_or_exit(
        &mut report,
        &config,
        acknowledgments_path,
        no_acknowledgments,
    );
    if traces.is_empty() && !report.findings.is_empty() {
        let trace_ids: std::collections::BTreeSet<String> =
            report.findings.iter().map(|f| f.trace_id.clone()).collect();
        traces = trace_ids
            .into_iter()
            .map(|tid| sentinel_core::correlate::Trace {
                trace_id: tid,
                spans: vec![],
            })
            .collect();
    }

    let mut app = tui::App::new(report.findings, traces, detect_config)
        .with_correlations(report.correlations);
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
    let config = load_config(config_path);
    let raw = read_events(Some(input), config.max_payload_size);

    let entries = match sentinel_core::ingest::pg_stat::parse_pg_stat(&raw, config.max_payload_size)
    {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Error parsing pg_stat_statements: {e}");
            std::process::exit(1);
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
        let traces_raw = read_events(Some(traces_path), config.max_payload_size);
        let ingest = JsonIngest::new(config.max_payload_size);
        match ingest.ingest(&traces_raw) {
            Ok(events) => {
                let report = pipeline::analyze(events, config);
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

/// Compute the per-event p50 and p99 latency in microseconds from a slice
/// of per-iteration nanosecond durations.
fn compute_latency_percentiles(durations_ns: &[u64], event_count: usize) -> (f64, f64) {
    if durations_ns.is_empty() {
        return (0.0, 0.0);
    }
    let mut per_event_ns: Vec<f64> = durations_ns
        .iter()
        .map(|&d| d as f64 / event_count as f64)
        .collect();
    per_event_ns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let len = per_event_ns.len();
    let last = len - 1;
    let p50_idx = ((len as f64 * 0.50).ceil() as usize)
        .saturating_sub(1)
        .min(last);
    let p99_idx = ((len as f64 * 0.99).ceil() as usize)
        .saturating_sub(1)
        .min(last);
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
                scoring_config: None,
            },
            quality_gate: QualityGate {
                passed: gate_passed,
                rules,
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
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
                scoring_config: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
        };
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn load_config_returns_default_when_no_file() {
        // No .perf-sentinel.toml in the test working directory
        let config = load_config(None);
        assert_eq!(config.n_plus_one_threshold, 5);
        assert_eq!(config.max_payload_size, 16 * 1024 * 1024);
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
        // Env-lookup returns a header → it wins over the --auth-header flag
        // value, matching the Electricity Maps precedence.
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
}
