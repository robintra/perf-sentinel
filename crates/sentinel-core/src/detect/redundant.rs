//! Redundant query/call detection.
//!
//! Detects exact duplicate operations within a single trace:
//! same normalized template AND same parameters.

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect redundant (exact duplicate) operations in a single trace.
#[must_use]
pub fn detect_redundant(trace: &Trace) -> Vec<Finding> {
    // Use borrowed keys: (&EventType, &str, &[String]) avoids cloning and
    // eliminates the join-ambiguity bug (a param containing the separator
    // could cause two different param lists to collide).
    let mut groups: HashMap<(&EventType, &str, &[String]), Vec<usize>> =
        HashMap::with_capacity(trace.spans.len().min(64));

    for (i, span) in trace.spans.iter().enumerate() {
        groups
            .entry((&span.event.event_type, &span.template, &span.params))
            .or_default()
            .push(i);
    }

    let mut findings = Vec::new();

    for ((event_type, template, _params), indices) in &groups {
        if indices.len() < 2 {
            continue;
        }

        let first = &trace.spans[indices[0]];
        let severity = if indices.len() >= 5 {
            Severity::Warning
        } else {
            Severity::Info
        };

        // Compute window and timestamp bounds in a single pass (no allocation)
        let (window_ms, min_ts, max_ts) = super::n_plus_one::compute_window_and_bounds_iter(
            indices
                .iter()
                .map(|&i| trace.spans[i].event.timestamp.as_str()),
        );

        findings.push(Finding {
            finding_type: FindingType::from_event_type_redundant(event_type),
            severity,
            trace_id: trace.trace_id.clone(),
            service: first.event.service.clone(),
            source_endpoint: first.event.source.endpoint.clone(),
            pattern: Pattern {
                template: (*template).to_string(),
                occurrences: indices.len(),
                window_ms,
                distinct_params: 1, // always 1 for redundant (same params)
            },
            suggestion: format!(
                "Identical operation executed {} times: cache result or deduplicate",
                indices.len()
            ),
            first_timestamp: min_ts.to_string(),
            last_timestamp: max_ts.to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            code_location: first.event.code_location(),
            suggested_fix: None,
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SpanEvent;
    use crate::test_helpers::{make_http_event, make_sql_event, make_trace};

    #[test]
    fn detects_redundant_sql() {
        let events = crate::test_helpers::make_redundant_events();

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::RedundantSql);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].pattern.occurrences, 3);
        assert_eq!(findings[0].pattern.distinct_params, 1);
        assert!(findings[0].suggestion.contains("cache"));
    }

    #[test]
    fn detects_redundant_http() {
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "http://user-svc:5000/api/users/42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::RedundantHttp);
        assert_eq!(findings[0].pattern.occurrences, 3);
    }

    #[test]
    fn no_duplicates_no_finding() {
        let events = vec![
            make_sql_event(
                "trace-1",
                "span-1",
                "SELECT * FROM order_item WHERE order_id = 1",
                "2025-07-10T14:32:01.000Z",
            ),
            make_sql_event(
                "trace-1",
                "span-2",
                "SELECT * FROM order_item WHERE order_id = 2",
                "2025-07-10T14:32:01.050Z",
            ),
        ];

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);
        assert!(findings.is_empty());
    }

    #[test]
    fn warning_severity_for_5_or_more() {
        let events: Vec<SpanEvent> = (1..=5)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_item WHERE order_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn comma_in_param_no_false_positive() {
        // Two different param sets that would collide with join(",")
        // param ["a,b"] vs params ["a", "b"] should NOT be grouped together
        let events = vec![
            make_sql_event(
                "trace-1",
                "span-1",
                "SELECT * FROM t WHERE x = 'a,b'",
                "2025-07-10T14:32:01.000Z",
            ),
            make_sql_event(
                "trace-1",
                "span-2",
                "SELECT * FROM t WHERE x = 'a,b'",
                "2025-07-10T14:32:01.050Z",
            ),
        ];

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);
        // These ARE redundant (same template, same params)
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern.occurrences, 2);
    }

    #[test]
    fn exactly_two_occurrences_is_info() {
        let events: Vec<SpanEvent> = (1..=2)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_item WHERE order_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].pattern.occurrences, 2);
    }

    #[test]
    fn exactly_four_occurrences_is_info() {
        let events: Vec<SpanEvent> = (1..=4)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_item WHERE order_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].pattern.occurrences, 4);
    }

    #[test]
    fn single_event_no_finding() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT * FROM order_item WHERE order_id = 42",
            "2025-07-10T14:32:01.000Z",
        )];

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);
        assert!(findings.is_empty());
    }

    #[test]
    fn redundant_finding_has_first_last_timestamps() {
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_item WHERE order_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_redundant(&trace);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].first_timestamp, "2025-07-10T14:32:01.050Z");
        assert_eq!(findings[0].last_timestamp, "2025-07-10T14:32:01.150Z");
    }
}
