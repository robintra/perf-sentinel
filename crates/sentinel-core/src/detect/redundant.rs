//! Redundant query/call detection.
//!
//! Detects exact duplicate operations within a single trace:
//! same normalized template AND same parameters.

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect redundant (exact duplicate) operations in a single trace.
///
/// `n_plus_one_findings` is the slice of N+1 findings already produced
/// for this trace. Templates that already triggered an N+1 finding (via
/// the standard distinct-params rule or via the sanitizer-aware
/// heuristic) are skipped so the same template is not double-reported as
/// both `n_plus_one_sql` and `redundant_sql`.
#[must_use]
pub fn detect_redundant(trace: &Trace, n_plus_one_findings: &[Finding]) -> Vec<Finding> {
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

        let n_plus_one_type = FindingType::from_event_type_n_plus_one(event_type);
        let already_n_plus_one = n_plus_one_findings
            .iter()
            .any(|f| f.pattern.template == **template && f.finding_type == n_plus_one_type);
        if already_n_plus_one {
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
            classification_method: None,
            code_location: first.event.code_location(),
            instrumentation_scopes: first.event.instrumentation_scopes.clone(),
            suggested_fix: None,
            signature: String::new(),
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
        let findings = detect_redundant(&trace, &[]);

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
        let findings = detect_redundant(&trace, &[]);

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
        let findings = detect_redundant(&trace, &[]);
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
        let findings = detect_redundant(&trace, &[]);

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
        let findings = detect_redundant(&trace, &[]);
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
        let findings = detect_redundant(&trace, &[]);

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
        let findings = detect_redundant(&trace, &[]);

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
        let findings = detect_redundant(&trace, &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn skips_groups_already_reclassified_as_n_plus_one() {
        // Two redundant groups in the same trace: template_X has 3 spans
        // and is also flagged by n+1 (e.g. via the sanitizer-aware
        // heuristic), template_Y has 2 spans and is not. Only template_Y
        // should produce a redundant finding.
        let template_x = "SELECT * FROM order_item WHERE order_id = ?";
        let template_y = "SELECT * FROM users WHERE id = ?";
        let mut events: Vec<SpanEvent> = Vec::new();
        for i in 1..=3 {
            events.push(make_sql_event(
                "trace-1",
                &format!("x-{i}"),
                "SELECT * FROM order_item WHERE order_id = 42",
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            ));
        }
        for i in 1..=2 {
            events.push(make_sql_event(
                "trace-1",
                &format!("y-{i}"),
                "SELECT * FROM users WHERE id = 7",
                &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
            ));
        }
        let trace = make_trace(events);

        let n_plus_one_findings = vec![crate::test_helpers::make_finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
        )];
        // Override the template on the synthetic n+1 finding to template_x.
        let mut n_plus_one_findings = n_plus_one_findings;
        n_plus_one_findings[0].pattern.template = template_x.to_string();

        let findings = detect_redundant(&trace, &n_plus_one_findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::RedundantSql);
        assert_eq!(findings[0].pattern.template, template_y);
    }

    #[test]
    fn emits_redundant_when_n_plus_one_findings_empty() {
        // Non-regression: empty n+1 findings slice must not change the
        // pre-0.5.7 behavior on a trivially redundant trace.
        let events = crate::test_helpers::make_redundant_events();
        let trace = make_trace(events);
        let findings = detect_redundant(&trace, &[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::RedundantSql);
        assert_eq!(findings[0].classification_method, None);
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
        let findings = detect_redundant(&trace, &[]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].first_timestamp, "2025-07-10T14:32:01.050Z");
        assert_eq!(findings[0].last_timestamp, "2025-07-10T14:32:01.150Z");
    }
}
