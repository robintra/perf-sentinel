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
/// Only the `exe` label and the numeric value matter;
/// other labels (`cmdline`, `pid`, `instance`) are discarded. The
/// parser below is tolerant of unknown labels so Scaphandre version
/// upgrades that add new label fields don't break scraping.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessPower {
    pub exe: String,
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
/// string — this is rare but can occur for JVM processes with quoted
/// args in their `cmdline` label.
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
        // (no labels — shouldn't happen for per-process power but
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
        let Some(exe) = extract_exe_label(labels_str) else {
            continue;
        };
        out.push(ProcessPower {
            exe,
            power_microwatts: value,
        });
    }
    out
}

/// Find the index of the closing `}` that matches the leading `{` in a
/// Prometheus labels block. The parser handles escape sequences inside
/// label values (`\"` and `\\`) so JVM-style cmdline labels with
/// embedded quotes don't trip a naive byte-match.
///
/// Returns `None` if the `{` is unmatched within the slice.
fn find_label_block_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut i = 1;
    let mut in_value = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' => in_value = !in_value,
            b'\\' if in_value => {
                // Skip the next byte (the escaped character). Safe to
                // advance by 2 bytes because Prometheus label values
                // only use single-byte ASCII escape sequences (\", \\,
                // \n), so the byte after the backslash is always a
                // single ASCII byte that cannot split a multi-byte
                // UTF-8 codepoint.
                i += 2;
                continue;
            }
            b'}' if !in_value => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Extract the `exe="..."` label value from a labels string (the part
/// between `{` and `}`, excluding the braces themselves).
///
/// Returns `None` if the `exe` label is absent. Unescapes `\"` and
/// `\\` in the value. Does not allocate unless escapes are present
/// AND a reallocation is needed beyond the initial capacity reserve.
fn extract_exe_label(labels: &str) -> Option<String> {
    // Scan for `exe="` (optionally preceded by a comma or start of string).
    // Prometheus labels are comma-separated name="value" pairs.
    let bytes = labels.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find the next `=` — the label name ends there.
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'=' {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let name = &labels[name_start..i];
        // Skip past '='.
        i += 1;
        // Expect '"'.
        if i >= bytes.len() || bytes[i] != b'"' {
            return None;
        }
        i += 1;
        // Collect value bytes until the unescaped closing '"'.
        let value_start = i;
        let mut needs_unescape = false;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if i + 1 < bytes.len() => {
                    needs_unescape = true;
                    i += 2;
                }
                b'"' => break,
                _ => i += 1,
            }
        }
        if i >= bytes.len() {
            return None;
        }
        if name.trim() == "exe" {
            let raw_value = &labels[value_start..i];
            return Some(if needs_unescape {
                unescape_prometheus_value(raw_value)
            } else {
                raw_value.to_string()
            });
        }
        // Skip closing '"' and any following comma / whitespace.
        i += 1;
        while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b' ') {
            i += 1;
        }
    }
    None
}

/// Unescape a Prometheus label value. Handles `\"`, `\\`, and `\n`
/// per the exposition format spec. Other backslash sequences are
/// passed through literally.
///
/// UTF-8-safe: walks the string by character, not by byte. A previous
/// implementation pushed `bytes[i] as char` which produced Latin-1
/// mojibake on any non-ASCII codepoint inside an `exe` label (rare in
/// Scaphandre output, but possible for paths with accented characters).
fn unescape_prometheus_value(raw: &str) -> String {
    // Fast path: no backslashes → return the input unchanged. Avoids
    // the per-char allocation path entirely for the common case where
    // the value has no escape sequences.
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    // Unknown escape: keep literal backslash + char.
                    out.push('\\');
                    out.push(other);
                }
                None => {
                    // Trailing backslash with no following char: keep
                    // it literal so the input is round-trippable.
                    out.push('\\');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
