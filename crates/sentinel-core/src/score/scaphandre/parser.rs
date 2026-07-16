//! Prometheus text-exposition parser for Scaphandre's per-process
//! power metric.
//!
//! The parser is deliberately forgiving: malformed lines are
//! silently skipped rather than returning an error, so a single bad
//! line can't break the entire scrape. perf-sentinel treats
//! Scaphandre as best-effort telemetry; invalid data falls back to
//! the proxy model automatically.
//!
//! Only the `scaph_process_power_consumption_microwatts` metric is
//! extracted. Host and socket metrics, comments, and any other
//! Scaphandre or Prometheus scrape-level metric are skipped.

/// One parsed line of the Scaphandre `/metrics` exposition.
///
/// Carries `exe` (absolute path emitted by Scaphandre, e.g.
/// `/usr/lib/jvm/temurin-25-jdk-amd64/bin/java`) and `cmdline` (argv
/// concatenated without separators, e.g. `java-jar/tmp/svc-a.jar`).
/// Both are needed because multiple co-located services sharing a
/// runtime (several JVMs, several .NET assemblies) collide on `exe`
/// and only `cmdline` discriminates them. `cmdline` may be empty if
/// the label was absent on the wire.
///
/// The `pid` label is intentionally NOT retained: PIDs are unstable
/// across restarts and serve no purpose for service-level attribution.
use crate::score::prom_parser::{
    find_label_block_end, parse_next_label, unescape_prometheus_value,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessPower {
    pub exe: String,
    pub cmdline: String,
    pub power_microwatts: f64,
}

/// Parse a Scaphandre `/metrics` exposition body, extracting the
/// per-process power-consumption entries.
///
/// Only lines for the metric
/// `scaph_process_power_consumption_microwatts` are returned. Other
/// metrics (`scaph_host_power_microwatts`, `scaph_socket_power_microwatts`,
/// go_*, process_*, etc.) are skipped. Comments (lines starting with
/// `#`) are skipped. Label values may contain escaped quotes (`\"`) and
/// escaped backslashes (`\\`), which are unescaped into the returned
/// string, this is rare but can occur for JVM processes with quoted
/// args in their `cmdline` label. Real Scaphandre also concatenates
/// argv without separators: `java -jar /tmp/svc.jar` is emitted as
/// `cmdline="java-jar/tmp/svc.jar"`, which downstream matchers must
/// account for.
#[must_use]
pub fn parse_scaphandre_metrics(body: &str) -> Vec<ProcessPower> {
    const METRIC_NAME: &str = "scaph_process_power_consumption_microwatts";
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A valid line is of the form:
        //   metric_name{label="value",...} 42.5[ timestamp]
        // We look for the metric_name followed by '{' (labels) or ' '
        // (no labels, shouldn't happen for per-process power but
        // handle defensively).
        let Some(rest) = line.strip_prefix(METRIC_NAME) else {
            continue;
        };
        let (labels_str, value_str) = match rest.as_bytes().first() {
            Some(b'{') => {
                // Find the matching closing '}' by walking the bytes
                // and respecting escape sequences inside label values.
                match find_label_block_end(rest) {
                    Some(end) => (&rest[1..end], rest[end + 1..].trim_start()),
                    None => continue, // unmatched '{' → skip
                }
            }
            Some(b' ') => ("", rest.trim_start()),
            _ => continue, // not a matching metric (prefix collision)
        };
        // value_str now starts with the numeric value, optionally
        // followed by a trailing timestamp. Split on whitespace and
        // take the first token.
        let value_token = value_str.split_whitespace().next().unwrap_or("");
        let Ok(value) = value_token.parse::<f64>() else {
            continue;
        };
        let (exe, cmdline) = extract_exe_and_cmdline(labels_str);
        let Some(exe) = exe else {
            continue;
        };
        out.push(ProcessPower {
            exe,
            cmdline,
            power_microwatts: value,
        });
    }
    out
}

/// Extract `exe` and `cmdline` label values from a labels string in a
/// single pass (the part between `{` and `}`, excluding the braces).
///
/// Returns `(exe, cmdline)` where `exe` is `Some` when the label is
/// present (lines without `exe` are dropped upstream) and `cmdline`
/// defaults to the empty string when absent. Unescapes `\"` and `\\`
/// in both values lazily. Stops walking labels as soon as both are
/// found.
fn extract_exe_and_cmdline(labels: &str) -> (Option<String>, String) {
    let bytes = labels.as_bytes();
    let mut i = 0;
    let mut exe: Option<String> = None;
    let mut cmdline: Option<String> = None;
    while i < bytes.len() {
        let Some(parsed) = parse_next_label(labels, bytes, i) else {
            break;
        };
        let materialize = || {
            if parsed.needs_unescape {
                unescape_prometheus_value(parsed.value)
            } else {
                parsed.value.to_string()
            }
        };
        match parsed.name.trim() {
            "exe" if exe.is_none() => exe = Some(materialize()),
            "cmdline" if cmdline.is_none() => cmdline = Some(materialize()),
            _ => {}
        }
        if exe.is_some() && cmdline.is_some() {
            break;
        }
        i = parsed.next_index;
    }
    (exe, cmdline.unwrap_or_default())
}
