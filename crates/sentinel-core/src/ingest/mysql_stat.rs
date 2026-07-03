//! Ingestion for `MySQL` Performance Schema statement digests.
//!
//! Parses CSV or JSON exports of `performance_schema.events_statements_summary_by_digest`
//! into a `MySqlStatReport` with top-N rankings by total execution time, call
//! count, mean execution time, and rows examined.
//!
//! Timer columns (`SUM_TIMER_WAIT`, `AVG_TIMER_WAIT`) arrive in picoseconds and
//! are converted to milliseconds at parse time. Like `pg_stat_statements`, the
//! digest view has no `trace_id`, it provides a complementary database-level
//! view of SQL hotspots.

use crate::detect::Finding;
use crate::normalize::sql::normalize_sql;
use serde::{Deserialize, Serialize};

/// Picoseconds per millisecond: `MySQL` timer columns are picoseconds.
const PS_PER_MS: f64 = 1e9;

/// A single entry from `events_statements_summary_by_digest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MySqlStatEntry {
    /// Original digest text (already `?`-parameterized by `MySQL`).
    pub query: String,
    /// Template after `perf-sentinel` SQL normalization.
    pub normalized_template: String,
    /// Schema the digest was observed in (`SCHEMA_NAME`), when present.
    pub schema_name: Option<String>,
    /// Number of times the statement was executed (`COUNT_STAR`).
    pub calls: u64,
    /// Total execution time in milliseconds (from `SUM_TIMER_WAIT` picoseconds).
    pub total_exec_time_ms: f64,
    /// Mean execution time in milliseconds (from `AVG_TIMER_WAIT` picoseconds).
    pub mean_exec_time_ms: f64,
    /// Total rows sent to clients (`SUM_ROWS_SENT`).
    pub rows_sent: u64,
    /// Total rows examined by the storage engine (`SUM_ROWS_EXAMINED`).
    pub rows_examined: u64,
    /// Whether this template was also seen in trace-based findings.
    #[serde(default)]
    pub seen_in_traces: bool,
}

/// A ranking of digest entries by a specific criterion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MySqlStatRanking {
    /// Label describing the ranking criterion (e.g., "top by `total_exec_time`").
    pub label: String,
    /// Entries sorted by the criterion, limited to `top_n`.
    pub entries: Vec<MySqlStatEntry>,
}

/// Report produced from Performance Schema digest analysis.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MySqlStatReport {
    /// Total number of entries parsed.
    pub total_entries: usize,
    /// Number of top entries per ranking.
    pub top_n: usize,
    /// Rankings in a stable order: by `total_exec_time`, by `calls`,
    /// by `mean_exec_time`, by `rows_examined`. Consumers that index by
    /// position (e.g., the HTML dashboard's `mysql_stat` sub-switcher)
    /// rely on this ordering not changing. New rankings are appended,
    /// existing indices are never reassigned.
    pub rankings: Vec<MySqlStatRanking>,
}

/// Detected input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MySqlStatFormat {
    Csv,
    Json,
}

/// Errors that can occur during Performance Schema digest parsing.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MySqlStatError {
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
}

/// Raw JSON entry matching common digest export shapes: raw
/// `performance_schema` UPPERCASE column names or their lowercase twins.
/// Timer fields stay in picoseconds here, converted on mapping.
#[derive(Deserialize)]
struct RawJsonEntry {
    // Option: performance_schema keeps a catch-all aggregation row with
    // DIGEST_TEXT = NULL once the digest table saturates; that row is
    // skipped instead of failing the whole export.
    #[serde(default, alias = "DIGEST_TEXT")]
    digest_text: Option<String>,
    #[serde(default, alias = "SCHEMA_NAME")]
    schema_name: Option<String>,
    #[serde(alias = "COUNT_STAR")]
    count_star: u64,
    #[serde(alias = "SUM_TIMER_WAIT")]
    sum_timer_wait: f64,
    #[serde(alias = "AVG_TIMER_WAIT")]
    avg_timer_wait: f64,
    #[serde(default, alias = "SUM_ROWS_SENT")]
    sum_rows_sent: u64,
    #[serde(default, alias = "SUM_ROWS_EXAMINED")]
    sum_rows_examined: u64,
}

