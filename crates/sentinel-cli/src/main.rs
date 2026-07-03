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
mod bench;
mod demo;
mod disclose;
mod hash_bake;
#[cfg(feature = "jaeger-query")]
mod jaeger_cmd;
mod limits;
#[cfg(all(feature = "daemon", feature = "tui"))]
mod monitor;
mod mysql_stat;
mod pg_stat;
#[cfg(feature = "daemon")]
mod query;
mod render;
#[cfg(feature = "tempo")]
mod tempo_cmd;
#[cfg(feature = "tui")]
mod tui;
#[cfg(feature = "tui")]
mod tui_launch;
#[cfg(feature = "tui")]
mod tui_resize;
mod verify_hash;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use render::emit_report_and_gate;
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
#[command(
    long_about = "Lightweight polyglot performance anti-pattern detector.\n\n\
    All subcommands read tuning from a .perf-sentinel.toml file (--config), not CLI flags. \
    Batch tuning lives in [thresholds], [detection] and [green] (see `analyze --help`). \
    Daemon tuning lives in [daemon] plus [daemon.correlation|ack|cors|archive] \
    (see `watch --help`). Full reference with defaults and ranges: docs/CONFIGURATION.md."
)]
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

/// Output format for the mysql-stat command.
#[derive(Clone, Copy, clap::ValueEnum)]
enum MySqlStatOutputFormat {
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
    ///
    /// All tuning lives in the config file (`--config`), not as CLI flags:
    /// `[thresholds]` for the quality gate, `[detection]` for detector
    /// knobs, `[green]` for carbon and energy. Full reference with
    /// defaults and ranges: docs/CONFIGURATION.md.
    #[command(after_help = help_examples::ANALYZE)]
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
        /// Launch the interactive TUI instead of printing the report.
        /// Opens on the Analyze view. Enter drills down to Inspect then
        /// Explain, Esc walks back up.
        #[cfg(feature = "tui")]
        #[arg(long, conflicts_with_all = ["ci", "format", "show_acknowledged"])]
        tui: bool,
    },

    /// Watch for traces in real-time (daemon mode).
    ///
    /// All runtime tuning lives in the `[daemon]` section of the config
    /// file (`--config`), not as CLI flags (except the listen overrides
    /// below). Full reference with defaults and ranges:
    /// docs/CONFIGURATION.md.
    ///
    /// Listeners: `listen_address`, `listen_port_http`, `listen_port_grpc`,
    /// `json_socket`, `tls_cert_path`, `tls_key_path`.
    /// Window sizing and memory: `max_active_traces`, `trace_ttl_ms`,
    /// `max_events_per_trace`, `max_payload_size`, `max_retained_findings`.
    /// Bounded-queue backpressure (default 1024 each):
    /// `ingest_queue_capacity` and `analysis_queue_capacity` (sheds whole
    /// batches when full).
    /// Behavior: `sampling_rate`, `environment`, `api_enabled`.
    /// Sub-sections: `[daemon.correlation]`, `[daemon.ack]`,
    /// `[daemon.cors]`, `[daemon.archive]`.
    #[cfg(feature = "daemon")]
    #[command(after_help = help_examples::WATCH)]
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
        /// Write the HTML dashboard to this path instead of printing
        /// the colored terminal report.
        #[arg(long, value_name = "PATH")]
        html: Option<PathBuf>,
        /// Open the interactive TUI report instead of printing the
        /// colored terminal report.
        #[cfg(feature = "tui")]
        #[arg(long, conflicts_with = "html")]
        tui: bool,
    },

    /// Explain a specific trace: tree view with findings annotated inline.
    /// Span-anchored detections (N+1, redundant, slow, fanout) land on
    /// their offending spans. Trace-level detections (chatty service,
    /// pool saturation, serialized calls) are rendered in a dedicated
    /// header section above the span tree. Cross-trace percentile
    /// findings from `analyze` are not included.
    #[command(after_help = help_examples::EXPLAIN)]
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
        /// Launch the interactive TUI instead of printing the tree.
        /// Opens on the Explain view focused on --trace-id. Esc walks up
        /// to the Inspect and Analyze views.
        #[cfg(feature = "tui")]
        #[arg(long, conflicts_with = "format")]
        tui: bool,
    },

    /// Benchmark perf-sentinel on a trace file or a synthetic dataset.
    Bench {
        /// Path to a JSON trace file. Reads from stdin if omitted
        /// (unless --synthetic-events is set).
        #[arg(short, long, conflicts_with = "synthetic_events")]
        input: Option<PathBuf>,
        /// Number of iterations (default 10).
        #[arg(long, default_value = "10")]
        iterations: u32,
        /// Generate a seeded synthetic dataset of this many events
        /// in-process instead of reading a file.
        #[arg(long)]
        synthetic_events: Option<usize>,
        /// Number of distinct services in the synthetic dataset.
        #[arg(long, default_value = "16", requires = "synthetic_events")]
        services: usize,
        /// Seed for the synthetic dataset (same seed, same events).
        #[arg(long, default_value = "42", requires = "synthetic_events")]
        seed: u64,
    },

    /// Interactive TUI to inspect traces and findings.
    ///
    /// `--input` accepts either a raw events JSON (auto-detected:
    /// native, Jaeger or Zipkin) or a pre-computed Report JSON
    /// (e.g. a daemon snapshot from `/api/export/report`). With a
    /// Report input the Findings and Correlations panels light up
    /// fully. The Detail panel falls back to per-trace stubs because
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
    #[command(after_help = help_examples::TEMPO)]
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
    #[command(after_help = help_examples::JAEGER_QUERY)]
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
    #[command(after_help = help_examples::CALIBRATE)]
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
    #[command(after_help = help_examples::PG_STAT)]
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

    /// Analyze `MySQL` Performance Schema statement digests for SQL hotspot detection.
    ///
    /// Reads a CSV or JSON export of
    /// `performance_schema.events_statements_summary_by_digest`. Timer
    /// columns (picoseconds) are converted to milliseconds.
    #[command(name = "mysql-stat", after_help = help_examples::MYSQL_STAT)]
    MySqlStat {
        /// Path to an `events_statements_summary_by_digest` CSV or JSON export.
        #[arg(short, long)]
        input: PathBuf,
        /// Number of top digests per ranking (default 10).
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
        format: MySqlStatOutputFormat,
    },

    /// Query a running perf-sentinel daemon for findings and status.
    #[cfg(feature = "daemon")]
    #[command(after_help = help_examples::QUERY)]
    Query {
        /// Daemon HTTP endpoint.
        #[arg(long, default_value = "http://localhost:4318")]
        daemon: String,
        #[command(subcommand)]
        action: QueryAction,
    },

    /// Acknowledge findings via the daemon API.
    ///
    /// Three subactions: `create`, `revoke`, `list`. Auth via the
    /// `PERF_SENTINEL_DAEMON_API_KEY` environment variable,
    /// `--api-key-file <path>`, or interactive prompt on 401 when stdin
    /// is a TTY. TOML CI acks (`.perf-sentinel-acknowledgments.toml`)
    /// are out of scope, edit the file and ship via PR review instead.
    #[cfg(feature = "daemon")]
    #[command(after_help = help_examples::ACK)]
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
    #[command(after_help = help_examples::REPORT)]
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
        /// add insight and stress the upstream exporter. The
        /// `postgres_exporter` default query timeout is 30s.
        /// Supplying this flag without a `pg_stat` source errors
        /// with a message pointing at the required companion flag.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
        pg_stat_top: Option<u32>,
        /// Path to an `events_statements_summary_by_digest` CSV or JSON
        /// export (`MySQL` Performance Schema). When set, the dashboard
        /// shows a `mysql_stat` tab with the same ranking sub-switcher
        /// as `pg_stat`.
        #[arg(long, value_name = "FILE")]
        mysql_stat: Option<PathBuf>,
        /// Override the number of top entries per `mysql_stat` ranking
        /// (default: 10). Only meaningful with --mysql-stat: supplying it
        /// without the companion flag errors.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000), requires = "mysql_stat")]
        mysql_stat_top: Option<u32>,
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
    #[command(after_help = help_examples::DIFF)]
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
    /// Generate a shell completion script for the requested shell.
    ///
    /// Pipe the output to the shell-specific completion path, e.g.
    /// `perf-sentinel completions zsh > ~/.zfunc/_perf-sentinel`.
    #[command(after_help = help_examples::COMPLETIONS)]
    Completions {
        /// Target shell: bash, zsh, fish, powershell, elvish.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Generate a man page for perf-sentinel on stdout.
    ///
    /// Renders the roff man page for the top-level command (it lists the
    /// subcommands, like `git.1`). Redirect it into your man path, e.g.
    /// `perf-sentinel man > /usr/local/share/man/man1/perf-sentinel.1`.
    #[command(after_help = help_examples::MAN)]
    Man,
    /// Produce a periodic public disclosure report (v1.3 schema).
    ///
    /// Reads archived per-window `Report` NDJSON, filters to the
    /// requested period, applies the official-intent validator when
    /// applicable, computes the deterministic SHA-256 content hash, and
    /// writes a single `perf-sentinel-report.json`. Designed for public
    /// transparency, not regulatory-grade.
    #[command(after_help = help_examples::DISCLOSE)]
    Disclose {
        /// `internal`, `official`, or `audited`. `audited` is reserved for
        /// a future release and exits with code 2. Optional under `--tui`
        /// (set it live in the preview).
        #[cfg_attr(
            feature = "tui",
            arg(long, value_enum, required_unless_present = "tui")
        )]
        #[cfg_attr(not(feature = "tui"), arg(long, value_enum, required = true))]
        intent: Option<disclose::ReportIntentCli>,
        /// `internal` (G1: per-anti-pattern detail) or `public` (G2:
        /// aggregate-only per service). Optional under `--tui`.
        #[cfg_attr(
            feature = "tui",
            arg(long, value_enum, required_unless_present = "tui")
        )]
        #[cfg_attr(not(feature = "tui"), arg(long, value_enum, required = true))]
        confidentiality: Option<disclose::ConfidentialityCli>,
        /// Period selector: `calendar-quarter`, `calendar-month`,
        /// `calendar-year`, or `custom`. Optional under `--tui`.
        #[cfg_attr(
            feature = "tui",
            arg(long, value_enum, required_unless_present = "tui")
        )]
        #[cfg_attr(not(feature = "tui"), arg(long, value_enum, required = true))]
        period_type: Option<disclose::PeriodTypeCli>,
        /// Inclusive period start (UTC), YYYY-MM-DD. Optional under `--tui`.
        #[cfg_attr(
            feature = "tui",
            arg(long, value_name = "YYYY-MM-DD", required_unless_present = "tui")
        )]
        #[cfg_attr(
            not(feature = "tui"),
            arg(long, value_name = "YYYY-MM-DD", required = true)
        )]
        from: Option<chrono::NaiveDate>,
        /// Inclusive period end (UTC), YYYY-MM-DD. Optional under `--tui`.
        #[cfg_attr(
            feature = "tui",
            arg(long, value_name = "YYYY-MM-DD", required_unless_present = "tui")
        )]
        #[cfg_attr(
            not(feature = "tui"),
            arg(long, value_name = "YYYY-MM-DD", required = true)
        )]
        to: Option<chrono::NaiveDate>,
        /// One or more archive paths. Each may be a single `.ndjson`
        /// file or a directory whose `*.ndjson` files are unioned.
        #[arg(long, value_name = "PATH", num_args = 1.., required = true)]
        input: Vec<PathBuf>,
        /// Where to write the produced `perf-sentinel-report.json`.
        /// Optional under `--tui` (the preview never writes).
        #[cfg_attr(
            feature = "tui",
            arg(long, value_name = "PATH", required_unless_present = "tui")
        )]
        #[cfg_attr(not(feature = "tui"), arg(long, value_name = "PATH", required = true))]
        output: Option<PathBuf>,
        /// Operator-supplied organisation/scope/methodology TOML.
        #[arg(long, value_name = "PATH")]
        org_config: PathBuf,
        /// Refuse to fold windows that have no per-service offenders.
        /// Default is to bucket them under `_unattributed`.
        #[arg(long)]
        strict_attribution: bool,
        /// Optional path for a sidecar in-toto v1 attestation that pins
        /// the report's SHA-256 digest. When set, `disclose` writes a
        /// second JSON file at this path. Designed to feed `cosign
        /// attest` for signed disclosures.
        #[arg(long, value_name = "PATH")]
        emit_attestation: Option<PathBuf>,
        /// Launch the read-only preview TUI: tune the period (month /
        /// quarter / year / custom), intent and confidentiality live, see
        /// the aggregated summary, and copy the equivalent command. Never
        /// writes or hashes a report.
        #[cfg(feature = "tui")]
        #[arg(long, conflicts_with = "emit_attestation")]
        tui: bool,
    },
    /// Verify the integrity of a published periodic disclosure report.
    ///
    /// Recomputes the canonical `content_hash` and, when the report
    /// carries signature/attestation metadata and the operator points
    /// at the matching sidecar files, delegates signature verification
    /// to `cosign verify-blob` and SLSA verification to
    /// `gh attestation verify`.
    #[command(after_help = help_examples::VERIFY_HASH)]
    VerifyHash {
        /// Local report file to verify. Required unless `--url` is set.
        #[arg(long, value_name = "PATH", conflicts_with = "url")]
        report: Option<PathBuf>,
        /// HTTPS URL of a published report. perf-sentinel will also
        /// fetch the sidecar attestation and bundle at the same prefix.
        #[arg(long, value_name = "URL", conflicts_with = "report")]
        url: Option<String>,
        /// Local in-toto v1 attestation file. When omitted in
        /// `--report` mode, signature verification is skipped.
        #[arg(long, value_name = "PATH")]
        attestation: Option<PathBuf>,
        /// Local cosign bundle file. When omitted in `--report` mode,
        /// signature verification is skipped.
        #[arg(long, value_name = "PATH")]
        bundle: Option<PathBuf>,
        /// Output format. Defaults to human-readable text.
        #[arg(long, value_enum, default_value = "text")]
        format: verify_hash::VerifyHashFormat,
        /// Expected OIDC identity that should have signed the report,
        /// e.g. `user@example.com` or
        /// `https://github.com/org/repo/.github/workflows/release.yml@refs/heads/main`.
        /// Required for signature verification unless
        /// `--no-identity-check` is passed.
        #[arg(long, value_name = "ID", conflicts_with = "no_identity_check")]
        expected_identity: Option<String>,
        /// Expected OIDC issuer URL, e.g. `https://accounts.google.com`
        /// or `https://token.actions.githubusercontent.com`. Required
        /// for signature verification unless `--no-identity-check` is
        /// passed.
        #[arg(long, value_name = "URL", conflicts_with = "no_identity_check")]
        expected_issuer: Option<String>,
        /// Opt out of identity verification. Signature is still
        /// cryptographically validated but no constraint is placed on
        /// the signer identity, so a forged bundle can still pass the
        /// check. Use only for internal self-checks.
        #[arg(long)]
        no_identity_check: bool,
    },
    /// Compute and bake the canonical `content_hash` into a periodic report.
    ///
    /// Reads `--report`, recomputes the canonical SHA-256 `content_hash`
    /// using the same signature-stable canonicalization rules that
    /// `disclose` applies, writes it into `integrity.content_hash`, and
    /// saves the result to `--output`. The same path as `--report` is
    /// allowed and bakes in place via an atomic temp+rename. Intended
    /// for test fixture generation and debugging.
    #[command(after_help = help_examples::HASH_BAKE)]
    HashBake {
        /// Local report file to read.
        #[arg(long, value_name = "PATH")]
        report: PathBuf,
        /// Path to write the report with baked `content_hash`. May
        /// equal `--report` for in-place baking.
        #[arg(long, value_name = "PATH")]
        output: PathBuf,
        /// Allow re-baking a report whose `integrity.signature` is
        /// already populated. Re-baking does not invalidate the
        /// signature (`content_hash` blanches signature in canonical
        /// form), but the default refusal guards against unintended
        /// rewrites of signed reports.
        #[arg(long)]
        allow_signed: bool,
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
    /// Interactive TUI with live daemon data. Press `a` on a finding
    /// to acknowledge it via the daemon API, `u` to revoke. The daemon
    /// must have `[daemon.ack] enabled = true` (the default).
    #[cfg(feature = "tui")]
    Inspect {
        /// Path to a file containing the daemon API key (X-API-Key
        /// header). Falls back to `PERF_SENTINEL_DAEMON_API_KEY` env
        /// var. Required when the daemon is configured with
        /// `[daemon.ack] api_key`.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
    },
    /// Live operator monitor: the daemon's settings-advisor hints and
    /// the effective energy/carbon mix (source per service, grid
    /// intensity per region), refreshed on an interval. Read-only,
    /// complements `inspect` (the developer's trace browser).
    #[cfg(feature = "tui")]
    Monitor {
        /// Refresh interval in seconds.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=3600))]
        refresh: u64,
    },
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

/// Usage-example blocks appended under each user-facing command's help
/// via clap `after_help`. Centralized so the examples stay in lockstep with
/// the invocations documented in `docs/CLI.md` and the README. Feature-gated
/// constants mirror their command's `#[cfg]` so no constant goes unused
/// in a `--no-default-features` build.
mod help_examples {
    pub const ANALYZE: &str = "Examples:
  # Gate a CI run and fail on regressions
  perf-sentinel analyze --ci --input traces.json

  # Emit JSON for a dashboard or further processing
  perf-sentinel analyze --input traces.json --format json";

    #[cfg(feature = "daemon")]
    pub const WATCH: &str = "Examples:
  # Run the daemon, listening on all interfaces for containers
  perf-sentinel watch --listen-address 0.0.0.0

  # Load thresholds and detection settings from a config file
  perf-sentinel watch --config .perf-sentinel.toml";

    pub const EXPLAIN: &str = "Examples:
  # Render the annotated span tree for a single trace
  perf-sentinel explain --input traces.json --trace-id abc123def456";

    pub const REPORT: &str = "Examples:
  # Build a self-contained HTML dashboard
  perf-sentinel report --input traces.json --output report.html

  # Add a Diff tab against a baseline report
  perf-sentinel report --input traces.json --output report.html --before baseline.json";

    pub const DIFF: &str = "Examples:
  # Compare a PR against its baseline and emit SARIF for code scanning
  perf-sentinel diff --before base.json --after pr.json --format sarif --output diff.sarif";

    #[cfg(feature = "tempo")]
    pub const TEMPO: &str = "Examples:
  # Fetch and analyze a single trace
  perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123def456

  # Search recent traces for a service
  perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 2h";

    #[cfg(feature = "jaeger-query")]
    pub const JAEGER_QUERY: &str = "Examples:
  # Pull recent traces for a service and analyze them
  perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc";

    pub const CALIBRATE: &str = "Examples:
  # Fit energy coefficients from measured power
  perf-sentinel calibrate --traces traces.json --measured-energy rapl.csv";

    pub const PG_STAT: &str = "Examples:
  # Rank SQL hotspots from a pg_stat_statements export
  perf-sentinel pg-stat --input pg_stat.csv --traces traces.json";

    pub const MYSQL_STAT: &str = "Examples:
  # Rank SQL hotspots from a performance_schema digest export
  perf-sentinel mysql-stat --input digests.csv --traces traces.json";

    // Two variants: the monitor example only exists when the tui
    // feature compiles the subcommand it advertises.
    #[cfg(all(feature = "daemon", feature = "tui"))]
    pub const QUERY: &str = "Examples:
  # List recent findings for a service from a running daemon
  perf-sentinel query findings --service order-svc

  # Show daemon status
  perf-sentinel query status

  # Live operator monitor (advisor hints, energy mix, scraper health)
  perf-sentinel query monitor --refresh 5";

    #[cfg(all(feature = "daemon", not(feature = "tui")))]
    pub const QUERY: &str = "Examples:
  # List recent findings for a service from a running daemon
  perf-sentinel query findings --service order-svc

  # Show daemon status
  perf-sentinel query status";

    #[cfg(feature = "daemon")]
    pub const ACK: &str = "Examples:
  # Acknowledge a finding for one week
  perf-sentinel ack create --signature \"<signature>\" --reason \"deferred to next cycle\" --expires 7d

  # List active daemon acknowledgments
  perf-sentinel ack list";

    pub const DISCLOSE: &str = "Examples:
  # Aggregate a quarter of archived windows into an internal report
  perf-sentinel disclose --intent internal --confidentiality internal --period-type calendar-quarter --from 2026-01-01 --to 2026-03-31 --input /var/lib/perf-sentinel/reports.ndjson --output report.json --org-config org.toml

  # Public report with a signed attestation sidecar
  perf-sentinel disclose --intent official --confidentiality public --period-type calendar-quarter --from 2026-01-01 --to 2026-03-31 --input archive/2026Q1/ --output report.json --emit-attestation report.intoto.jsonl --org-config org.toml";

    pub const VERIFY_HASH: &str = "Examples:
  # Verify a local report and its sidecar signature
  perf-sentinel verify-hash --report report.json --attestation report.intoto.jsonl --bundle report.sig --expected-identity release@example.com --expected-issuer https://accounts.google.com

  # Recompute the content hash of a published report
  perf-sentinel verify-hash --url https://example.com/perf-sentinel-report.json --no-identity-check";

    pub const HASH_BAKE: &str = "Examples:
  # Bake the canonical content hash into a report in place
  perf-sentinel hash-bake --report report.json --output report.json";

    pub const COMPLETIONS: &str = "Examples:
  # Install zsh completions
  perf-sentinel completions zsh > ~/.zfunc/_perf-sentinel

  # Install bash completions
  perf-sentinel completions bash > /usr/local/etc/bash_completion.d/perf-sentinel";

    pub const MAN: &str = "Examples:
  # Install the man page into the system man path
  perf-sentinel man > /usr/local/share/man/man1/perf-sentinel.1";
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

    dispatch_command(Cli::parse().command).await;
}

/// Render the root man page plus one page per subcommand to `out`, so
/// tuning documented only in a subcommand's long help (e.g. the `[daemon]`
/// queue knobs on `watch`) is discoverable from `man`, not just `--help`.
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

/// Dispatch a parsed CLI command to its handler. Lifted out of
/// `main()` so the binary entry point stays focused on tracing init
/// and parsing while the per-subcommand wiring lives here.
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
        } => {
            cmd_watch(
                config.as_deref(),
                listen_address,
                listen_port_http,
                listen_port_grpc,
            )
            .await;
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
            let resolved_auth = resolve_auth_header_or_exit(auth_header, auth_header_env);
            tempo_cmd::cmd_tempo(
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
            let resolved_auth = resolve_auth_header_or_exit(auth_header, auth_header_env);
            jaeger_cmd::cmd_jaeger_query(
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
            pg_stat::dispatch_pg_stat(
                input.as_deref(),
                #[cfg(feature = "daemon")]
                prometheus.as_deref(),
                #[cfg(feature = "daemon")]
                auth_header,
                top_n,
                traces.as_deref(),
                config.as_deref(),
                format,
            )
            .await;
        }
        Commands::MySqlStat {
            input,
            top_n,
            traces,
            config,
            format,
        } => {
            mysql_stat::cmd_mysql_stat(&input, top_n, traces.as_deref(), config.as_deref(), format);
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
            mysql_stat,
            mysql_stat_top,
            acknowledgments,
            no_acknowledgments,
            show_acknowledged,
            #[cfg(feature = "daemon")]
            daemon_url,
        } => {
            #[cfg(feature = "daemon")]
            let daemon_url = validate_daemon_url_or_exit(daemon_url);
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
                mysql_stat.as_deref(),
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
                std::process::exit(1);
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
                format,
                &identity,
            );
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
/// `conflicts_with`; this helper only handles "neither / one / other"
/// and reads the env var when `--auth-header-env` is used.
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
fn resolve_auth_header(
    direct: Option<String>,
    env_var: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(value) = direct {
        // `--auth-header` is `ps`-visible; nudge operators toward
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

/// Resolve the auth header or exit on error. Used by Tempo and
/// Jaeger-Query dispatch arms which both share the same fail-fast
/// shape (`Err(e)` -> `eprintln!("Error: {e}")` -> exit 1).
#[cfg(any(feature = "tempo", feature = "jaeger-query"))]
fn resolve_auth_header_or_exit(direct: Option<String>, env_var: Option<String>) -> Option<String> {
    resolve_auth_header(direct, env_var).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    })
}

#[cfg(feature = "daemon")]
fn validate_daemon_url_or_exit(raw: Option<String>) -> Option<String> {
    match raw {
        Some(s) => match ack::validate_url(&s) {
            Ok(normalized) => Some(normalized),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        None => None,
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
    // Metadata pre-check: reject oversized regular files without reading
    // them. The take() below stays as the defense for special files
    // whose metadata lies (pipes, device files).
    if let Ok(meta) = file.metadata()
        && meta.is_file()
        && meta.len() > max_size
    {
        eprintln!(
            "Error: file {} exceeds maximum of {max_size} bytes",
            path.display()
        );
        std::process::exit(1);
    }
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
    let raw = read_events(input, limits::MAX_BATCH_INPUT_BYTES);

    let events = ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES);
    // Free the raw bytes before analysis: holding a multi-hundred-MB
    // input buffer through the whole pipeline doubles peak RSS.
    drop(raw);

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
    let before_raw = read_events(Some(before), limits::MAX_BATCH_INPUT_BYTES);
    let before_events = ingest_json_or_exit(&before_raw, limits::MAX_BATCH_INPUT_BYTES);
    drop(before_raw);
    let mut before_report = pipeline::analyze(before_events, &config);

    let after_raw = read_events(Some(after), limits::MAX_BATCH_INPUT_BYTES);
    let after_events = ingest_json_or_exit(&after_raw, limits::MAX_BATCH_INPUT_BYTES);
    drop(after_raw);
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

/// Dispatch the `--input` payload by JSON shape: a top-level array goes
/// through the normalize/correlate/detect/score pipeline (native event
/// streams, Zipkin v2), a top-level object is first tried as a
/// pre-computed `Report` (daemon snapshot, baseline file) and falls
/// back to `JsonIngest`, which auto-detects OTLP/JSON and Jaeger.
/// Report-first guarantees a daemon snapshot is never misrouted to the
/// Jaeger ingest even when its payload contains a `"data"` literal in
/// the first 4 KB, at the cost of one extra Report parse on OTLP/Jaeger
/// inputs (rare through this CLI); an OTLP request can never parse as a
/// Report (its required fields are absent). The depth cap is enforced
/// before the Report parse so an over-deep Report does not silently
/// fall through to the ingest fallback. Empty input and scalar roots
/// exit 1 with distinct messages.
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
            let events = ingest_json_or_exit(raw, limits::MAX_BATCH_INPUT_BYTES);
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
            let ingest = JsonIngest::new(limits::MAX_BATCH_INPUT_BYTES);
            match ingest.ingest(raw) {
                Ok(events) => pipeline::analyze_with_traces(events, config),
                Err(e) => {
                    eprintln!(
                        "Error: --input top-level object is neither a pre-computed Report JSON, an OTLP/JSON export, nor a Jaeger export. Underlying error: {e}"
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
    );
    sentinel_core::diff::diff_runs(&baseline, current)
}

#[allow(clippy::too_many_arguments)]
// optional flags, each adds a dedicated ingestion path
// Without the `daemon` feature the only `.await` (the Prometheus pg-stat
// fetch) is compiled out, leaving an async fn with no await, and the
// pg_stat `if let`/`else` collapses to a shape clippy reads as `Option::map`
// even though the `else` carries the daemon-gated Prometheus branch.
#[cfg_attr(
    not(feature = "daemon"),
    allow(clippy::unused_async, clippy::manual_map)
)]
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
    mysql_stat_path: Option<&std::path::Path>,
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
        Some(pg_stat::load_pg_stat_from_file(path, top_n))
    } else {
        #[cfg(feature = "daemon")]
        {
            match pg_stat_prometheus {
                Some(url) => {
                    let resolved_auth = pg_stat::resolve_pg_stat_auth_header(pg_stat_auth_header);
                    Some(
                        pg_stat::load_pg_stat_from_prometheus(
                            url,
                            &config,
                            top_n,
                            resolved_auth.as_deref(),
                        )
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

    // --mysql-stat-top is tied to --mysql-stat by clap `requires`, so
    // no post-parse OR-of-flags validation is needed here.
    let mysql_top_n = mysql_stat_top.unwrap_or(DEFAULT_PG_STAT_TOP);
    let mysql_stat =
        mysql_stat_path.map(|path| mysql_stat::load_mysql_stat_from_file(path, mysql_top_n));

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

fn cmd_calibrate(
    traces_path: &std::path::Path,
    energy_path: &std::path::Path,
    output_path: &std::path::Path,
    config_path: Option<&std::path::Path>,
) {
    // Load (and so validate) --config even though calibrate only needs
    // the trace file: a broken config should fail loudly here too.
    let _config = load_config(config_path);
    let raw = read_events(Some(traces_path), limits::MAX_BATCH_INPUT_BYTES);

    let events = ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES);

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
            std::process::exit(1);
        }
    }
}

/// Print `trace not found` plus up to 20 available trace IDs (with an
/// `... and N more` tail), then exit 1. Shared by `explain` and
/// `explain --tui` so both give the operator the same recovery hint.
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
    std::process::exit(1);
}

fn cmd_explain(
    input: &std::path::Path,
    trace_id: &str,
    config_path: Option<&std::path::Path>,
    format: ExplainFormat,
) {
    let config = load_config(config_path);
    let raw = read_events(Some(input), limits::MAX_BATCH_INPUT_BYTES);

    let events = ingest_json_or_exit(&raw, limits::MAX_BATCH_INPUT_BYTES);

    let normalized = sentinel_core::normalize::normalize_all(events);
    let traces = sentinel_core::correlate::correlate(normalized);

    let Some(trace) = traces.iter().find(|t| t.trace_id == trace_id) else {
        trace_not_found_exit(trace_id, traces.iter().map(|t| t.trace_id.as_str()));
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
        config.daemon.listen_addr = addr;
    }
    if let Some(port) = listen_port_http {
        config.daemon.listen_port = port;
    }
    if let Some(port) = listen_port_grpc {
        config.daemon.listen_port_grpc = port;
    }
    // Re-run strict validation so CLI overrides on listen_addr / ports
    // are checked. Advisory warnings are NOT re-emitted here (they were
    // emitted once at load), only the non-loopback security advisory
    // is re-checked because it is the only one affected by overrides.
    if let Err(e) = config.validate() {
        eprintln!("Error: invalid daemon configuration after CLI overrides: {e}");
        std::process::exit(1);
    }
    config.warn_listen_addr_if_non_loopback();
    info!(
        "Starting daemon: gRPC={}:{}, HTTP={}:{}",
        config.daemon.listen_addr,
        config.daemon.listen_port_grpc,
        config.daemon.listen_addr,
        config.daemon.listen_port,
    );
    if let Err(e) = sentinel_core::daemon::run(config).await {
        eprintln!("Daemon error: {e}");
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
        Report {
            analysis: Analysis {
                duration_ms: 1,
                events_processed: event_count,
                traces_analyzed: 1,
            },
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
            warnings: vec![],
            warning_details: vec![],
            acknowledged_findings: vec![],
            binary_version: String::new(),
            disclosure_waste: None,
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
                ..GreenSummary::disabled(0)
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
            binary_version: String::new(),
            disclosure_waste: None,
        };
        render::format_colored_report(&report, "report", false);
    }

    #[test]
    fn load_config_returns_default_when_no_file() {
        // No .perf-sentinel.toml in the test working directory
        let config = load_config(None);
        assert_eq!(config.detection.n_plus_one_threshold, 5);
        assert_eq!(config.daemon.max_payload_size, 16 * 1024 * 1024);
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
        assert_eq!(config.detection.n_plus_one_threshold, 15);
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
