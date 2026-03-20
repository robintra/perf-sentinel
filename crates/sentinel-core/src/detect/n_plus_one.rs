//! N+1 detection for SQL queries and HTTP calls.
//!
//! Detects patterns where the same normalized template is called N times
//! with different parameters within a single trace, inside a configurable
//! time window.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Finding, FindingType, Pattern, Severity};

/// Detect N+1 patterns in a single trace.
///
/// Groups spans by (`event_type`, template) and flags groups where:
/// - The number of occurrences >= threshold
/// - The number of distinct parameter sets >= threshold
/// - The time window of occurrences <= `window_limit`
#[must_use]
pub fn detect_n_plus_one(trace: &Trace, threshold: u32, window_limit: u64) -> Vec<Finding> {
    let threshold = threshold as usize;

    // Group spans by (event_type, template) using borrowed keys
    let mut groups: HashMap<(&EventType, &str), Vec<usize>> = HashMap::new();
    for (i, span) in trace.spans.iter().enumerate() {
        groups
            .entry((&span.event.event_type, &span.template))
            .or_default()
            .push(i);
    }

    let mut findings = Vec::new();

    for ((event_type, template), indices) in &groups {
        if indices.len() < threshold {
            continue;
        }

        // Count distinct parameter sets using borrowed slices
        let distinct_params: HashSet<&[String]> = indices
            .iter()
            .map(|&i| trace.spans[i].params.as_slice())
            .collect();

        if distinct_params.len() < threshold {
            continue;
        }

        // Compute window from timestamps (lexicographic sort works for ISO 8601)
        let timestamps: Vec<&str> = indices
            .iter()
            .map(|&i| trace.spans[i].event.timestamp.as_str())
            .collect();
        let window_ms = compute_window_ms(&timestamps);

        // Filter out groups that span beyond the window limit
        if window_ms > window_limit {
            continue;
        }

        // Use the first span for metadata
        let first = &trace.spans[indices[0]];
        let severity = if indices.len() >= 10 {
            Severity::Critical
        } else {
            Severity::Warning
        };

        let suggestion = match event_type {
            EventType::Sql => format!(
                "Use WHERE ... IN (?) to batch {} queries into one",
                indices.len()
            ),
            EventType::HttpOut => format!(
                "Use batch endpoint with ?ids=... to batch {} calls into one",
                indices.len()
            ),
        };

        findings.push(Finding {
            finding_type: FindingType::from_event_type_n_plus_one(event_type),
            severity,
            trace_id: trace.trace_id.clone(),
            service: first.event.service.clone(),
            source_endpoint: first.event.source.endpoint.clone(),
            pattern: Pattern {
                template: (*template).to_string(),
                occurrences: indices.len(),
                window_ms,
                distinct_params: distinct_params.len(),
            },
            suggestion,
        });
    }

    findings
}

/// Compute the time window in milliseconds between the earliest and latest timestamps.
/// Expects ISO 8601 format: `YYYY-MM-DDTHH:MM:SS.mmmZ`
pub(crate) fn compute_window_ms(timestamps: &[&str]) -> u64 {
    if timestamps.len() < 2 {
        return 0;
    }

    let mut min_ts = timestamps[0];
    let mut max_ts = timestamps[0];
    for &ts in &timestamps[1..] {
        if ts < min_ts {
            min_ts = ts;
        }
        if ts > max_ts {
            max_ts = ts;
        }
    }

    let min_ms = parse_timestamp_ms(min_ts);
    let max_ms = parse_timestamp_ms(max_ts);

    match (min_ms, max_ms) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    }
}