/// Detect whether the input is CSV or JSON.
///
/// Peeks at the first non-whitespace byte: `[` or `{` indicates JSON,
/// otherwise CSV. Returns `Csv` as fallback for empty input; the caller
/// should validate non-emptiness separately.
#[must_use]
pub fn detect_mysql_stat_format(raw: &[u8]) -> MySqlStatFormat {
    let trimmed = raw.iter().position(|&b| !b.is_ascii_whitespace());
    match trimmed.map(|i| raw[i]) {
        Some(b'[' | b'{') => MySqlStatFormat::Json,
        _ => MySqlStatFormat::Csv,
    }
}

/// Parse Performance Schema digest data from raw bytes.
///
/// Auto-detects CSV vs JSON format. Normalizes each digest through the
/// SQL normalizer for consistency with trace-based analysis (backticked
/// identifiers survive, `IN (?, ?, ...)` collapses to `IN (?)`).
///
/// # Errors
///
/// Returns an error if the payload exceeds `max_size`, the input is empty,
/// or parsing fails.
pub fn parse_mysql_stat(
    raw: &[u8],
    max_size: usize,
) -> Result<Vec<MySqlStatEntry>, MySqlStatError> {
    if raw.len() > max_size {
        return Err(MySqlStatError::PayloadTooLarge {
            size: raw.len(),
            max: max_size,
        });
    }
    if raw.is_empty() || raw.iter().all(|&b| b.is_ascii_whitespace()) {
        return Err(MySqlStatError::EmptyInput);
    }

    let text = std::str::from_utf8(raw).map_err(|e| MySqlStatError::CsvParse {
        line: 0,
        detail: format!("invalid UTF-8: {e}"),
    })?;

    match detect_mysql_stat_format(raw) {
        MySqlStatFormat::Csv => parse_csv(text),
        MySqlStatFormat::Json => parse_json(text),
    }
}

/// Generate rankings from parsed entries.
///
/// Produces four rankings in a stable order: by total execution time,
/// by call count, by mean execution time, by rows examined. Each ranking
/// contains at most `top_n` entries. Index-based sorting, same clone
/// trade-off as `rank_pg_stat` (one-shot path, never per-event).
///
/// Downstream consumers rely on the rankings appearing at the documented
/// positions, new rankings are always appended and existing indices
/// never reassign.
#[must_use]
pub fn rank_mysql_stat(entries: &[MySqlStatEntry], top_n: usize) -> MySqlStatReport {
    let total_entries = entries.len();

    let top_n_by = |cmp: fn(&MySqlStatEntry, &MySqlStatEntry) -> std::cmp::Ordering,
                    label: &str|
     -> MySqlStatRanking {
        let mut indices: Vec<usize> = (0..entries.len()).collect();
        indices.sort_by(|&a, &b| cmp(&entries[a], &entries[b]));
        indices.truncate(top_n);
        MySqlStatRanking {
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

    // Rows examined is MySQL's I/O-cost signal: a high examined-to-sent
    // ratio flags full scans and missing indexes, the closest analog to
    // pg's shared-buffer traffic ranking.
    let by_rows_examined = top_n_by(
        |a, b| b.rows_examined.cmp(&a.rows_examined),
        "top by rows_examined",
    );

    MySqlStatReport {
        total_entries,
        top_n,
        rankings: vec![by_total_time, by_calls, by_mean_time, by_rows_examined],
    }
}

/// Cross-reference digest entries with trace-based findings.
///
/// Marks entries whose `normalized_template` matches any finding's
/// pattern template. Both sides are canonicalized first: `MySQL`
/// `DIGEST_TEXT` spaces every token (`` `c` . `name` ``), uppercases
/// keywords and forces backtick quoting, none of which appears in a
/// template normalized from raw application SQL, so an exact string
/// compare would silently never match.
pub fn cross_reference(entries: &mut [MySqlStatEntry], findings: &[Finding]) {
    let templates: std::collections::HashSet<String> = findings
        .iter()
        .map(|f| comparison_key(&f.pattern.template))
        .collect();

    for entry in entries {
        if templates.contains(&comparison_key(&entry.normalized_template)) {
            entry.seen_in_traces = true;
        }
    }
}

/// Punctuation and operator tokens `MySQL` digest text surrounds with
/// spaces while compact application SQL does not.
fn is_token_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | '(' | ')' | '=' | '<' | '>' | ';' | '!' | '+' | '-' | '*' | '/' | '%' | '?'
    )
}

