//! Ingestion for `PostgreSQL` `pg_stat_statements` data.
//!
//! Parses CSV or JSON exports of `pg_stat_statements` into a `PgStatReport`
//! with top-N rankings by total execution time, call count, and mean execution time.
//!
//! Unlike trace-based ingestion, `pg_stat_statements` has no `trace_id`, it provides
//! a complementary view of SQL hotspots at the database level.

use crate::detect::Finding;
use crate::normalize::sql::normalize_sql;
use serde::{Deserialize, Serialize};

/// A single entry from `pg_stat_statements`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PgStatEntry {
    /// Original query text (as normalized by `PostgreSQL`).
    pub query: String,
    /// Template after `perf-sentinel` SQL normalization.
    pub normalized_template: String,
    /// Number of times the query was executed.
    pub calls: u64,
    /// Total execution time in milliseconds.
    pub total_exec_time_ms: f64,
    /// Mean execution time in milliseconds.
    pub mean_exec_time_ms: f64,
    /// Total rows returned or affected.
    pub rows: u64,
    /// Number of shared buffer hits.
    pub shared_blks_hit: u64,
    /// Number of shared buffer reads (cache misses).
    pub shared_blks_read: u64,
    /// Whether this template was also seen in trace-based findings.
    #[serde(default)]
    pub seen_in_traces: bool,
}

/// A ranking of `pg_stat_statements` entries by a specific criterion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PgStatRanking {
    /// Label describing the ranking criterion (e.g., "top by `total_exec_time`").
    pub label: String,
    /// Entries sorted by the criterion, limited to `top_n`.
    pub entries: Vec<PgStatEntry>,
}

/// Report produced from `pg_stat_statements` analysis.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PgStatReport {
    /// Total number of entries parsed.
    pub total_entries: usize,
    /// Number of top entries per ranking.
    pub top_n: usize,
    /// Rankings in a stable order: by `total_exec_time`, by `calls`,
    /// by `mean_exec_time`, by `shared_blks_total` (cache hits + reads).
    /// Consumers that index by position (e.g., the HTML dashboard's
    /// `pg_stat` sub-switcher) rely on this ordering not changing. New
    /// rankings are appended, existing indices are never reassigned.
    pub rankings: Vec<PgStatRanking>,
}

/// Detected input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgStatFormat {
    Csv,
    Json,
}

/// Errors that can occur during `pg_stat_statements` parsing.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PgStatError {
    #[error("payload too large: {size} bytes exceeds maximum of {max} bytes")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("CSV parse error at line {line}: {detail}")]
    CsvParse { line: usize, detail: String },
    #[error("JSON parse error: {0}")]
    JsonParse(String),
    #[error("missing required column: {0}")]
    MissingColumn(String),
    #[error("empty input")]
    EmptyInput,
    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[error("Prometheus request failed: {0}")]
    PrometheusRequest(String),
    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[error("Prometheus response parse error: {0}")]
    PrometheusFormat(String),
}

/// Raw JSON entry matching common `pg_stat_statements` export formats.
#[derive(Deserialize)]
struct RawJsonEntry {
    query: String,
    calls: u64,
    #[serde(alias = "total_exec_time")]
    total_exec_time_ms: f64,
    #[serde(alias = "mean_exec_time")]
    mean_exec_time_ms: f64,
    #[serde(default)]
    rows: u64,
    #[serde(default)]
    shared_blks_hit: u64,
    #[serde(default)]
    shared_blks_read: u64,
}

/// Detect whether the input is CSV or JSON.
///
/// Peeks at the first non-whitespace byte: `[` or `{` indicates JSON,
/// otherwise CSV. Returns `Csv` as fallback for empty input; the caller
/// should validate non-emptiness separately.
#[must_use]
pub fn detect_pg_stat_format(raw: &[u8]) -> PgStatFormat {
    let trimmed = raw.iter().position(|&b| !b.is_ascii_whitespace());
    match trimmed.map(|i| raw[i]) {
        Some(b'[' | b'{') => PgStatFormat::Json,
        _ => PgStatFormat::Csv,
    }
}

/// Parse `pg_stat_statements` data from raw bytes.
///
/// Auto-detects CSV vs JSON format. Normalizes each query through
/// the SQL normalizer for consistency with trace-based analysis.
///
/// # Errors
///
/// Returns an error if the payload exceeds `max_size`, the input is empty,
/// or parsing fails.
pub fn parse_pg_stat(raw: &[u8], max_size: usize) -> Result<Vec<PgStatEntry>, PgStatError> {
    if raw.len() > max_size {
        return Err(PgStatError::PayloadTooLarge {
            size: raw.len(),
            max: max_size,
        });
    }
    if raw.is_empty() || raw.iter().all(|&b| b.is_ascii_whitespace()) {
        return Err(PgStatError::EmptyInput);
    }

    let text = std::str::from_utf8(raw).map_err(|e| PgStatError::CsvParse {
        line: 0,
        detail: format!("invalid UTF-8: {e}"),
    })?;

    match detect_pg_stat_format(raw) {
        PgStatFormat::Csv => parse_csv(text),
        PgStatFormat::Json => parse_json(text),
    }
}