/// Parse an ISO 8601 timestamp to milliseconds since midnight.
/// Format: `YYYY-MM-DDTHH:MM:SS.mmmZ`
fn parse_timestamp_ms(ts: &str) -> Option<u64> {
    // Find the 'T' separator
    let time_part = ts.split('T').nth(1)?;
    // Strip trailing 'Z' or timezone
    let time_part = time_part.trim_end_matches('Z');

    let parts: Vec<&str> = time_part.split(':').collect();
    if parts.len() < 3 {
        return None;
    }

    let hours: u64 = parts[0].parse().ok()?;
    let minutes: u64 = parts[1].parse().ok()?;

    // Seconds may have fractional part
    let sec_parts: Vec<&str> = parts[2].split('.').collect();
    let seconds: u64 = sec_parts[0].parse().ok()?;
    let millis: u64 = if sec_parts.len() > 1 {
        let frac = sec_parts[1];
        match frac.len() {
            0 => 0,
            1 => frac.parse::<u64>().unwrap_or(0) * 100,
            2 => frac.parse::<u64>().unwrap_or(0) * 10,
            _ => frac[..3].parse::<u64>().unwrap_or(0),
        }
    } else {
        0
    };

    Some(hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SpanEvent;
    use crate::test_helpers::{make_http_event, make_sql_event, make_trace};

    #[test]
    fn detects_n_plus_one_sql() {
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 6);
        assert_eq!(findings[0].pattern.distinct_params, 6);
        assert!(findings[0].suggestion.contains("batch"));
    }

    #[test]
    fn detects_n_plus_one_http() {
        let events: Vec<SpanEvent> = (101..=106)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://account-svc:5000/api/account/{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", (i - 100) * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneHttp);
        assert_eq!(findings[0].pattern.occurrences, 6);
        assert!(findings[0].suggestion.contains("batch endpoint"));
    }

    #[test]
    fn below_threshold_no_finding() {
        let events: Vec<SpanEvent> = (1..=4)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);
        assert!(findings.is_empty());
    }

    #[test]
    fn mixed_templates_no_finding() {
        let events = vec![
            make_sql_event(
                "trace-1",
                "span-1",
                "SELECT * FROM users WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
            ),
            make_sql_event(
                "trace-1",
                "span-2",
                "SELECT * FROM orders WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
            ),
            make_sql_event(
                "trace-1",
                "span-3",
                "INSERT INTO logs (msg) VALUES ('hello')",
                "2025-07-10T14:32:01.100Z",
            ),
        ];

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);
        assert!(findings.is_empty());
    }

    #[test]
    fn critical_severity_for_10_or_more() {
        let events: Vec<SpanEvent> = (1..=12)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[0].pattern.occurrences, 12);
    }

    #[test]
    fn same_params_not_n_plus_one() {
        // 6 events with same template AND same params -> not N+1 (that's redundant)
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM player WHERE game_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);
        // Only 1 distinct param set, below threshold of 5
        assert!(findings.is_empty());
    }

    #[test]
    fn window_exceeded_no_finding() {
        // 6 events spread over 10 seconds — exceeds 500ms window limit
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:{:02}.000Z", i * 2),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);
        assert!(findings.is_empty());
    }

    #[test]
    fn compute_window_ms_basic() {
        let timestamps = vec![
            "2025-07-10T14:32:01.000Z",
            "2025-07-10T14:32:01.250Z",
            "2025-07-10T14:32:01.100Z",
        ];
        assert_eq!(compute_window_ms(&timestamps), 250);
    }

    #[test]
    fn parse_timestamp_ms_basic() {
        assert_eq!(
            parse_timestamp_ms("2025-07-10T14:32:01.123Z"),
            Some(14 * 3_600_000 + 32 * 60_000 + 1 * 1_000 + 123)
        );
    }

    #[test]
    fn parse_timestamp_ms_single_frac_digit() {
        // "01.1Z" should be 100ms
        assert_eq!(parse_timestamp_ms("2025-07-10T00:00:01.1Z"), Some(1_100));
    }

    #[test]
    fn parse_timestamp_ms_two_frac_digits() {
        // "01.12Z" should be 120ms
        assert_eq!(parse_timestamp_ms("2025-07-10T00:00:01.12Z"), Some(1_120));
    }

    #[test]
    fn window_at_exact_limit_still_detected() {
        // 5 events spanning exactly 500ms -> window_ms == 500, limit == 500
        // Code uses `>` so `==` should pass
        let events: Vec<SpanEvent> = (0..5)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {}", i + 1),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 125),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern.window_ms, 500);
    }

    #[test]
    fn window_zero_limit_filters_all() {
        // window_limit = 0 -> only events with identical timestamps pass
        let events: Vec<SpanEvent> = (1..=5)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 0);
        assert!(findings.is_empty());
    }

    #[test]
    fn severity_boundary_9_is_warning() {
        let events: Vec<SpanEvent> = (1..=9)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 9);
    }

    #[test]
    fn severity_boundary_10_is_critical() {
        let events: Vec<SpanEvent> = (1..=10)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[0].pattern.occurrences, 10);
    }

    #[test]
    fn compute_window_ms_single_timestamp() {
        let timestamps = vec!["2025-07-10T14:32:01.000Z"];
        assert_eq!(compute_window_ms(&timestamps), 0);
    }

    #[test]
    fn compute_window_ms_empty() {
        let timestamps: Vec<&str> = vec![];
        assert_eq!(compute_window_ms(&timestamps), 0);
    }

    #[test]
    fn parse_timestamp_ms_no_fractional() {
        // No fractional part -> millis = 0
        assert_eq!(
            parse_timestamp_ms("2025-07-10T14:32:01Z"),
            Some(14 * 3_600_000 + 32 * 60_000 + 1_000)
        );
    }

    #[test]
    fn parse_timestamp_ms_invalid_returns_none() {
        assert_eq!(parse_timestamp_ms("not-a-timestamp"), None);
    }

    #[test]
    fn parse_timestamp_ms_missing_parts() {
        // Only 2 colon-separated parts (HH:MM, no seconds) -> None
        assert_eq!(parse_timestamp_ms("2025-07-10T14:32Z"), None);
    }
}