/// Best-effort canonical form for digest-vs-trace template comparison:
/// strip backtick quoting, drop whitespace around punctuation and
/// operators, collapse remaining whitespace runs, lowercase (`MySQL`
/// uppercases keywords in digest text while application SQL usually
/// does not).
///
/// Known ceiling: lowercasing also folds identifiers, so on a
/// case-sensitive server (`lower_case_table_names=0`) two tables that
/// differ only by case share a key and the `[seen in traces]` marker
/// can over-match. Accepted for an informational marker; a
/// keyword-only fold would need a full keyword table.
fn comparison_key(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut pending_space = false;
    for c in template.chars() {
        if c == '`' {
            continue;
        }
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space
            && !is_token_punct(c)
            && !out.is_empty()
            // Drop the pending space when the previous emitted char was
            // punctuation too ("a . b" and "a. b" both become "a.b").
            && !out.chars().next_back().is_some_and(is_token_punct)
        {
            out.push(' ');
        }
        pending_space = false;
        out.extend(c.to_lowercase());
    }
    out
}

// ---------------------------------------------------------------------------
// CSV parsing (RFC 4180 subset, row parser shared with pg_stat)
// ---------------------------------------------------------------------------

const MAX_CSV_ROWS: usize = 1_000_000;

use super::pg_stat::parse_csv_row;

/// `SCHEMA_NAME` normalization: `MySQL` renders absent schemas as SQL
/// `NULL` (client exports) or `\N` (`INTO OUTFILE` style dumps).
fn parse_schema_name(value: Option<&String>) -> Option<String> {
    value
        .map(|s| s.trim())
        .filter(|s| !is_null_marker(s))
        .map(ToString::to_string)
}

/// The textual NULL renderings `MySQL` tooling emits for absent values.
fn is_null_marker(s: &str) -> bool {
    s.is_empty() || s.eq_ignore_ascii_case("null") || s == "\\N"
}