/// Generate rankings from parsed entries.
///
/// Produces four rankings in a stable order: by total execution time,
/// by call count, by mean execution time, by total shared buffer
/// blocks touched (`shared_blks_read + shared_blks_hit`). Each ranking
/// contains at most `top_n` entries.
///
/// Uses index-based sorting so the full `entries` slice is never
/// cloned during the sort. Each ranking still clones its own `top_n`
/// entries because `PgStatRanking.entries: Vec<PgStatEntry>` is owned
/// data on the public Serialize surface. Four rankings times `top_n`
/// (defaults to 100) gives about 400 small-struct clones per call,
/// which is acceptable because `pg_stat` ingestion is one-shot (CLI
/// batch or daemon-load path), never on the per-event hot path.
/// If `top_n` grows past a few thousand or the call is moved into a
/// hot path, switch to an `Arc<PgStatEntry>` refcount shared across
/// rankings to reclaim the duplicate allocations.
///
/// Downstream consumers (the HTML dashboard's `pg_stat` sub-switcher
/// in particular) rely on the rankings appearing at the documented
/// positions, new rankings are always appended and existing indices
/// never reassign.
#[must_use]
pub fn rank_pg_stat(entries: &[PgStatEntry], top_n: usize) -> PgStatReport {
    let total_entries = entries.len();

    let top_n_by =
        |cmp: fn(&PgStatEntry, &PgStatEntry) -> std::cmp::Ordering, label: &str| -> PgStatRanking {
            let mut indices: Vec<usize> = (0..entries.len()).collect();
            indices.sort_by(|&a, &b| cmp(&entries[a], &entries[b]));
            indices.truncate(top_n);
            PgStatRanking {
                label: label.to_string(),
                entries: indices.iter().map(|&i| entries[i].clone()).collect(),
            }
        };

    let by_total_time = top_n_by(
        |a, b| {
            b.total_exec_time_ms
                .partial_cmp(&a.total_exec_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        },
        "top by total_exec_time",
    );

    let by_calls = top_n_by(|a, b| b.calls.cmp(&a.calls), "top by calls");

    let by_mean_time = top_n_by(
        |a, b| {
            b.mean_exec_time_ms
                .partial_cmp(&a.mean_exec_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        },
        "top by mean_exec_time",
    );

    // Total shared buffer blocks touched = hits + reads. Highest first
    // identifies queries that move the most data through the cache
    // regardless of whether they hit or miss, which correlates with
    // memory pressure better than raw call count.
    let by_io_blocks = top_n_by(
        |a, b| {
            let bt = b.shared_blks_read.saturating_add(b.shared_blks_hit);
            let at = a.shared_blks_read.saturating_add(a.shared_blks_hit);
            bt.cmp(&at)
        },
        "top by shared_blks_total",
    );

    PgStatReport {
        total_entries,
        top_n,
        rankings: vec![by_total_time, by_calls, by_mean_time, by_io_blocks],
    }
}

/// Cross-reference `pg_stat_statements` entries with trace-based findings.
///
/// Marks entries whose `normalized_template` matches any finding's pattern template.
pub fn cross_reference(entries: &mut [PgStatEntry], findings: &[Finding]) {
    let templates: std::collections::HashSet<&str> = findings
        .iter()
        .map(|f| f.pattern.template.as_str())
        .collect();

    for entry in entries {
        if templates.contains(entry.normalized_template.as_str()) {
            entry.seen_in_traces = true;
        }
    }
}

// ---------------------------------------------------------------------------
// CSV parsing (RFC 4180 subset)
// ---------------------------------------------------------------------------

const MAX_CSV_ROWS: usize = 1_000_000;

fn parse_csv(text: &str) -> Result<Vec<PgStatEntry>, PgStatError> {
    let mut lines = text.lines();

    let header_line = lines.next().ok_or(PgStatError::EmptyInput)?;
    let headers = parse_csv_row(header_line);
    let col = |name: &str| -> Result<usize, PgStatError> {
        headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(name))
            .ok_or_else(|| PgStatError::MissingColumn(name.to_string()))
    };

    let query_idx = col("query")?;
    let calls_idx = col("calls")?;
    let total_time_idx = col("total_exec_time")?;
    let mean_time_idx = col("mean_exec_time")?;
    let rows_idx = col("rows").ok();
    let hit_idx = col("shared_blks_hit").ok();
    let read_idx = col("shared_blks_read").ok();

    // Estimate row count from byte length (~100 bytes per row), capped at 100k entries
    let estimated = (text.len() / 100).min(100_000);
    let mut entries = Vec::with_capacity(estimated);
    for (line_num, line) in lines.enumerate() {
        if entries.len() >= MAX_CSV_ROWS {
            return Err(PgStatError::CsvParse {
                line: line_num + 2,
                detail: format!("CSV exceeds maximum of {MAX_CSV_ROWS} rows"),
            });
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields = parse_csv_row(line);
        let line_num = line_num + 2; // 1-indexed, header is line 1

        let query = fields.get(query_idx).cloned().unwrap_or_default();
        let calls = parse_u64(&fields, calls_idx, line_num, "calls")?;
        let total_exec_time_ms = parse_f64(&fields, total_time_idx, line_num, "total_exec_time")?;
        let mean_exec_time_ms = parse_f64(&fields, mean_time_idx, line_num, "mean_exec_time")?;
        let rows = rows_idx.map_or(Ok(0), |i| parse_u64(&fields, i, line_num, "rows"))?;
        let shared_blks_hit = hit_idx.map_or(Ok(0), |i| {
            parse_u64(&fields, i, line_num, "shared_blks_hit")
        })?;
        let shared_blks_read = read_idx.map_or(Ok(0), |i| {
            parse_u64(&fields, i, line_num, "shared_blks_read")
        })?;

        let normalized = normalize_sql(&query);

        entries.push(PgStatEntry {
            query,
            normalized_template: normalized.template,
            calls,
            total_exec_time_ms,
            mean_exec_time_ms,
            rows,
            shared_blks_hit,
            shared_blks_read,
            seen_in_traces: false,
        });
    }

    if entries.is_empty() {
        return Err(PgStatError::EmptyInput);
    }
    Ok(entries)
}

/// Parse a single CSV row, handling double-quoted fields.
///
/// Iterates over chars (not bytes) to correctly handle multi-byte UTF-8 content
/// in query strings. Only ASCII delimiters (`"` and `,`) receive special treatment.
/// `pub(crate)`: shared with the `mysql_stat` CSV parser.
pub(crate) fn parse_csv_row(line: &str) -> Vec<String> {
    let mut fields = Vec::with_capacity(8);
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    // Escaped quote
                    current.push('"');
                    chars.next();
                } else {
                    // End of quoted field
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

fn parse_u64(
    fields: &[String],
    idx: usize,
    line: usize,
    col_name: &str,
) -> Result<u64, PgStatError> {
    let val = fields.get(idx).map_or("", String::as_str).trim();
    val.parse::<u64>().map_err(|_| PgStatError::CsvParse {
        line,
        detail: format!("cannot parse '{val}' as integer for column {col_name}"),
    })
}

fn parse_f64(
    fields: &[String],
    idx: usize,
    line: usize,
    col_name: &str,
) -> Result<f64, PgStatError> {
    let val = fields.get(idx).map_or("", String::as_str).trim();
    val.parse::<f64>().map_err(|_| PgStatError::CsvParse {
        line,
        detail: format!("cannot parse '{val}' as float for column {col_name}"),
    })
}

// ---------------------------------------------------------------------------
// JSON parsing
// ---------------------------------------------------------------------------

fn parse_json(text: &str) -> Result<Vec<PgStatEntry>, PgStatError> {
    let raw_entries: Vec<RawJsonEntry> =
        serde_json::from_str(text).map_err(|e| PgStatError::JsonParse(e.to_string()))?;

    if raw_entries.is_empty() {
        return Err(PgStatError::EmptyInput);
    }
    if raw_entries.len() > MAX_CSV_ROWS {
        return Err(PgStatError::JsonParse(format!(
            "JSON array exceeds maximum of {MAX_CSV_ROWS} entries (got {})",
            raw_entries.len()
        )));
    }

    let entries = raw_entries
        .into_iter()
        .map(|raw| {
            let normalized = normalize_sql(&raw.query);
            PgStatEntry {
                query: raw.query,
                normalized_template: normalized.template,
                calls: raw.calls,
                total_exec_time_ms: raw.total_exec_time_ms,
                mean_exec_time_ms: raw.mean_exec_time_ms,
                rows: raw.rows,
                shared_blks_hit: raw.shared_blks_hit,
                shared_blks_read: raw.shared_blks_read,
                seen_in_traces: false,
            }
        })
        .collect();

    Ok(entries)
}

// ── Prometheus scrape path ─────────────────────────────────────────

/// Unit of the ranked time series.
///
/// `pg_stat_statements` counts in milliseconds. The `postgres_exporter`
/// built-in query converts to seconds, a hand-written one usually forwards
/// the column untouched, and reading one for the other is off by a thousand.
#[cfg(any(feature = "daemon", feature = "tempo"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PgStatTimeUnit {
    /// `pg_stat_statements_seconds_total`, the exporter built-in.
    #[default]
    Seconds,
    /// `total_exec_time` forwarded as is.
    Milliseconds,
}

/// Which series and label carry `pg_stat_statements` on a given Prometheus.
///
/// The defaults match the `postgres_exporter` built-in query. A deployment
/// that publishes its own hand-written query names its own columns, and the
/// exporter derives the series from them, so neither name is ours to assume.
///
/// `#[non_exhaustive]` so a future knob stays a minor bump rather than a
/// breaking change: external crates build it with [`Self::default`] or
/// [`Self::with_overrides`], never a struct literal.
#[cfg(any(feature = "daemon", feature = "tempo"))]
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PrometheusPgStat {
    /// Series ranked by `topk`, holding the cumulated execution time in seconds.
    pub series: String,
    /// Label carrying the SQL text. Falls back to `queryid` when absent, which
    /// leaves the report with opaque identifiers rather than statements.
    pub query_label: String,
    /// Series holding the call counter. Every exporter publishes it as a
    /// series of its own, never as a label, so it takes a second query joined
    /// on `queryid`. `None` skips that query and leaves the counts at zero,
    /// which empties the ranking by calls and collapses the mean onto the
    /// total.
    pub calls_series: Option<String>,
    /// Unit of [`Self::series`].
    pub unit: PgStatTimeUnit,
}

#[cfg(any(feature = "daemon", feature = "tempo"))]
impl Default for PrometheusPgStat {
    fn default() -> Self {
        Self {
            series: "pg_stat_statements_seconds_total".to_string(),
            query_label: "query".to_string(),
            calls_series: Some("pg_stat_statements_calls_total".to_string()),
            unit: PgStatTimeUnit::Seconds,
        }
    }
}

#[cfg(any(feature = "daemon", feature = "tempo"))]
impl PrometheusPgStat {
    /// Apply optional CLI overrides on top of the `postgres_exporter`
    /// defaults. Keeps the default names in one place, so a caller that
    /// overrides neither reproduces the pre-0.12.0 behavior exactly.
    #[must_use]
    pub fn with_overrides(series: Option<String>, query_label: Option<String>) -> Self {
        let defaults = Self::default();
        Self {
            series: series.unwrap_or(defaults.series),
            query_label: query_label.unwrap_or(defaults.query_label),
            calls_series: defaults.calls_series,
            unit: defaults.unit,
        }
    }
}

/// Reject a series name that is not a bare `PromQL` metric name.
///
/// Delegates to the shared guard so `pg-stat` and `mysql-stat` cannot drift
/// on what they accept in a query string.
#[cfg(any(feature = "daemon", feature = "tempo"))]
fn validate_series_name(series: &str) -> Result<(), PgStatError> {
    crate::ingest::prometheus_scrape::validate_series_name(series)
        .map_err(PgStatError::PrometheusRequest)
}

/// Identity of a `pg_stat_statements` row. `postgres_exporter` also labels by
/// `datname` and `user`, which the report does not carry: both series fold on
/// this key so the time and the call count describe the same rows.
#[cfg(any(feature = "daemon", feature = "tempo"))]
const PG_IDENTITY: &[&str] = &["queryid"];

/// Build the `topk` instant query, aggregated on the statement identity plus
/// the label carrying its text.
#[cfg(any(feature = "daemon", feature = "tempo"))]
fn build_prometheus_query(top_n: usize, opts: &PrometheusPgStat) -> String {
    crate::ingest::prometheus_scrape::build_topk_query(
        top_n,
        &opts.series,
        &["queryid", opts.query_label.as_str()],
    )
}

/// Fetch `pg_stat_statements` data from a Prometheus endpoint.
///
/// Queries the Prometheus HTTP API for the series named by `opts`
/// (defaulting to the `postgres_exporter` built-in
/// `pg_stat_statements_seconds_total`), converts the result to
/// [`PgStatEntry`] structs, and normalizes SQL templates.
///
/// When `auth_header` is `Some`, the `"Name: Value"` string is parsed
/// once via [`crate::ingest::auth_header::AuthHeader::parse`] and the
/// resulting header is attached to the outbound request. Required for
/// Grafana Cloud, Grafana Mimir and any Prometheus ingress enforcing
/// bearer/basic auth.
///
/// # Errors
///
/// Returns [`PgStatError::PrometheusRequest`] on transport errors,
/// invalid auth headers, or auth-over-cleartext warnings, and
/// [`PgStatError::PrometheusFormat`] if the response cannot be parsed.
#[cfg(any(feature = "daemon", feature = "tempo"))]
pub async fn fetch_from_prometheus(
    endpoint: &str,
    top_n: usize,
    auth_header: Option<&str>,
    opts: &PrometheusPgStat,
) -> Result<Vec<PgStatEntry>, PgStatError> {
    // Validate the endpoint and the operator-supplied series before issuing
    // the request, through the guards `mysql-stat` shares.
    validate_prometheus_endpoint(endpoint)?;
    validate_series_name(&opts.series)?;
    // The query label now lands in the `sum by (...)` clause, not only in the
    // response it is read back from.
    crate::ingest::prometheus_scrape::validate_label_name(&opts.query_label)
        .map_err(PgStatError::PrometheusRequest)?;
    // The call-counter series lands in the same query string, unencoded, so it
    // needs the same guard: an operator typo carrying `&` or `#` would smuggle
    // a second parameter into the URL rather than fail.
    if let Some(series) = opts.calls_series.as_deref() {
        validate_series_name(series)?;
    }

    let query = build_prometheus_query(top_n, opts);
    let body = crate::ingest::prometheus_scrape::fetch_instant_query(
        endpoint,
        &query,
        auth_header,
        "perf-sentinel/pg-stat",
    )
    .await
    .map_err(PgStatError::PrometheusRequest)?;

    // Second query for the call counter. A failure here is not fatal: the
    // timings are already in hand, and a report ranked by time beats no
    // report at all. The counts stay at zero and the CLI says so.
    let calls_body = match opts.calls_series.as_deref() {
        Some(series) => {
            let calls_query = crate::ingest::prometheus_scrape::build_counter_query(
                series,
                PG_IDENTITY,
                "queryid",
                &query,
            );
            match crate::ingest::prometheus_scrape::fetch_instant_query(
                endpoint,
                &calls_query,
                auth_header,
                "perf-sentinel/pg-stat",
            )
            .await
            {
                Ok(raw) => Some(raw),
                Err(e) => {
                    tracing::warn!(
                        series,
                        error = %e,
                        "call-count series unavailable; the calls ranking stays at zero \
                         and the mean ranking repeats the total"
                    );
                    None
                }
            }
        }
        None => None,
    };

    parse_prometheus_response(&body, calls_body.as_deref(), opts)
}

/// Validate a user-supplied Prometheus endpoint string.
///
/// Rejects URLs that:
/// - fail to parse as a hyper `Uri`
/// - have a scheme other than `http` or `https`
/// - carry userinfo (credentials in the authority, e.g. `user:pass@host`)
///   since credentials must flow via env vars or a `.pgpass`-style file
#[cfg(any(feature = "daemon", feature = "tempo"))]
fn validate_prometheus_endpoint(endpoint: &str) -> Result<(), PgStatError> {
    crate::ingest::prometheus_scrape::validate_endpoint(endpoint)
        .map_err(PgStatError::PrometheusRequest)
}

/// Parse a Prometheus instant query response into `PgStatEntry` structs.
#[cfg(any(feature = "daemon", feature = "tempo"))]
fn parse_prometheus_response(
    body: &[u8],
    calls_body: Option<&[u8]>,
    opts: &PrometheusPgStat,
) -> Result<Vec<PgStatEntry>, PgStatError> {
    // identity -> calls, empty when no second query was made.
    let call_counts = match calls_body {
        Some(raw) => crate::ingest::prometheus_scrape::counter_by_labels(raw, PG_IDENTITY)
            .map_err(PgStatError::PrometheusFormat)?,
        None => std::collections::HashMap::new(),
    };
    let json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| PgStatError::PrometheusFormat(format!("invalid JSON: {e}")))?;

    let results = json
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array())
        .ok_or_else(|| PgStatError::PrometheusFormat("missing data.result array".to_string()))?;

    let mut entries = Vec::with_capacity(results.len());
    for result in results {
        let metric = result.get("metric").unwrap_or(&serde_json::Value::Null);
        let query_text = metric
            .get(opts.query_label.as_str())
            .or_else(|| metric.get("queryid"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // value is [timestamp, "string_value"]
        let value = result
            .get("value")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.get(1))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let total_exec_time_ms = match opts.unit {
            PgStatTimeUnit::Seconds => value * 1000.0,
            PgStatTimeUnit::Milliseconds => value,
        };

        // Prometheus label values are always strings. `.as_str() + parse` is
        // the correct path; the previous `.as_u64().map(|_| "")` branch was
        // dead code that silently produced 0 for non-string values.
        // A label named `calls` is not a thing any exporter publishes; the
        // count comes from the joined series, on the same identity both
        // queries aggregated by.
        let calls = crate::ingest::prometheus_scrape::identity_key(metric, PG_IDENTITY)
            .and_then(|key| call_counts.get(&key).copied())
            .unwrap_or(0);

        #[allow(clippy::cast_precision_loss)]
        let mean_exec_time_ms = if calls > 0 {
            total_exec_time_ms / (calls as f64)
        } else {
            total_exec_time_ms
        };

        let normalized = normalize_sql(&query_text);

        entries.push(PgStatEntry {
            query: query_text,
            normalized_template: normalized.template,
            calls,
            total_exec_time_ms,
            mean_exec_time_ms,
            rows: 0,
            shared_blks_hit: 0,
            shared_blks_read: 0,
            seen_in_traces: false,
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    fn sample_csv() -> &'static str {
        "query,calls,total_exec_time,mean_exec_time,rows,shared_blks_hit,shared_blks_read\n\
         SELECT * FROM order_item WHERE order_id = 42,1500,4500.50,3.000,1500,12000,150\n\
         \"SELECT * FROM orders WHERE id = 1 AND status = 'active'\",800,2400.00,3.000,800,6400,80\n\
         INSERT INTO audit_log VALUES (1),200,600.00,3.000,200,0,200\n\
         SELECT count(*) FROM order_item,50,250.00,5.000,50,500,10"
    }

    fn sample_json() -> &'static str {
        r#"[
            {
                "query": "SELECT * FROM order_item WHERE order_id = 42",
                "calls": 1500,
                "total_exec_time_ms": 4500.50,
                "mean_exec_time_ms": 3.0,
                "rows": 1500,
                "shared_blks_hit": 12000,
                "shared_blks_read": 150
            },
            {
                "query": "SELECT * FROM orders WHERE id = 1 AND status = 'active'",
                "calls": 800,
                "total_exec_time_ms": 2400.0,
                "mean_exec_time_ms": 3.0,
                "rows": 800,
                "shared_blks_hit": 6400,
                "shared_blks_read": 80
            },
            {
                "query": "INSERT INTO audit_log VALUES (1)",
                "calls": 200,
                "total_exec_time_ms": 600.0,
                "mean_exec_time_ms": 3.0,
                "rows": 200,
                "shared_blks_hit": 0,
                "shared_blks_read": 200
            },
            {
                "query": "SELECT count(*) FROM order_item",
                "calls": 50,
                "total_exec_time_ms": 250.0,
                "mean_exec_time_ms": 5.0,
                "rows": 50,
                "shared_blks_hit": 500,
                "shared_blks_read": 10
            }
        ]"#
    }

    // -- Format detection --

    #[test]
    fn detect_format_csv() {
        assert_eq!(
            detect_pg_stat_format(b"query,calls,total_exec_time"),
            PgStatFormat::Csv
        );
    }

    #[test]
    fn detect_format_json_array() {
        assert_eq!(
            detect_pg_stat_format(b"[{\"query\": \"SELECT 1\"}]"),
            PgStatFormat::Json
        );
    }

    #[test]
    fn detect_format_json_with_whitespace() {
        assert_eq!(
            detect_pg_stat_format(b"  \n  [{\"query\": \"SELECT 1\"}]"),
            PgStatFormat::Json
        );
    }

    #[test]
    fn detect_format_empty_defaults_csv() {
        assert_eq!(detect_pg_stat_format(b""), PgStatFormat::Csv);
    }

    // -- CSV parsing --

    #[test]
    fn parse_csv_basic() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].calls, 1500);
        assert!((entries[0].total_exec_time_ms - 4500.50).abs() < f64::EPSILON);
        assert_eq!(entries[0].rows, 1500);
        assert_eq!(entries[0].shared_blks_hit, 12000);
    }

    #[test]
    fn parse_csv_quoted_field() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        // Second entry has a quoted query with comma-free content but single quotes
        assert!(entries[1].query.contains("status = 'active'"));
    }

    #[test]
    fn parse_csv_normalization_applied() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        // order_id = 42 -> order_id = ?
        assert_eq!(
            entries[0].normalized_template,
            "SELECT * FROM order_item WHERE order_id = ?"
        );
    }

    #[test]
    fn parse_csv_empty_input() {
        let result = parse_pg_stat(b"", 1_048_576);
        assert_matches!(result, Err(PgStatError::EmptyInput));
    }

    #[test]
    fn parse_csv_whitespace_only() {
        let result = parse_pg_stat(b"  \n  \n  ", 1_048_576);
        assert_matches!(result, Err(PgStatError::EmptyInput));
    }

    #[test]
    fn parse_csv_header_only() {
        let result = parse_pg_stat(b"query,calls,total_exec_time,mean_exec_time\n", 1_048_576);
        assert_matches!(result, Err(PgStatError::EmptyInput));
    }

    #[test]
    fn parse_csv_missing_column() {
        let result = parse_pg_stat(b"query,calls\nSELECT 1,100", 1_048_576);
        assert_matches!(result, Err(PgStatError::MissingColumn(_)));
    }

    #[test]
    fn parse_csv_oversized_payload() {
        let result = parse_pg_stat(sample_csv().as_bytes(), 10);
        assert_matches!(result, Err(PgStatError::PayloadTooLarge { .. }));
    }

    #[test]
    fn parse_csv_escaped_quotes() {
        let csv = "query,calls,total_exec_time,mean_exec_time\n\
                   \"SELECT * FROM t WHERE name = \"\"O'Brien\"\"\",100,500.0,5.0";
        let entries = parse_pg_stat(csv.as_bytes(), 1_048_576).unwrap();
        assert!(entries[0].query.contains("O'Brien"));
    }

    // -- JSON parsing --

    #[test]
    fn parse_json_basic() {
        let entries = parse_pg_stat(sample_json().as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].calls, 1500);
    }

    #[test]
    fn parse_json_normalization_applied() {
        let entries = parse_pg_stat(sample_json().as_bytes(), 1_048_576).unwrap();
        assert_eq!(
            entries[0].normalized_template,
            "SELECT * FROM order_item WHERE order_id = ?"
        );
    }

    #[test]
    fn parse_json_empty_array() {
        let result = parse_pg_stat(b"[]", 1_048_576);
        assert_matches!(result, Err(PgStatError::EmptyInput));
    }

    #[test]
    fn parse_json_invalid() {
        let result = parse_pg_stat(b"[{invalid json}]", 1_048_576);
        assert_matches!(result, Err(PgStatError::JsonParse(_)));
    }

    #[test]
    fn parse_json_field_alias() {
        // pg_stat_statements uses total_exec_time without _ms suffix
        let json = r#"[{
            "query": "SELECT 1",
            "calls": 10,
            "total_exec_time": 100.0,
            "mean_exec_time": 10.0,
            "rows": 10
        }]"#;
        let entries = parse_pg_stat(json.as_bytes(), 1_048_576).unwrap();
        assert!((entries[0].total_exec_time_ms - 100.0).abs() < f64::EPSILON);
    }

    // -- Ranking --

    #[test]
    fn rank_by_total_time() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_pg_stat(&entries, 2);
        assert_eq!(report.total_entries, 4);
        assert_eq!(report.top_n, 2);
        let by_time = &report.rankings[0];
        assert_eq!(by_time.label, "top by total_exec_time");
        assert_eq!(by_time.entries.len(), 2);
        // First should be highest total_exec_time
        assert!(by_time.entries[0].total_exec_time_ms >= by_time.entries[1].total_exec_time_ms);
    }

    #[test]
    fn rank_by_calls() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_pg_stat(&entries, 10);
        let by_calls = &report.rankings[1];
        assert_eq!(by_calls.label, "top by calls");
        assert!(by_calls.entries[0].calls >= by_calls.entries[1].calls);
    }

    #[test]
    fn rank_by_mean_time() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_pg_stat(&entries, 10);
        let by_mean = &report.rankings[2];
        assert_eq!(by_mean.label, "top by mean_exec_time");
        assert!(by_mean.entries[0].mean_exec_time_ms >= by_mean.entries[1].mean_exec_time_ms);
    }

    #[test]
    fn rank_top_n_limits_output() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_pg_stat(&entries, 1);
        for ranking in &report.rankings {
            assert_eq!(ranking.entries.len(), 1);
        }
    }

    #[test]
    fn rank_pg_stat_emits_four_rankings_in_stable_order() {
        let entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_pg_stat(&entries, 10);
        assert_eq!(report.rankings.len(), 4, "exactly 4 rankings expected");
        assert_eq!(report.rankings[0].label, "top by total_exec_time");
        assert_eq!(report.rankings[1].label, "top by calls");
        assert_eq!(report.rankings[2].label, "top by mean_exec_time");
        assert_eq!(report.rankings[3].label, "top by shared_blks_total");

        // by_io_blocks ranking: first entry has the highest hits+reads
        // sum among all parsed entries.
        let by_io = &report.rankings[3];
        let expected_top_sum = entries
            .iter()
            .map(|e| e.shared_blks_read.saturating_add(e.shared_blks_hit))
            .max()
            .unwrap();
        let actual_top_sum = by_io.entries[0]
            .shared_blks_read
            .saturating_add(by_io.entries[0].shared_blks_hit);
        assert_eq!(
            actual_top_sum, expected_top_sum,
            "by_io_blocks top must be the entry with max hits+reads"
        );
    }

    #[test]
    fn rank_empty_entries() {
        let report = rank_pg_stat(&[], 10);
        assert_eq!(report.total_entries, 0);
        for ranking in &report.rankings {
            assert!(ranking.entries.is_empty());
        }
    }

    // -- Cross-reference --

    use crate::detect::test_finding_with_template as make_finding;

    #[test]
    fn cross_reference_marks_matching_templates() {
        let mut entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let findings = vec![make_finding("SELECT * FROM order_item WHERE order_id = ?")];
        cross_reference(&mut entries, &findings);
        assert!(entries[0].seen_in_traces);
        assert!(!entries[1].seen_in_traces);
    }

    #[test]
    fn cross_reference_no_matches() {
        let mut entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let findings = vec![make_finding("SELECT * FROM nonexistent WHERE id = ?")];
        cross_reference(&mut entries, &findings);
        assert!(entries.iter().all(|e| !e.seen_in_traces));
    }

    #[test]
    fn cross_reference_empty_findings() {
        let mut entries = parse_pg_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        cross_reference(&mut entries, &[]);
        assert!(entries.iter().all(|e| !e.seen_in_traces));
    }

    // -- CSV row parsing edge cases --

    #[test]
    fn csv_row_with_embedded_comma() {
        let row = r#""SELECT a, b FROM t",100,500.0,5.0"#;
        let fields = parse_csv_row(row);
        assert_eq!(fields[0], "SELECT a, b FROM t");
        assert_eq!(fields[1], "100");
    }

    #[test]
    fn csv_row_simple() {
        let row = "a,b,c,d";
        let fields = parse_csv_row(row);
        assert_eq!(fields, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn csv_row_with_utf8_content() {
        let row = "\"SELECT * FROM café WHERE naïve = 'résumé'\",100,500.0,5.0";
        let fields = parse_csv_row(row);
        assert_eq!(fields[0], "SELECT * FROM café WHERE naïve = 'résumé'");
    }

    #[test]
    fn parse_invalid_utf8_returns_error() {
        let data: &[u8] = &[0xFF, 0xFE, 0x00, 0x01];
        let result = parse_pg_stat(data, 1_048_576);
        assert_matches!(result, Err(PgStatError::CsvParse { line: 0, .. }));
    }

    #[test]
    fn parse_csv_invalid_number_returns_error() {
        let csv = "query,calls,total_exec_time,mean_exec_time\nSELECT 1,abc,500.0,5.0";
        let result = parse_pg_stat(csv.as_bytes(), 1_048_576);
        assert_matches!(result, Err(PgStatError::CsvParse { line: 2, .. }));
    }

    // -- Prometheus response parsing --

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn parse_prometheus_response_basic() {
        let json = br#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    {
                        "metric": {
                            "__name__": "pg_stat_statements_seconds_total",
                            "query": "SELECT * FROM orders WHERE id = $1"
                        },
                        "value": [1720000000, "4.5"]
                    },
                    {
                        "metric": {
                            "__name__": "pg_stat_statements_seconds_total",
                            "query": "INSERT INTO audit_log VALUES ($1)"
                        },
                        "value": [1720000000, "1.2"]
                    }
                ]
            }
        }"#;

        let entries = parse_prometheus_response(json, None, &PrometheusPgStat::default()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!((entries[0].total_exec_time_ms - 4500.0).abs() < f64::EPSILON);
        assert!((entries[1].total_exec_time_ms - 1200.0).abs() < f64::EPSILON);
        // Templates should be normalized.
        assert!(entries[0].normalized_template.contains('?'));
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn parse_prometheus_response_empty_result() {
        let json = br#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let entries = parse_prometheus_response(json, None, &PrometheusPgStat::default()).unwrap();
        assert!(entries.is_empty());
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn parse_prometheus_response_invalid_json() {
        let result = parse_prometheus_response(b"not json", None, &PrometheusPgStat::default());
        assert_matches!(result, Err(PgStatError::PrometheusFormat(_)));
    }

    // -- Prometheus endpoint URL validation --

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn validate_endpoint_accepts_http_and_https() {
        assert!(validate_prometheus_endpoint("http://prometheus:9090").is_ok());
        assert!(validate_prometheus_endpoint("https://prometheus.example.com").is_ok());
        assert!(validate_prometheus_endpoint("http://127.0.0.1:9090").is_ok());
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn validate_endpoint_rejects_malformed_url() {
        let result = validate_prometheus_endpoint("not a url");
        assert_matches!(result, Err(PgStatError::PrometheusRequest(_)));
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn validate_endpoint_rejects_userinfo() {
        let result = validate_prometheus_endpoint("http://user:pass@prometheus:9090");
        assert!(
            matches!(result, Err(PgStatError::PrometheusRequest(msg)) if msg.contains("credentials")),
            "must reject userinfo in URL"
        );
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn validate_endpoint_rejects_non_http_scheme() {
        let result = validate_prometheus_endpoint("ftp://prometheus:9090");
        assert!(
            matches!(result, Err(PgStatError::PrometheusRequest(msg)) if msg.contains("scheme")),
            "must reject non-http(s) schemes"
        );
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_time_unit_defaults_to_seconds() {
        // The postgres_exporter built-in publishes seconds_total.
        let body = br#"{"data":{"result":[
            {"metric":{"query":"SELECT 1"},"value":[1,"4.5"]}]}}"#;
        let entries =
            parse_prometheus_response(body, None, &PrometheusPgStat::default()).expect("parse");
        assert!((entries[0].total_exec_time_ms - 4500.0).abs() < 0.001);
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_time_unit_can_be_milliseconds() {
        // pg_stat_statements itself counts in milliseconds, and an exporter
        // running its own query usually forwards that column as is. Reading
        // it as seconds would inflate every figure a thousandfold.
        let body = br#"{"data":{"result":[
            {"metric":{"query":"SELECT 1"},"value":[1,"4500"]}]}}"#;
        let opts = PrometheusPgStat {
            unit: PgStatTimeUnit::Milliseconds,
            ..PrometheusPgStat::default()
        };
        let entries = parse_prometheus_response(body, None, &opts).expect("parse");
        assert!((entries[0].total_exec_time_ms - 4500.0).abs() < 0.001);
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn call_counts_join_on_queryid() {
        // calls is a series of its own on every exporter, never a label, so
        // the ranking by calls was silently empty before.
        let timings = br#"{"data":{"result":[
            {"metric":{"query":"SELECT 1","queryid":"42"},"value":[1,"4.5"]}]}}"#;
        let calls = br#"{"data":{"result":[
            {"metric":{"queryid":"42"},"value":[1,"9"]}]}}"#;
        let entries = parse_prometheus_response(timings, Some(calls), &PrometheusPgStat::default())
            .expect("parse");
        assert_eq!(entries[0].calls, 9);
        assert!(
            (entries[0].mean_exec_time_ms - 500.0).abs() < 0.001,
            "the mean must follow from the call count, got {}",
            entries[0].mean_exec_time_ms
        );
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn call_counts_leave_unmatched_entries_alone() {
        let timings = br#"{"data":{"result":[
            {"metric":{"query":"SELECT 1","queryid":"42"},"value":[1,"4.5"]}]}}"#;
        let calls = br#"{"data":{"result":[
            {"metric":{"queryid":"999"},"value":[1,"9"]}]}}"#;
        let entries = parse_prometheus_response(timings, Some(calls), &PrometheusPgStat::default())
            .expect("parse");
        assert_eq!(entries[0].calls, 0, "no match means no invented count");
        assert!((entries[0].mean_exec_time_ms - 4500.0).abs() < 0.001);
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_series_name_defaults_to_the_official_exporter() {
        let opts = PrometheusPgStat::default();
        assert!(
            build_prometheus_query(10, &opts).contains("pg_stat_statements_seconds_total"),
            "the postgres_exporter built-in series stays the default"
        );
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_series_name_is_overridable() {
        // A deployment whose exporter publishes a hand-written query names
        // its columns itself, so the series is not ours to dictate.
        let opts = PrometheusPgStat {
            series: "pg_stat_statements_total_exec_time".to_string(),
            ..PrometheusPgStat::default()
        };
        let query = build_prometheus_query(10, &opts);
        assert!(
            query.contains("pg_stat_statements_total_exec_time"),
            "{query}"
        );
        assert!(!query.contains("seconds_total"), "{query}");
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_series_name_rejects_anything_but_a_metric_name() {
        // The series is interpolated into the query string unencoded, so a
        // separator would split the parameter or truncate it into a fragment.
        for bad in [
            "",
            "9starts_with_a_digit",
            "pg_stat&injected=1",
            "pg_stat#frag",
            "pg_stat total",
            "pg_stat{job=\"db\"}",
            "topk(1, x)",
        ] {
            assert!(
                validate_series_name(bad).is_err(),
                "`{bad}` must be rejected before it reaches the URL"
            );
        }
        assert!(validate_series_name("pg_stat_statements_total_exec_time").is_ok());
        assert!(validate_series_name(":recorded_rule:sum").is_ok());
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_query_label_is_overridable() {
        let body = br#"{"status":"success","data":{"resultType":"vector","result":[
            {"metric":{"__name__":"x","query_sample":"SELECT 1 FROM t"},
             "value":[1,"2.5"]}]}}"#;
        let opts = PrometheusPgStat {
            query_label: "query_sample".to_string(),
            ..PrometheusPgStat::default()
        };
        let entries = parse_prometheus_response(body, None, &opts).expect("parse");
        assert_eq!(entries[0].query, "SELECT 1 FROM t");
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[test]
    fn prometheus_query_label_still_falls_back_to_queryid() {
        let body = br#"{"status":"success","data":{"resultType":"vector","result":[
            {"metric":{"__name__":"x","queryid":"12345"},"value":[1,"1.0"]}]}}"#;
        let entries =
            parse_prometheus_response(body, None, &PrometheusPgStat::default()).expect("parse");
        assert_eq!(entries[0].query, "12345");
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[tokio::test]
    async fn fetch_from_prometheus_sends_auth_header_on_wire() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let response = crate::test_helpers::http_200_text("application/json", body);
        let (endpoint, mut rx, server) = crate::test_helpers::spawn_capture_server(response).await;

        let entries = fetch_from_prometheus(
            &endpoint,
            5,
            Some("Authorization: Bearer topsecret"),
            &PrometheusPgStat::default(),
        )
        .await
        .expect("fetch_from_prometheus must succeed");
        assert!(entries.is_empty());

        let captured = rx.recv().await.expect("captured request");
        let text = std::str::from_utf8(&captured).expect("utf8");
        assert!(
            text.contains("authorization: Bearer topsecret")
                || text.contains("Authorization: Bearer topsecret"),
            "auth header missing from request, got:\n{text}"
        );
        server.await.expect("server join");
    }

    #[cfg(any(feature = "daemon", feature = "tempo"))]
    #[tokio::test]
    async fn fetch_from_prometheus_rejects_invalid_auth_header() {
        let err = fetch_from_prometheus(
            "http://prometheus.local:9090",
            5,
            Some("NoColonHere"),
            &PrometheusPgStat::default(),
        )
        .await
        .expect_err("malformed auth header must be rejected");
        match err {
            PgStatError::PrometheusRequest(msg) => {
                assert!(
                    msg.contains("invalid auth header"),
                    "error message should flag the auth header parse failure, got: {msg}"
                );
            }
            other => panic!("expected PrometheusRequest, got {other:?}"),
        }
    }
}
