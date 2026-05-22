//! Prometheus text-exposition parser for Kepler's cumulative joule
//! counters.
//!
//! Mirrors `scaphandre::parser` in spirit (forgiving, skips malformed
//! lines) but is generic over the metric name and the routing label
//! key, since Kepler v2 exports a per-container series
//! (`kepler_container_cpu_joules_total`, keyed by `container_name`)
//! and a per-process series (`kepler_process_cpu_joules_total`, keyed
//! by `comm`).

/// One parsed line of a Kepler counter exposition.
///
/// `label_value` is whatever the configured `label_key` resolved to
/// (a container name for the container series, a kernel `comm` string
/// for the process series). `joules_total` is the cumulative joule
/// reading at the moment of the scrape, the caller derives a delta vs
/// the previous scrape window.
#[derive(Debug, Clone, PartialEq)]
pub struct KeplerSample {
    pub label_value: String,
    pub joules_total: f64,
}

/// Parse a Kepler `/metrics` exposition body, extracting samples for
/// the requested `metric_name`, keyed on `label_key`.
///
/// Only lines matching `metric_name` are returned. Comments, blank
/// lines, and all other Kepler/Prometheus scrape-level metrics are
/// skipped. Lines without the configured label or with an unparseable
/// numeric value are also skipped, this is best-effort telemetry,
/// not a strict parser.
#[must_use]
pub fn parse_kepler_metrics(body: &str, metric_name: &str, label_key: &str) -> Vec<KeplerSample> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(metric_name) else {
            continue;
        };
        let (labels_str, value_str) = match rest.as_bytes().first() {
            Some(b'{') => match find_label_block_end(rest) {
                Some(end) => (&rest[1..end], rest[end + 1..].trim_start()),
                None => continue,
            },
            Some(b' ') => ("", rest.trim_start()),
            _ => continue,
        };
        let value_token = value_str.split_whitespace().next().unwrap_or("");
        let Ok(joules_total) = value_token.parse::<f64>() else {
            continue;
        };
        let Some(label_value) = extract_label(labels_str, label_key) else {
            continue;
        };
        out.push(KeplerSample {
            label_value,
            joules_total,
        });
    }
    out
}

/// Find the index of the closing `}` that matches the leading `{` in a
/// Prometheus labels block. Handles escape sequences inside label
/// values so a backslash followed by a quote does not prematurely end
/// the block.
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

/// Extract a single label value by key from a Prometheus labels block.
/// Returns the unescaped value, or `None` if the key is absent.
fn extract_label(labels: &str, target_key: &str) -> Option<String> {
    let bytes = labels.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let parsed = parse_next_label(labels, bytes, i)?;
        if parsed.name.trim() == target_key {
            return Some(if parsed.needs_unescape {
                unescape_prometheus_value(parsed.value)
            } else {
                parsed.value.to_string()
            });
        }
        i = parsed.next_index;
    }
    None
}

struct ParsedLabel<'a> {
    name: &'a str,
    value: &'a str,
    needs_unescape: bool,
    next_index: usize,
}

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

fn read_label_name<'a>(labels: &'a str, bytes: &[u8], i: usize) -> Option<(&'a str, usize)> {
    let name_start = i;
    let mut pos = i;
    while pos < bytes.len() && bytes[pos] != b'=' {
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    Some((&labels[name_start..pos], pos + 1))
}

fn read_label_value<'a>(labels: &'a str, bytes: &[u8], i: usize) -> Option<(&'a str, bool, usize)> {
    if i >= bytes.len() || bytes[i] != b'"' {
        return None;
    }
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
    Some((&labels[value_start..pos], needs_unescape, pos + 1))
}

fn advance_past_separators(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b' ') {
        i += 1;
    }
    i
}

fn unescape_prometheus_value(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