fn parse_csv(text: &str) -> Result<Vec<MySqlStatEntry>, MySqlStatError> {
    let mut lines = text.lines();

    let header_line = lines.next().ok_or(MySqlStatError::EmptyInput)?;
    let headers = parse_csv_row(header_line);
    let col = |name: &str| -> Result<usize, MySqlStatError> {
        headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(name))
            .ok_or_else(|| MySqlStatError::MissingColumn(name.to_string()))
    };

    let digest_idx = col("digest_text")?;
    let calls_idx = col("count_star")?;
    let total_time_idx = col("sum_timer_wait")?;
    let mean_time_idx = col("avg_timer_wait")?;
    let schema_idx = col("schema_name").ok();
    let rows_sent_idx = col("sum_rows_sent").ok();
    let rows_examined_idx = col("sum_rows_examined").ok();

    // Estimate row count from byte length (~100 bytes per row), capped at 100k entries
    let estimated = (text.len() / 100).min(100_000);
    let mut entries = Vec::with_capacity(estimated);
    for (line_num, line) in lines.enumerate() {
        if entries.len() >= MAX_CSV_ROWS {
            return Err(MySqlStatError::CsvParse {
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

        let query = fields.get(digest_idx).cloned().unwrap_or_default();
        // Skip the catch-all aggregation row (DIGEST_TEXT is NULL once
        // the digest table saturates) instead of ranking a "NULL" query.
        if is_null_marker(query.trim()) {
            continue;
        }
        let calls = parse_u64(&fields, calls_idx, line_num, "count_star")?;
        // Timer columns are picoseconds and can exceed u64 on aggregated
        // servers, so they parse as f64 from the start.
        let sum_timer_wait = parse_f64(&fields, total_time_idx, line_num, "sum_timer_wait")?;
        let avg_timer_wait = parse_f64(&fields, mean_time_idx, line_num, "avg_timer_wait")?;
        let schema_name = parse_schema_name(schema_idx.and_then(|i| fields.get(i)));
        let rows_sent =
            rows_sent_idx.map_or(Ok(0), |i| parse_u64(&fields, i, line_num, "sum_rows_sent"))?;
        let rows_examined = rows_examined_idx.map_or(Ok(0), |i| {
            parse_u64(&fields, i, line_num, "sum_rows_examined")
        })?;

        let normalized = normalize_sql(&query);

        entries.push(MySqlStatEntry {
            query,
            normalized_template: normalized.template,
            schema_name,
            calls,
            total_exec_time_ms: sum_timer_wait / PS_PER_MS,
            mean_exec_time_ms: avg_timer_wait / PS_PER_MS,
            rows_sent,
            rows_examined,
            seen_in_traces: false,
        });
    }

    if entries.is_empty() {
        return Err(MySqlStatError::EmptyInput);
    }
    Ok(entries)
}

fn parse_u64(
    fields: &[String],
    idx: usize,
    line: usize,
    col_name: &str,
) -> Result<u64, MySqlStatError> {
    let val = fields.get(idx).map_or("", String::as_str).trim();
    val.parse::<u64>().map_err(|_| MySqlStatError::CsvParse {
        line,
        detail: format!("cannot parse '{val}' as integer for column {col_name}"),
    })
}

fn parse_f64(
    fields: &[String],
    idx: usize,
    line: usize,
    col_name: &str,
) -> Result<f64, MySqlStatError> {
    let val = fields.get(idx).map_or("", String::as_str).trim();
    val.parse::<f64>().map_err(|_| MySqlStatError::CsvParse {
        line,
        detail: format!("cannot parse '{val}' as float for column {col_name}"),
    })
}

// ---------------------------------------------------------------------------
// JSON parsing
// ---------------------------------------------------------------------------

fn parse_json(text: &str) -> Result<Vec<MySqlStatEntry>, MySqlStatError> {
    let raw_entries: Vec<RawJsonEntry> =
        serde_json::from_str(text).map_err(|e| MySqlStatError::JsonParse(e.to_string()))?;

    if raw_entries.is_empty() {
        return Err(MySqlStatError::EmptyInput);
    }
    if raw_entries.len() > MAX_CSV_ROWS {
        return Err(MySqlStatError::JsonParse(format!(
            "JSON array exceeds maximum of {MAX_CSV_ROWS} entries (got {})",
            raw_entries.len()
        )));
    }

    let entries: Vec<MySqlStatEntry> = raw_entries
        .into_iter()
        .filter_map(|raw| {
            // Skip the catch-all aggregation row (DIGEST_TEXT NULL).
            let digest_text = raw.digest_text.filter(|d| !is_null_marker(d.trim()))?;
            let normalized = normalize_sql(&digest_text);
            let schema_name = parse_schema_name(raw.schema_name.as_ref());
            Some(MySqlStatEntry {
                query: digest_text,
                normalized_template: normalized.template,
                schema_name,
                calls: raw.count_star,
                total_exec_time_ms: raw.sum_timer_wait / PS_PER_MS,
                mean_exec_time_ms: raw.avg_timer_wait / PS_PER_MS,
                rows_sent: raw.sum_rows_sent,
                rows_examined: raw.sum_rows_examined,
                seen_in_traces: false,
            })
        })
        .collect();

    // Every row was dropped: distinguish "wrong export shape" from a
    // legitimate report instead of returning a silent empty success
    // (rows missing DIGEST_TEXT entirely look exactly like the NULL
    // catch-all row to the filter above).
    if entries.is_empty() {
        return Err(MySqlStatError::JsonParse(
            "no row carried a usable DIGEST_TEXT (wrong column names in the \
             export, or every digest is NULL)"
                .to_string(),
        ));
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    const CSV_HEADER: &str = "SCHEMA_NAME,DIGEST_TEXT,COUNT_STAR,SUM_TIMER_WAIT,AVG_TIMER_WAIT,SUM_ROWS_SENT,SUM_ROWS_EXAMINED";

    fn sample_csv() -> String {
        format!(
            "{CSV_HEADER}\n\
             shop,SELECT * FROM `order_item` WHERE `order_id` = ?,1500,4500500000000,3000000000,1500,45000\n\
             shop,\"SELECT * FROM orders WHERE id IN (?, ?, ?)\",800,2400000000000,3000000000,2400,2400\n\
             NULL,SELECT COUNT ( * ) FROM order_item,50,250000000000,5000000000,50,500000"
        )
    }

    // ----- Format detection -----

    #[test]
    fn detect_format_csv() {
        assert_eq!(
            detect_mysql_stat_format(sample_csv().as_bytes()),
            MySqlStatFormat::Csv
        );
    }

    #[test]
    fn detect_format_json_array() {
        assert_eq!(
            detect_mysql_stat_format(b"[{\"DIGEST_TEXT\": \"SELECT ?\"}]"),
            MySqlStatFormat::Json
        );
    }

    #[test]
    fn detect_format_json_with_whitespace() {
        assert_eq!(
            detect_mysql_stat_format(b"  \n\t [{}]"),
            MySqlStatFormat::Json
        );
    }

    #[test]
    fn detect_format_empty_defaults_csv() {
        assert_eq!(detect_mysql_stat_format(b""), MySqlStatFormat::Csv);
    }

    // ----- CSV parsing -----

    #[test]
    fn parse_csv_basic() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].calls, 1500);
        assert_eq!(entries[0].schema_name.as_deref(), Some("shop"));
        assert_eq!(entries[0].rows_sent, 1500);
        assert_eq!(entries[0].rows_examined, 45000);
    }

    #[test]
    fn parse_csv_converts_picoseconds_to_ms() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        // 4_500_500_000_000 ps -> 4500.5 ms; 3_000_000_000 ps -> 3.0 ms.
        assert!((entries[0].total_exec_time_ms - 4500.5).abs() < f64::EPSILON);
        assert!((entries[0].mean_exec_time_ms - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_csv_huge_timer_exceeding_u64_parses_as_f64() {
        // Aggregated SUM_TIMER_WAIT can exceed u64::MAX (~1.8e19).
        let csv = format!("{CSV_HEADER}\nshop,SELECT ?,1,20000000000000000000,1000000000,1,1");
        let entries = parse_mysql_stat(csv.as_bytes(), 1_048_576).unwrap();
        assert!((entries[0].total_exec_time_ms - 2e10).abs() < 1e-3);
    }

    #[test]
    fn parse_csv_backticked_identifiers_survive_normalization() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        assert!(
            entries[0].normalized_template.contains("`order_item`"),
            "backticks must survive: {}",
            entries[0].normalized_template
        );
    }

    #[test]
    fn parse_csv_collapses_in_list() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        assert!(
            entries[1].normalized_template.contains("IN (?)"),
            "IN list must collapse: {}",
            entries[1].normalized_template
        );
    }

    #[test]
    fn parse_csv_null_schema_maps_to_none() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries[2].schema_name, None);
    }

    #[test]
    fn parse_csv_case_insensitive_headers() {
        let csv = "schema_name,digest_text,count_star,sum_timer_wait,avg_timer_wait\n\
                   shop,SELECT ?,10,1000000000,100000000";
        let entries = parse_mysql_stat(csv.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].calls, 10);
        // Optional columns absent -> defaults.
        assert_eq!(entries[0].rows_sent, 0);
        assert_eq!(entries[0].rows_examined, 0);
    }

    #[test]
    fn parse_csv_missing_required_column() {
        let csv = "DIGEST_TEXT,COUNT_STAR\nSELECT ?,10";
        let result = parse_mysql_stat(csv.as_bytes(), 1_048_576);
        assert_matches!(result, Err(MySqlStatError::MissingColumn(c)) if c == "sum_timer_wait");
    }

    #[test]
    fn parse_csv_quoted_field_with_comma() {
        let csv = format!(
            "{CSV_HEADER}\nshop,\"SELECT `a`, `b` FROM t WHERE id = ?\",5,1000000000,200000000,5,5"
        );
        let entries = parse_mysql_stat(csv.as_bytes(), 1_048_576).unwrap();
        assert!(entries[0].query.contains("`a`, `b`"));
    }

    #[test]
    fn parse_csv_invalid_number_reports_line() {
        let csv = format!("{CSV_HEADER}\nshop,SELECT ?,abc,1000000000,100000000,1,1");
        let result = parse_mysql_stat(csv.as_bytes(), 1_048_576);
        assert_matches!(result, Err(MySqlStatError::CsvParse { line: 2, .. }));
    }

    #[test]
    fn parse_empty_input() {
        assert_matches!(
            parse_mysql_stat(b"", 1_048_576),
            Err(MySqlStatError::EmptyInput)
        );
        assert_matches!(
            parse_mysql_stat(b"   \n  ", 1_048_576),
            Err(MySqlStatError::EmptyInput)
        );
    }

    #[test]
    fn parse_oversized_payload() {
        assert_matches!(
            parse_mysql_stat(&[b'a'; 100], 10),
            Err(MySqlStatError::PayloadTooLarge { size: 100, max: 10 })
        );
    }

    // ----- JSON parsing -----

    #[test]
    fn parse_json_uppercase_keys() {
        let json = r#"[{
            "SCHEMA_NAME": "shop",
            "DIGEST_TEXT": "SELECT * FROM `order_item` WHERE `order_id` = ?",
            "COUNT_STAR": 1500,
            "SUM_TIMER_WAIT": 4500500000000,
            "AVG_TIMER_WAIT": 3000000000,
            "SUM_ROWS_SENT": 1500,
            "SUM_ROWS_EXAMINED": 45000
        }]"#;
        let entries = parse_mysql_stat(json.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].calls, 1500);
        assert!((entries[0].total_exec_time_ms - 4500.5).abs() < f64::EPSILON);
        assert_eq!(entries[0].schema_name.as_deref(), Some("shop"));
    }

    #[test]
    fn parse_json_lowercase_keys() {
        let json = r#"[{
            "digest_text": "SELECT ?",
            "count_star": 10,
            "sum_timer_wait": 1000000000,
            "avg_timer_wait": 100000000
        }]"#;
        let entries = parse_mysql_stat(json.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries[0].calls, 10);
        assert_eq!(entries[0].schema_name, None);
        assert_eq!(entries[0].rows_examined, 0);
    }

    #[test]
    fn parse_json_null_schema_maps_to_none() {
        let json = r#"[{
            "SCHEMA_NAME": null,
            "DIGEST_TEXT": "SELECT ?",
            "COUNT_STAR": 1,
            "SUM_TIMER_WAIT": 1000000000,
            "AVG_TIMER_WAIT": 1000000000
        }]"#;
        let entries = parse_mysql_stat(json.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries[0].schema_name, None);
    }

    #[test]
    fn parse_json_empty_array() {
        assert_matches!(
            parse_mysql_stat(b"[]", 1_048_576),
            Err(MySqlStatError::EmptyInput)
        );
    }

    #[test]
    fn parse_json_invalid() {
        assert_matches!(
            parse_mysql_stat(b"[{\"DIGEST_TEXT\": 42}]", 1_048_576),
            Err(MySqlStatError::JsonParse(_))
        );
    }

    // ----- Ranking -----

    #[test]
    fn rank_produces_four_rankings_in_stable_order() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_mysql_stat(&entries, 10);
        assert_eq!(report.total_entries, 3);
        let labels: Vec<&str> = report.rankings.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "top by total_exec_time",
                "top by calls",
                "top by mean_exec_time",
                "top by rows_examined",
            ]
        );
    }

    #[test]
    fn rank_orders_by_criterion() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_mysql_stat(&entries, 10);
        // total time: 4500.5 first; rows_examined: 500_000 (COUNT(*)) first.
        assert_eq!(report.rankings[0].entries[0].calls, 1500);
        assert_eq!(report.rankings[3].entries[0].rows_examined, 500_000);
    }

    #[test]
    fn rank_truncates_to_top_n() {
        let entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let report = rank_mysql_stat(&entries, 2);
        for ranking in &report.rankings {
            assert_eq!(ranking.entries.len(), 2);
        }
    }

    // ----- Cross-reference -----

    use crate::detect::test_finding_with_template as make_finding;

    #[test]
    fn cross_reference_marks_matching_template() {
        let mut entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        let template = entries[0].normalized_template.clone();
        let findings = vec![make_finding(&template)];
        cross_reference(&mut entries, &findings);
        assert!(entries[0].seen_in_traces);
        assert!(!entries[1].seen_in_traces);
    }

    #[test]
    fn cross_reference_empty_findings() {
        let mut entries = parse_mysql_stat(sample_csv().as_bytes(), 1_048_576).unwrap();
        cross_reference(&mut entries, &[]);
        assert!(entries.iter().all(|e| !e.seen_in_traces));
    }

    #[test]
    fn cross_reference_bridges_digest_spacing_and_backticks() {
        // Regression: MySQL DIGEST_TEXT spaces every token and forces
        // backticks; the trace-side template comes from raw application
        // SQL. Exact string equality would silently never match.
        let csv = format!(
            "{CSV_HEADER}\ncrm,\"SELECT `c` . `name` , `o` . `total` FROM `customers` `c` WHERE `c` . `id` = ?\",10,1000000000,100000000,10,10"
        );
        let mut entries = parse_mysql_stat(csv.as_bytes(), 1_048_576).unwrap();
        let findings = vec![make_finding(
            "SELECT c.name, o.total FROM customers c WHERE c.id = ?",
        )];
        cross_reference(&mut entries, &findings);
        assert!(
            entries[0].seen_in_traces,
            "spaced backticked digest must match the plain trace template"
        );
    }

    #[test]
    fn comparison_key_canonicalizes_common_digest_shapes() {
        assert_eq!(
            comparison_key("SELECT `a` . `b` FROM `t` WHERE `a` . `id` IN (?)"),
            comparison_key("select a.b from t where a.id in (?)")
        );
        // Distinct queries stay distinct.
        assert_ne!(
            comparison_key("SELECT `a` FROM `t`"),
            comparison_key("SELECT `b` FROM `t`")
        );
    }

    #[test]
    fn parse_json_null_digest_row_is_skipped() {
        // performance_schema keeps a catch-all row with DIGEST_TEXT NULL
        // once the digest table saturates: skip it, keep the rest.
        let json = r#"[
            {"DIGEST_TEXT": null, "COUNT_STAR": 9999, "SUM_TIMER_WAIT": 1, "AVG_TIMER_WAIT": 1},
            {"DIGEST_TEXT": "SELECT ?", "COUNT_STAR": 10, "SUM_TIMER_WAIT": 1000000000, "AVG_TIMER_WAIT": 100000000}
        ]"#;
        let entries = parse_mysql_stat(json.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].calls, 10);
    }

    #[test]
    fn parse_json_all_null_digests_is_an_error_not_empty_success() {
        // Wrong column names (no DIGEST_TEXT at all) and a fully NULL
        // export must fail loudly, not print "Total entries: 0".
        let json = r#"[
            {"QUERY": "SELECT 1", "COUNT_STAR": 1, "SUM_TIMER_WAIT": 1, "AVG_TIMER_WAIT": 1},
            {"DIGEST_TEXT": null, "COUNT_STAR": 2, "SUM_TIMER_WAIT": 1, "AVG_TIMER_WAIT": 1}
        ]"#;
        assert_matches!(
            parse_mysql_stat(json.as_bytes(), 1_048_576),
            Err(MySqlStatError::JsonParse(_))
        );
    }

    #[test]
    fn comparison_key_bridges_spaced_operators() {
        // MySQL digest text spaces operators the app SQL writes compactly.
        assert_eq!(
            comparison_key("WHERE `a` != ? AND `b` > ?"),
            comparison_key("where a!=? and b>?")
        );
        assert_eq!(
            comparison_key("SELECT `a` + `b` FROM `t`"),
            comparison_key("select a+b from t")
        );
    }

    #[test]
    fn parse_csv_null_digest_row_is_skipped() {
        let csv = format!(
            "{CSV_HEADER}\nshop,NULL,9999,1,1,0,0\nshop,SELECT ?,10,1000000000,100000000,1,1"
        );
        let entries = parse_mysql_stat(csv.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].calls, 10);
    }

    #[test]
    fn parse_csv_backslash_n_schema_maps_to_none() {
        // \N is the INTO OUTFILE-style NULL rendering.
        let csv = format!("{CSV_HEADER}\n\\N,SELECT ?,10,1000000000,100000000,1,1");
        let entries = parse_mysql_stat(csv.as_bytes(), 1_048_576).unwrap();
        assert_eq!(entries[0].schema_name, None);
    }
}
