//! Shared Prometheus text-exposition parser for the label-keyed energy
//! scrapers.
//!
//! Generic over the metric name and the routing label key, since each
//! backend exposes a different pair: Kepler exports a per-container
//! series (`kepler_container_cpu_joules_total`, keyed by
//! `container_name`) and a per-process series
//! (`kepler_process_cpu_joules_total`, keyed by `comm`), while Alumet
//! lets the operator name both through its exporter config.
//!
//! Forgiving by contract, like `scaphandre::parser`: a malformed line is
//! skipped, never an error. This is best-effort telemetry.
//!
//! `scaphandre::parser` stays separate on purpose: it extracts two
//! labels (`exe` and `cmdline`) in a single pass, so its scan loop
//! legitimately diverges from the single-label shape here.

/// One parsed sample of a Prometheus exposition line.
///
/// `label_value` is whatever the configured `label_key` resolved to (a
/// container name, a kernel `comm` string, a pod name). `value` is the
/// raw reading at the moment of the scrape, its unit and semantics are
/// the caller's business: Kepler reads it as a cumulative joule counter
/// and derives a delta, Alumet reads it as the energy of one poll
/// interval.
#[derive(Debug, Clone, PartialEq)]
pub struct PromSample {
    pub label_value: String,
    pub value: f64,
}

/// Parse a Prometheus `/metrics` exposition body, extracting samples for
/// the requested `metric_name`, keyed on `label_key`.
///
/// Only lines matching `metric_name` are returned. Comments, blank
/// lines, and all other scrape-level metrics are skipped. Lines without
/// the configured label or with an unparseable numeric value are also
/// skipped.
#[must_use]
pub fn parse_metric_samples(body: &str, metric_name: &str, label_key: &str) -> Vec<PromSample> {
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
        let Ok(value) = value_token.parse::<f64>() else {
            continue;
        };
        let Some(label_value) = extract_label(labels_str, label_key) else {
            continue;
        };
        out.push(PromSample { label_value, value });
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

#[cfg(test)]
mod tests {
    use super::{PromSample, parse_metric_samples};

    #[test]
    fn parse_empty_body() {
        assert!(
            parse_metric_samples("", "kepler_container_cpu_joules_total", "container_name")
                .is_empty()
        );
    }

    #[test]
    fn parse_comments_only() {
        let body = "# HELP kepler_container_cpu_joules_total ...\n# TYPE kepler_container_cpu_joules_total counter\n";
        assert!(
            parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name")
                .is_empty()
        );
    }

    #[test]
    fn parse_single_container_sample() {
        let body = "kepler_container_cpu_joules_total{container_name=\"order-svc\",pod_name=\"p1\"} 1234.5\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label_value, "order-svc");
        assert!((out[0].value - 1234.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_multiple_containers() {
        let body = "kepler_container_cpu_joules_total{container_name=\"a\"} 100.0\n\
                    kepler_container_cpu_joules_total{container_name=\"b\"} 250.5\n\
                    kepler_container_cpu_joules_total{container_name=\"c\"} 999.9\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        assert_eq!(out.len(), 3);
        let names: Vec<&str> = out.iter().map(|s| s.label_value.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }

    #[test]
    fn parse_skips_unrelated_metrics() {
        let body = "kepler_node_info{name=\"foo\"} 1\n\
                    kepler_container_cpu_joules_total{container_name=\"order-svc\"} 42.0\n\
                    kepler_host_joules{name=\"bar\"} 7.0\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label_value, "order-svc");
    }

    #[test]
    fn parse_process_cpu_with_comm_label() {
        let body = "kepler_process_cpu_joules_total{comm=\"java\",pid=\"42\"} 88.0\n";
        let out = parse_metric_samples(body, "kepler_process_cpu_joules_total", "comm");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label_value, "java");
    }

    #[test]
    fn parse_skips_invalid_value() {
        let body = "kepler_container_cpu_joules_total{container_name=\"order-svc\"} not_a_number\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        assert!(out.is_empty());
    }

    #[test]
    fn parse_skips_missing_label() {
        let body = "kepler_container_cpu_joules_total{pod_name=\"only-pod\"} 5.0\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        assert!(out.is_empty());
    }

    #[test]
    fn parse_handles_escaped_quote_in_label() {
        let body =
            "kepler_container_cpu_joules_total{container_name=\"with \\\"quotes\\\"\"} 1.0\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label_value, "with \"quotes\"");
    }

    #[test]
    fn parse_handles_no_labels() {
        let body = "kepler_container_cpu_joules_total 99.0\n";
        let out = parse_metric_samples(body, "kepler_container_cpu_joules_total", "container_name");
        // No label block means no label_value, sample is skipped.
        assert!(out.is_empty());
    }

    // Alumet's exporter emits every attribute as a label alongside four
    // fixed `resource_*` labels, and OpenMetrics framing adds `# EOF`.
    // Shape taken from the upstream user-book exposition excerpt.
    #[test]
    fn parse_alumet_attributed_energy_by_pod_name() {
        let body = "# HELP attributed_energy_cpu_alumet attributed energy.\n\
                    # TYPE attributed_energy_cpu_alumet gauge\n\
                    attributed_energy_cpu_alumet{domain=\"package\",name=\"checkout-pod\",namespace=\"prod\",resource_consumer_id=\"\",resource_consumer_kind=\"cgroup\",resource_id=\"\",resource_kind=\"local_machine\"} 12.5\n\
                    # EOF\n";
        let out = parse_metric_samples(body, "attributed_energy_cpu_alumet", "name");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label_value, "checkout-pod");
        assert!((out[0].value - 12.5).abs() < f64::EPSILON);
    }

    // A shorter metric name must not match a longer one that starts with
    // it: `strip_prefix` alone would let `rapl_consumed_energy_alumet`
    // swallow a line for `rapl_consumed_energy_alumet_joules`.
    #[test]
    fn parse_does_not_match_longer_metric_name_prefix() {
        let body = "rapl_consumed_energy_alumet_joules{domain=\"package\"} 7.0\n";
        let out = parse_metric_samples(body, "rapl_consumed_energy_alumet", "domain");
        assert!(out.is_empty());
    }

    #[test]
    fn prom_sample_is_constructible_for_callers() {
        let s = PromSample {
            label_value: "svc".to_string(),
            value: 1.0,
        };
        assert_eq!(s.label_value, "svc");
    }
}
