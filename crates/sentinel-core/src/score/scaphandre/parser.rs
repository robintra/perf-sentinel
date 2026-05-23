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

/// One parsed Prometheus label, as returned by [`parse_next_label`].
/// All string slices borrow from the outer `labels` buffer.
struct ParsedLabel<'a> {
    name: &'a str,
    value: &'a str,
    needs_unescape: bool,
    /// Byte offset in `labels.as_bytes()` just past the trailing
    /// comma / whitespace, i.e. the start of the next label.
    next_index: usize,
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

/// Parse a single `name="value"` label starting at byte offset `i`.
///
/// Returns `None` if the buffer is truncated or the shape is invalid
/// (missing `=`, missing opening `"`, unterminated value). On success,
/// returns a [`ParsedLabel`] with the three components plus the offset
/// just past the following separator, ready for the next iteration.
fn parse_next_label<'a>(labels: &'a str, bytes: &[u8], i: usize) -> Option<ParsedLabel<'a>> {
    let (name, after_eq) = read_label_name(labels, bytes, i)?;
    let (value, needs_unescape, after_close_quote) = read_label_value(labels, bytes, after_eq)?;
    let next_index = advance_past_separators(bytes, after_close_quote);
    Some(ParsedLabel {
        name,
        value,
        needs_unescape,
        next_index,
    })
}

/// Read the label name starting at `i` and return `(name, index_after_equals)`.
/// `None` if the buffer runs out before an `=` is found.
fn read_label_name<'a>(labels: &'a str, bytes: &[u8], i: usize) -> Option<(&'a str, usize)> {
    let name_start = i;
    let mut pos = i;
    while pos < bytes.len() && bytes[pos] != b'=' {
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    // +1 to consume the '='.
    Some((&labels[name_start..pos], pos + 1))
}

/// Read a quoted label value starting at `i` (which must point at the
/// opening `"`). Returns `(value_slice, needs_unescape, index_after_close_quote)`.
/// `None` if the opening quote is missing or the value is unterminated.
fn read_label_value<'a>(labels: &'a str, bytes: &[u8], i: usize) -> Option<(&'a str, bool, usize)> {
    if i >= bytes.len() || bytes[i] != b'"' {
        return None;
    }
    // +1 to consume the opening quote.
    let value_start = i + 1;
    let mut pos = value_start;
    let mut needs_unescape = false;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' if pos + 1 < bytes.len() => {
                needs_unescape = true;
                pos += 2;
            }
            b'"' => break,
            _ => pos += 1,
        }
    }
    if pos >= bytes.len() {
        return None;
    }
    // +1 to consume the closing quote.
    Some((&labels[value_start..pos], needs_unescape, pos + 1))
}

/// Advance past any trailing `,` / whitespace separating two labels.
fn advance_past_separators(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b' ') {
        i += 1;
    }
    i
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
