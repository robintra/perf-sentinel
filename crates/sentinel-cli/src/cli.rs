//! The clap command tree: the `Cli` root, its subcommands and their value enums.

#[cfg(feature = "daemon")]
use crate::ack;
use crate::{disclose, render, verify_hash};
use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "perf-sentinel")]
#[command(about = "Lightweight polyglot performance anti-pattern detector")]
#[command(
    long_about = "Lightweight polyglot performance anti-pattern detector.\n\n\
    All subcommands read tuning from a .perf-sentinel.toml file (--config), not CLI flags. \
    Numbered NN-name.toml fragments in the sibling .perf-sentinel.d directory load first. \
    Batch tuning lives in [thresholds], [detection] and [green] (see `analyze --help`). \
    Daemon tuning lives in [daemon] plus [daemon.correlation|ack|cors|archive] \
    (see `watch --help`). Full reference with defaults and ranges: docs/CONFIGURATION.md."
)]
#[command(version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

/// Output format for the explain command.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ExplainFormat {
    /// Colored terminal tree view (default).
    Text,
    /// Structured JSON tree.
    Json,
}

/// Output format for the analyze command.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum OutputFormat {
    /// Colored terminal report (default for interactive use).
    Text,
    /// Structured JSON report.
    Json,
    /// SARIF v2.1.0 for GitHub/GitLab code scanning.
    Sarif,
}

/// Output format for the pg-stat command.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum PgStatOutputFormat {
    /// Colored terminal table (default).
    Text,
    /// Structured JSON report.
    Json,
}

/// Output format for the mysql-stat command.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum MySqlStatOutputFormat {
    /// Colored terminal table (default).
    Text,
    /// Structured JSON report.
    Json,
}

/// The absolute search window `tempo` and `jaeger-query` share.
///
/// Flattened into both rather than written twice: they are the same two flags,
/// and `conflicts_with_all` resolves against the parent command's own args.
#[derive(Args, Clone, Debug)]
pub(crate) struct AbsoluteWindow {
    /// Start of an absolute search window, ISO 8601 UTC
    /// (e.g. `2026-08-20T15:59:00Z`). Requires --to, conflicts with
    /// --lookback and --trace-id.
    #[arg(long, requires = "to", conflicts_with_all = ["lookback", "trace_id"])]
    pub(crate) from: Option<String>,
    /// End of an absolute search window, ISO 8601 UTC. Requires --from.
    /// A trace ID resolves to exactly one trace, so no window applies to it.
    #[arg(long, requires = "from", conflicts_with_all = ["lookback", "trace_id"])]
    pub(crate) to: Option<String>,
}

