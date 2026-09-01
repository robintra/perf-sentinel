//! Redundant query/call detection.
//!
//! Detects exact duplicate operations within a single trace:
//! same normalized template AND same parameters.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;
use crate::normalize::sql;

use super::{Confidence, Finding, FindingType, Pattern, Severity};

type RedundantKey<'a> = (
    &'a EventType,
    &'a str,
    &'a [String],
    Option<(&'a str, &'a str)>,
);
type NPlusOneKey<'a> = (&'a FindingType, &'a str, Option<(&'a str, &'a str)>);

/// Detect redundant (exact duplicate) operations in a single trace.
///
/// `n_plus_one_findings` is the slice of N+1 findings already produced
/// for this trace. Templates that already triggered an N+1 finding (via
/// the standard distinct-params rule or via the sanitizer-aware
/// heuristic) are skipped so the same template is not double-reported as
/// both `n_plus_one_sql` and `redundant_sql`.
#[must_use]
pub fn detect_redundant(trace: &Trace, n_plus_one_findings: &[Finding]) -> Vec<Finding> {
    redundant_impl(trace, n_plus_one_findings, false)
        .into_iter()
        .map(|(finding, _)| finding)
        .collect()
}

/// Same detection, also returning the exact duplicate span ids for HTML proof.
#[must_use]
pub(crate) fn detect_redundant_with_spans<'a>(
    trace: &'a Trace,
    n_plus_one_findings: &[Finding],
) -> Vec<(Finding, Vec<&'a str>)> {
    redundant_impl(trace, n_plus_one_findings, true)
}

fn redundant_impl<'a>(
    trace: &'a Trace,
    n_plus_one_findings: &[Finding],
    collect_spans: bool,
) -> Vec<(Finding, Vec<&'a str>)> {
    // Use borrowed keys: (&EventType, &str, &[String]) avoids cloning and
    // eliminates the join-ambiguity bug (a param containing the separator
    // could cause two different param lists to collide).
    let mut groups: HashMap<RedundantKey<'_>, Vec<usize>> =
        HashMap::with_capacity(trace.spans.len().min(64));

    for (i, span) in trace.spans.iter().enumerate() {
        // Messaging carries no params, so every publish to one destination
        // would group as a duplicate. Two messages are never the same message.
        if span.event.event_type == EventType::Messaging {
            continue;
        }
        // One per connection checkout, not a repeated read. Caching applies to
        // neither.
        if span.event.event_type == EventType::Sql && sql::is_session_command(&span.template) {
            continue;
        }
        groups
            .entry((
                &span.event.event_type,
                &span.template,
                &span.params,
                span.event.grouping_identity(),
            ))
            .or_default()
            .push(i);
    }

    let mut findings = Vec::new();

    // Index N+1 groups once to avoid O(G*F) per-group scans. The type and
    // grouping identity keep unrelated redundant hits visible.
    let n_plus_one_index: HashSet<NPlusOneKey<'_>> = n_plus_one_findings
        .iter()
        .map(|finding| {
            (
                &finding.finding_type,
                finding.pattern.template.as_str(),
                finding.grouping_identity(),
            )
        })
        .collect();

    for ((event_type, template, _params, grouping), indices) in &groups {
        if indices.len() < 2 {
            continue;
        }

        let n_plus_one_type = FindingType::from_event_type_n_plus_one(event_type);
        if n_plus_one_index.contains(&(&n_plus_one_type, *template, *grouping)) {
            continue;
        }
        let Some(finding_type) = FindingType::from_event_type_redundant(event_type) else {
            continue;
        };

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

        let finding = Finding {
            finding_type,
            severity,
            trace_id: trace.trace_id.clone(),
            service: first.event.service.to_string(),
            grouping: first.event.grouping.clone(),
            source_endpoint: first.event.source.endpoint.clone(),
            pattern: Pattern {
                template: (*template).to_string(),
                occurrences: indices.len(),
                occurrences_by_service: crate::detect::occurrences_by_service(trace, indices),
                window_ms,
                distinct_params: 1,
                ..Default::default()
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
            instrumentation_scopes: first
                .event
                .instrumentation_scopes
                .iter()
                .map(ToString::to_string)
                .collect(),
            suggested_fix: None,
            signature: String::new(),
        };
        let span_ids = crate::detect::member_span_ids(trace, indices, collect_spans);
        findings.push((finding, span_ids));
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
        assert!(findings[0].pattern.occurrences_by_service.is_empty());
    }

    /// Same as the N+1 case: the redundant key has no service either.
    #[test]
    fn redundant_across_services_splits_occurrences() {
        let mut events = crate::test_helpers::make_redundant_events();
        for (i, event) in events.iter_mut().enumerate() {
            event.service = std::sync::Arc::from(if i < 2 { "order-svc" } else { "inventory-svc" });
        }
        let trace = make_trace(events);
        let findings = detect_redundant(&trace, &[]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].service, "order-svc");
        assert_eq!(
            findings[0].pattern.occurrences_by_service,
            std::collections::BTreeMap::from([
                ("inventory-svc".to_string(), 1),
                ("order-svc".to_string(), 2),
            ])
        );
        let credit: Vec<_> = findings[0].avoidable_by_service().collect();
        assert_eq!(credit, [("order-svc", 1), ("inventory-svc", 1)]);
    }

    #[test]
    fn session_commands_are_never_redundant() {
        let mut events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT set_config(?, ?, false)",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();
        // Positive control: a real duplicate in the same trace must survive,
        // so the skip cannot pass by silencing everything.
        events.extend((1..=2).map(|i| {
            make_sql_event(
                "trace-1",
                &format!("order-{i}"),
                "SELECT * FROM order_item WHERE order_id = 42",
                &format!("2025-07-10T14:32:01.{:03}Z", 200 + i * 10),
            )
        }));
        let trace = make_trace(events);

        let findings = detect_redundant(&trace, &[]);

        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert!(
            findings[0].pattern.template.contains("order_item"),
            "six connection checkouts are not a redundant query: {findings:?}"
        );
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
    fn redundant_groups_keep_equal_values_from_different_keys_separate() {
        let mut events: Vec<_> = (0..4)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_item WHERE order_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        for (i, event) in events.iter_mut().enumerate() {
            let key = if i < 2 {
                "tenant.id"
            } else {
                "k8s.namespace.name"
            };
            event.grouping = crate::test_helpers::grouping(key, "prod");
        }
        let trace = make_trace(events);

        let findings = detect_redundant(&trace, &[]);

        assert_eq!(findings.len(), 2, "{findings:#?}");
        assert!(findings.iter().all(|f| f.pattern.occurrences == 2));
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
        // Non-regression: an empty n+1 findings slice must leave a
        // trivially redundant trace classified as redundant_sql.
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
