//! Slow query/call detection.
//!
//! Detects recurring slow operations within a single trace where
//! `duration_us` exceeds a configurable threshold and the same
//! normalized template occurs >= N times.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Finding, FindingType, Pattern, Severity};

/// Detect recurring slow operations in a single trace.
///
/// Flags operations where `duration_us > threshold_ms * 1000` and
/// the same normalized template occurs >= `min_occurrences` times.
#[must_use]
pub fn detect_slow(trace: &Trace, threshold_ms: u64, min_occurrences: u32) -> Vec<Finding> {
    let threshold_us = threshold_ms.saturating_mul(1000);
    let min_occ = min_occurrences as usize;

    // Group slow spans by (event_type, template)
    let mut groups: HashMap<(&EventType, &str), Vec<usize>> = HashMap::new();
    for (i, span) in trace.spans.iter().enumerate() {
        if span.event.duration_us > threshold_us {
            groups
                .entry((&span.event.event_type, &span.template))
                .or_default()
                .push(i);
        }
    }

    let mut findings = Vec::new();
    for ((event_type, template), indices) in &groups {
        if indices.len() < min_occ {
            continue;
        }

        // Find max duration for severity calculation
        let max_duration_us = indices
            .iter()
            .map(|&i| trace.spans[i].event.duration_us)
            .max()
            .unwrap_or(0);

        // Severity: Critical if > 5x threshold, Warning otherwise
        let severity = if max_duration_us > threshold_us.saturating_mul(5) {
            Severity::Critical
        } else {
            Severity::Warning
        };

        // Compute timestamps and window (no allocation)
        let (window_ms, min_ts, max_ts) = super::n_plus_one::compute_window_and_bounds_iter(
            indices
                .iter()
                .map(|&i| trace.spans[i].event.timestamp.as_str()),
        );

        // Count distinct params
        let distinct_params: HashSet<&[String]> = indices
            .iter()
            .map(|&i| trace.spans[i].params.as_slice())
            .collect();

        let first = &trace.spans[indices[0]];

        let suggestion = match event_type {
            EventType::Sql => "Consider adding an index or optimizing query".to_string(),
            EventType::HttpOut => "Consider caching or optimizing endpoint".to_string(),
        };

        findings.push(Finding {
            finding_type: FindingType::from_event_type_slow(event_type),
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
            first_timestamp: min_ts.to_string(),
            last_timestamp: max_ts.to_string(),
            green_impact: None,
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{
        make_http_event_with_duration, make_sql_event_with_duration, make_trace,
    };

    #[test]
    fn detects_slow_sql() {
        let events = vec![
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                700_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s3",
                "SELECT * FROM t WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
                650_000,
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::SlowSql);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 3);
        assert!(findings[0].suggestion.contains("index"));
    }

    #[test]
    fn below_min_occurrences_no_finding() {
        let events = vec![
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                700_000,
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);
        assert!(findings.is_empty());
    }

    #[test]
    fn below_threshold_duration_no_finding() {
        let events = vec![
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                400_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                300_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s3",
                "SELECT * FROM t WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
                450_000,
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);
        assert!(findings.is_empty());
    }

    #[test]
    fn critical_severity_5x_threshold() {
        let events = vec![
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                700_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s3",
                "SELECT * FROM t WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
                2_600_000, // > 5 * 500ms = 2500ms
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn warning_severity_above_threshold() {
        let events = vec![
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                700_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s3",
                "SELECT * FROM t WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
                800_000, // < 5 * 500ms
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn mixed_slow_and_fast_same_template() {
        // 5 events with same template, but only 3 are slow
        let events = vec![
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                100_000, // fast
            ),
            make_sql_event_with_duration(
                "t1",
                "s3",
                "SELECT * FROM t WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
                700_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s4",
                "SELECT * FROM t WHERE id = 4",
                "2025-07-10T14:32:01.150Z",
                200_000, // fast
            ),
            make_sql_event_with_duration(
                "t1",
                "s5",
                "SELECT * FROM t WHERE id = 5",
                "2025-07-10T14:32:01.200Z",
                650_000,
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern.occurrences, 3); // only the slow ones
    }

    #[test]
    fn detects_slow_http() {
        let events = vec![
            make_http_event_with_duration(
                "t1",
                "s1",
                "http://svc:5000/api/data/1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_http_event_with_duration(
                "t1",
                "s2",
                "http://svc:5000/api/data/2",
                "2025-07-10T14:32:01.050Z",
                700_000,
            ),
            make_http_event_with_duration(
                "t1",
                "s3",
                "http://svc:5000/api/data/3",
                "2025-07-10T14:32:01.100Z",
                650_000,
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::SlowHttp);
        assert!(findings[0].suggestion.contains("caching"));
    }

    #[test]
    fn different_templates_separate_findings() {
        let events = vec![
            // Template A: 3 slow
            make_sql_event_with_duration(
                "t1",
                "s1",
                "SELECT * FROM t WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
                600_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s2",
                "SELECT * FROM t WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
                700_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s3",
                "SELECT * FROM t WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
                650_000,
            ),
            // Template B: 3 slow
            make_sql_event_with_duration(
                "t1",
                "s4",
                "SELECT * FROM orders WHERE user_id = 1",
                "2025-07-10T14:32:01.150Z",
                800_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s5",
                "SELECT * FROM orders WHERE user_id = 2",
                "2025-07-10T14:32:01.200Z",
                900_000,
            ),
            make_sql_event_with_duration(
                "t1",
                "s6",
                "SELECT * FROM orders WHERE user_id = 3",
                "2025-07-10T14:32:01.250Z",
                850_000,
            ),
        ];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);

        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn empty_trace_no_findings() {
        let trace = Trace {
            trace_id: "t1".to_string(),
            spans: vec![],
        };
        let findings = detect_slow(&trace, 500, 3);
        assert!(findings.is_empty());
    }
}