/// The `--sort` flag the two backend-query subcommands share.
///
/// Flattened into both rather than written twice, like [`AbsoluteWindow`]:
/// same flag, same help, and one definition cannot drift.
#[derive(Args, Clone, Debug)]
pub(crate) struct SortArg {
    /// Order the findings: impact (highest aggregate avoidable I/O per
    /// signature first) or severity (worst first). Applies to every
    /// format, so `--format json --sort impact` comes out ranked.
    #[arg(long, value_enum, value_name = "KEY")]
    pub(crate) sort: Option<render::FindingsSort>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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
        /// Order the findings: impact (highest aggregate avoidable I/O per
        /// signature first) or severity (worst first). Omit to keep the
        /// canonical detector order in the printed report. `--tui` opens
        /// on this key when given, and on impact otherwise.
        #[arg(long, value_enum, value_name = "KEY")]
        sort: Option<render::FindingsSort>,
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
        /// Override the number of findings carried by one
        /// `/api/export/report` snapshot. Takes precedence over
        /// `[daemon] max_export_findings`. Raise it to report on a busy
        /// daemon, whose store holds far more than one snapshot ships.
        /// Each finding costs a few KB of response body.
        #[arg(long, value_name = "N")]
        max_export_findings: Option<usize>,
    },

    /// Receive OTLP traces into a file for batch analysis, without a
    /// Collector. Point the application at this listener the way it points
    /// at any OTLP endpoint (`OTEL_EXPORTER_OTLP_ENDPOINT`), then feed the
    /// file to `analyze --ci`.
    ///
    /// With a trailing `-- <command>` the capture wraps that command: ports
    /// are bound first, the command inherits stdout and stderr untouched,
    /// and its exit code is propagated. Without one, the capture runs until
    /// SIGINT or SIGTERM, alongside an existing test step.
    #[cfg(feature = "daemon")]
    #[command(after_help = help_examples::CAPTURE)]
    Capture {
        /// Path of the NDJSON trace file to write.
        #[arg(short, long, value_name = "PATH")]
        output: PathBuf,
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1")]
        listen_address: String,
        /// OTLP gRPC listen port.
        #[arg(long, default_value_t = 4317)]
        listen_port_grpc: u16,
        /// OTLP HTTP listen port.
        #[arg(long, default_value_t = 4318)]
        listen_port_http: u16,
        /// Stop appending past this size, in MiB, so a runaway exporter
        /// cannot fill the CI agent's disk.
        #[arg(long, default_value_t = 512)]
        max_file_size: u64,
        /// How long to keep listening after the stop signal or the wrapped
        /// command's exit, in milliseconds. Exporters flush their last batch
        /// at that moment, when the application shuts down.
        #[arg(long, default_value_t = 2000)]
        grace_ms: u64,
        /// Test command to run under capture, after `--`. Its own flags are
        /// passed through untouched, so `-- mvn -X verify` reaches Maven whole.
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
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
    /// (e.g. a daemon snapshot from `/api/export/report`, or
    /// `tempo`/`jaeger-query --format json`). With a Report input the
    /// Findings and Correlations panels light up fully, and the Detail
    /// panel draws the masked span trees the report carries, stubbing
    /// only a trace whose spans it does not.
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
        /// Lookback window for search (e.g. `1h`, `30m`, `7d`).
        #[arg(long, default_value = "1h")]
        lookback: String,
        #[command(flatten)]
        window: AbsoluteWindow,
        /// Maximum number of traces to fetch (1..=10000). The ceiling is
        /// this client's, not Tempo's: it is the largest search response
        /// the ingest is sized to read back.
        #[arg(
            long,
            default_value = "100",
            value_parser = clap::value_parser!(u32)
                .range(1..=sentinel_core::ingest::MAX_SEARCH_TRACES as i64)
        )]
        max_traces: u32,
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
        #[command(flatten)]
        sort: SortArg,
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
        /// Lookback window for search (e.g. `1h`, `30m`, `7d`).
        #[arg(long, default_value = "1h")]
        lookback: String,
        #[command(flatten)]
        window: AbsoluteWindow,
        /// Maximum number of traces to fetch (1..=10000). The same
        /// ceiling as `tempo`, and this client's rather than Jaeger's.
        #[arg(
            long,
            default_value = "100",
            value_parser = clap::value_parser!(u32)
                .range(1..=sentinel_core::ingest::MAX_SEARCH_TRACES as i64)
        )]
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
        #[command(flatten)]
        sort: SortArg,
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
        /// Series holding cumulated execution time on that Prometheus
        /// (default: `pg_stat_statements_seconds_total`, the
        /// `postgres_exporter` built-in query). Set it when the exporter
        /// runs a hand-written query, which names its own columns.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "prometheus")]
        metric: Option<String>,
        /// Label carrying the SQL text on that series (default: `query`).
        /// Without a match the ranking falls back to `queryid`, leaving
        /// opaque identifiers instead of statements.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "LABEL", requires = "prometheus")]
        query_label: Option<String>,
        /// Series holding the call counter (default:
        /// `pg_stat_statements_calls_total`). Fetched in a second query and
        /// joined on `queryid`, because every exporter publishes calls as a
        /// series of its own rather than a label. Pass an empty value to skip
        /// that query: the calls ranking then stays at zero and the mean
        /// ranking repeats the total.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "prometheus")]
        calls_metric: Option<String>,
        /// Unit of the time series: `seconds` (default, the
        /// `postgres_exporter` built-in) or `milliseconds`
        /// (`pg_stat_statements` counts in milliseconds, and a hand-written
        /// exporter query usually forwards that column untouched). Reading
        /// one for the other is off by a factor of a thousand.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "UNIT", requires = "prometheus",
              value_parser = ["seconds", "milliseconds"])]
        unit: Option<String>,
        /// Number of top queries per ranking (default 10).
        #[arg(long, default_value = "10")]
        top_n: usize,
        /// Optional: path to a trace file for cross-referencing with trace findings.
        #[arg(long)]
        traces: Option<PathBuf>,
        /// Earlier `pg_stat_statements` export captured when the trace
        /// window opened. With --traces, compares the call delta between
        /// the two snapshots to the traced span counts on the same
        /// templates and reports an empirical tracing coverage.
        #[arg(long, requires = "traces")]
        baseline: Option<PathBuf>,
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
        input: Option<PathBuf>,
        /// Prometheus endpoint to scrape Performance Schema digests from.
        /// Needs `--collect.perf_schema.eventsstatements` on `mysqld_exporter`,
        /// which is off by default.
        #[cfg(feature = "daemon")]
        #[arg(long)]
        prometheus: Option<String>,
        /// Optional auth header for --prometheus. Example:
        /// --auth-header "Authorization: Bearer ${TOKEN}". Falls back to
        /// the `PERF_SENTINEL_MYSQLSTAT_AUTH_HEADER` env var when unset.
        #[cfg(feature = "daemon")]
        #[arg(long)]
        auth_header: Option<String>,
        /// Series holding cumulated execution time on that Prometheus
        /// (default: `mysql_perf_schema_events_statements_seconds_total`,
        /// the `mysqld_exporter` built-in). Set it when scraping through a
        /// recording rule, which names its own series.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "prometheus")]
        metric: Option<String>,
        /// Label carrying the digest text on that series (default:
        /// `digest_text`). Without a match the report falls back to
        /// `digest`, leaving opaque hashes instead of statements.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "LABEL", requires = "prometheus")]
        query_label: Option<String>,
        /// Label carrying the schema name (default: `schema`). It is part of
        /// the identity every query folds on, so a recording rule that renames
        /// it merges two schemas into one row with their times summed.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "LABEL", requires = "prometheus")]
        schema_label: Option<String>,
        /// Series holding `COUNT_STAR` (default:
        /// `mysql_perf_schema_events_statements_total`). Fetched in a second
        /// query and joined on `digest`, because the exporter publishes the
        /// call count as a series of its own rather than a label. Pass an
        /// empty value to skip that query: the calls ranking then stays at
        /// zero and the mean ranking repeats the total.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "prometheus")]
        calls_metric: Option<String>,
        /// Series holding `SUM_ROWS_SENT` (default:
        /// `mysql_perf_schema_events_statements_rows_sent_total`), fetched and
        /// joined like the call counter. An empty value skips that query and
        /// leaves the column at zero.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "prometheus")]
        rows_sent_metric: Option<String>,
        /// Series holding `SUM_ROWS_EXAMINED` (default:
        /// `mysql_perf_schema_events_statements_rows_examined_total`). An empty
        /// value skips that query, which empties the ranking by rows examined.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "prometheus")]
        rows_examined_metric: Option<String>,
        /// Unit of the time series: `seconds` (default, the
        /// `mysqld_exporter` built-in), `milliseconds`, or `picoseconds`
        /// (Performance Schema counts `SUM_TIMER_WAIT` in picoseconds, and a
        /// recording rule usually forwards that column untouched). Reading
        /// picoseconds as seconds is off by a factor of 10^12.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "UNIT", requires = "prometheus",
              value_parser = ["seconds", "milliseconds", "picoseconds"])]
        unit: Option<String>,
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
    /// are out of scope. Edit the file and ship via PR review instead.
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
    /// Same pipeline as `analyze`. The output is a self-contained HTML
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
        /// Order the findings: impact (highest aggregate avoidable I/O per
        /// signature first, the default) or severity (worst first). This
        /// also decides which span trees `--max-traces-embedded` retains,
        /// since the sink keeps the trees the top findings point at.
        #[arg(long, value_enum, value_name = "KEY")]
        sort: Option<render::FindingsSort>,
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
        /// Series holding cumulated execution time on that Prometheus
        /// (default: `pg_stat_statements_seconds_total`, the
        /// `postgres_exporter` built-in query). Set it when the exporter
        /// runs a hand-written query, which names its own columns.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "pg_stat_prometheus")]
        pg_stat_metric: Option<String>,
        /// Label carrying the SQL text on that series (default: `query`).
        /// Without a match the report falls back to `queryid`, leaving
        /// opaque identifiers instead of statements.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "LABEL", requires = "pg_stat_prometheus")]
        pg_stat_query_label: Option<String>,
        /// Series holding the call counter (default:
        /// `pg_stat_statements_calls_total`). Fetched in a second query and
        /// joined on `queryid`, because every exporter publishes calls as a
        /// series of its own rather than a label. Pass an empty value to skip
        /// that query: the calls ranking then stays at zero and the mean
        /// ranking repeats the total.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "pg_stat_prometheus")]
        pg_stat_calls_metric: Option<String>,
        /// Unit of the time series: `seconds` (default, the
        /// `postgres_exporter` built-in) or `milliseconds`
        /// (`pg_stat_statements` counts in milliseconds, and a hand-written
        /// exporter query usually forwards that column untouched). Reading
        /// one for the other is off by a factor of a thousand.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "UNIT", requires = "pg_stat_prometheus",
              value_parser = ["seconds", "milliseconds"])]
        pg_stat_unit: Option<String>,
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
        /// Prometheus endpoint to scrape Performance Schema digests from,
        /// instead of reading a file. Needs
        /// `--collect.perf_schema.eventsstatements` on `mysqld_exporter`,
        /// which is off by default.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "URL", conflicts_with = "mysql_stat")]
        mysql_stat_prometheus: Option<String>,
        /// Auth header for --mysql-stat-prometheus, as `Name: Value`.
        /// Falls back to `PERF_SENTINEL_MYSQLSTAT_AUTH_HEADER` when unset.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "NAME_VALUE", requires = "mysql_stat_prometheus")]
        mysql_stat_auth_header: Option<String>,
        /// Series holding cumulated execution time on that Prometheus
        /// (default: `mysql_perf_schema_events_statements_seconds_total`).
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "mysql_stat_prometheus")]
        mysql_stat_metric: Option<String>,
        /// Label carrying the digest text on that series (default:
        /// `digest_text`). Without a match the tab falls back to `digest`
        /// and shows opaque hashes instead of statements.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "LABEL", requires = "mysql_stat_prometheus")]
        mysql_stat_query_label: Option<String>,
        /// Label carrying the schema name (default: `schema`). It is part of
        /// the identity every query folds on, so a recording rule that renames
        /// it merges two schemas into one row with their times summed.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "LABEL", requires = "mysql_stat_prometheus")]
        mysql_stat_schema_label: Option<String>,
        /// Series holding `COUNT_STAR` (default:
        /// `mysql_perf_schema_events_statements_total`). Fetched in a second
        /// query and joined on `digest`, because the exporter publishes the
        /// call count as a series of its own rather than a label. Pass an
        /// empty value to skip that query: the calls ranking then stays at
        /// zero and the mean ranking repeats the total.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "mysql_stat_prometheus")]
        mysql_stat_calls_metric: Option<String>,
        /// Series holding `SUM_ROWS_SENT` (default:
        /// `mysql_perf_schema_events_statements_rows_sent_total`), fetched and
        /// joined like the call counter. An empty value skips that query and
        /// leaves the column at zero.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "mysql_stat_prometheus")]
        mysql_stat_rows_sent_metric: Option<String>,
        /// Series holding `SUM_ROWS_EXAMINED` (default:
        /// `mysql_perf_schema_events_statements_rows_examined_total`). An empty
        /// value skips that query, which empties the ranking by rows examined.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "SERIES", requires = "mysql_stat_prometheus")]
        mysql_stat_rows_examined_metric: Option<String>,
        /// Unit of the time series: `seconds` (default, the
        /// `mysqld_exporter` built-in), `milliseconds`, or `picoseconds`
        /// (Performance Schema counts `SUM_TIMER_WAIT` in picoseconds, and a
        /// recording rule usually forwards that column untouched). Reading
        /// picoseconds as seconds is off by a factor of 10^12.
        #[cfg(feature = "daemon")]
        #[arg(long, value_name = "UNIT", requires = "mysql_stat_prometheus",
              value_parser = ["seconds", "milliseconds", "picoseconds"])]
        mysql_stat_unit: Option<String>,
        /// Override the number of top entries per `mysql_stat` ranking
        /// (default: 10). Only meaningful with --mysql-stat or
        /// --mysql-stat-prometheus.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
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
    /// Produce a periodic public disclosure report.
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
        /// Path to the perf-sentinel binary whose SLSA build provenance to
        /// verify via `gh attestation verify` (requires the `gh` CLI and
        /// network access). When omitted, binary attestation metadata is
        /// reported but not verified, and the report cannot reach TRUSTED.
        #[arg(long, value_name = "PATH")]
        verify_binary: Option<PathBuf>,
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
        /// signature (`content_hash` blanks the signature in canonical
        /// form), but the default refusal guards against unintended
        /// rewrites of signed reports.
        #[arg(long)]
        allow_signed: bool,
    },
}

/// Output format for query sub-actions.
#[cfg(feature = "daemon")]
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum QueryOutputFormat {
    /// Colored terminal output (default).
    Text,
    /// Structured JSON.
    Json,
}

/// Sub-actions for the `query` subcommand.
#[cfg(feature = "daemon")]
#[derive(Subcommand)]
pub(crate) enum QueryAction {
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
        /// Order the rows: severity (worst first) or impact (highest
        /// estimated aggregate avoidable I/O first, `seen_count` x the
        /// representative detection's avoidable ops). Text output only,
        /// omit to keep the daemon's newest-first order.
        #[arg(long, value_enum, value_name = "KEY")]
        sort: Option<render::FindingsSort>,
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
        /// header). `PERF_SENTINEL_DAEMON_API_KEY` wins over it when
        /// set. Required when the daemon is configured with
        /// `[daemon.ack] api_key`.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
        /// Open the trace and finding lists in this order: impact
        /// (highest aggregate avoidable I/O first) or severity (worst
        /// first). Same keys as `analyze --sort`. Omit to open on
        /// impact, and `s` cycles through both plus trace id order.
        #[arg(long, value_enum, value_name = "KEY")]
        sort: Option<render::FindingsSort>,
    },
    /// Live operator monitor: the daemon's settings-advisor hints, the
    /// effective energy/carbon mix (source per service, grid intensity
    /// per region), scraper health, the daemon config and the recorded
    /// incidents, refreshed on an interval. Read-only, complements
    /// `inspect` (the developer's trace browser).
    #[cfg(feature = "tui")]
    Monitor {
        /// Refresh interval in seconds.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=3600))]
        refresh: u64,
        /// Path to a file containing the daemon API key (X-API-Key
        /// header). `PERF_SENTINEL_DAEMON_API_KEY` wins over it when
        /// set. Only the Incidents tab needs it, and the read-only
        /// `[daemon] read_api_key` suffices there.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
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
    /// List the incidents the alerting posted to the daemon, newest
    /// first, each with the findings frozen from the window before it
    /// (daemon 0.20.0+, `[daemon.incidents] enabled = true`).
    Incidents {
        /// Only the incidents of this service (exact match).
        #[arg(long)]
        service: Option<String>,
        /// Only the incidents of this namespace (exact match), the alert
        /// label the daemon carries as `namespace`.
        #[arg(long)]
        namespace: Option<String>,
        /// Skip this many incidents, to page past the newest.
        #[arg(long, default_value = "0")]
        offset: usize,
        /// Maximum number of incidents (default 50, the daemon caps at 100).
        // Refused at zero rather than forwarded: the daemon would answer an
        // empty array and the text renderer would print the sentence a quiet
        // daemon prints. The upper bound stays the daemon's to apply, so a
        // later release can raise it without a new CLI.
        #[arg(
            long,
            default_value = "50",
            value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..)
        )]
        limit: usize,
        /// Output format: text (colored, default) or json.
        #[arg(long, value_enum, default_value = "text")]
        format: QueryOutputFormat,
        /// Path to a file containing the daemon API key (X-API-Key
        /// header). `PERF_SENTINEL_DAEMON_API_KEY` wins over it when
        /// set. The read-only `[daemon] read_api_key` suffices.
        #[arg(long, value_name = "PATH")]
        api_key_file: Option<PathBuf>,
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

    #[cfg(feature = "daemon")]
    pub const CAPTURE: &str = "Examples:
  # Wrap the test step: ports are up before it starts, its exit code is kept
  perf-sentinel capture --output traces.json -- mvn verify
  perf-sentinel analyze --ci --input traces.json

  # Any test command works, capture just starts a process
  perf-sentinel capture --output traces.json -- pytest tests/integration
  perf-sentinel capture --output traces.json -- npm run test:e2e
  perf-sentinel capture --output traces.json -- go test ./...

  # Alongside an existing test step, stopped with a signal
  perf-sentinel capture --output traces.json &
  ./scripts/run-integration-tests.sh
  kill %1

  # The application only needs the standard OTLP endpoint variables
  export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
  export OTEL_EXPORTER_OTLP_PROTOCOL=grpc";

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
  perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 2h

  # Re-read the exact window an incident happened in
  perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc \\
    --from 2026-08-20T15:00:00Z --to 2026-08-20T16:00:00Z";

    #[cfg(feature = "jaeger-query")]
    pub const JAEGER_QUERY: &str = "Examples:
  # Pull recent traces for a service and analyze them
  perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc

  # Re-read the exact window an incident happened in
  perf-sentinel jaeger-query --endpoint http://victoria:10428 --service order-svc \\
    --from 2026-08-20T15:00:00Z --to 2026-08-20T16:00:00Z";

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
  perf-sentinel query monitor --refresh 5

  # The incidents the alerting posted, with the findings of their window
  perf-sentinel query incidents --service cart-svc --api-key-file /run/secrets/read-key";

    #[cfg(all(feature = "daemon", not(feature = "tui")))]
    pub const QUERY: &str = "Examples:
  # List recent findings for a service from a running daemon
  perf-sentinel query findings --service order-svc

  # Show daemon status
  perf-sentinel query status

  # The incidents the alerting posted, with the findings of their window
  perf-sentinel query incidents --service cart-svc --api-key-file /run/secrets/read-key";

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
