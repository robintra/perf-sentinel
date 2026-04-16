//! Chatty service detection: identifies traces with excessive inter-service HTTP calls.

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect chatty service patterns within a trace.
///
/// A trace with more than `min_calls` HTTP outbound spans is flagged.
/// Severity is `Warning` if > `min_calls`, `Critical` if > 3x `min_calls`.
#[must_use]
pub fn detect_chatty(trace: &Trace, min_calls: u32) -> Vec<Finding> {
    // Fast-path gate: count HttpOut spans without materializing the
    // index Vec. The common case (traces below threshold) now exits
    // after one filter+count pass, with zero heap allocation.
    let count = trace
        .spans
        .iter()
        .filter(|s| s.event.event_type == EventType::HttpOut)
        .count();
    if count <= min_calls as usize {
        return vec![];
    }

    // Chatty-flagged path: collect indices now that we know we need
    // them for the template-count + window-computation iterations below.
    let http_indices: Vec<usize> = trace
        .spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.event.event_type == EventType::HttpOut)
        .map(|(i, _)| i)
        .collect();

    let severity = if count > (min_calls as usize) * 3 {
        Severity::Critical
    } else {
        Severity::Warning
    };

    // Count occurrences per normalized template for "top N" display
    let mut template_counts: HashMap<&str, usize> =
        HashMap::with_capacity(http_indices.len().min(64));
    for &idx in &http_indices {
        *template_counts
            .entry(trace.spans[idx].template.as_str())
            .or_default() += 1;
    }

    // Find top-2 by count. For large trace fan-ins (dozens of distinct
    // endpoints) this is O(k) via `select_nth_unstable_by` instead of
    // the O(k log k) full sort we used previously. Only runs on
    // chatty-flagged traces, so the difference is cosmetic, but avoids
    // gratuitous work on traces with high endpoint cardinality.
    let mut entries: Vec<(&str, usize)> = template_counts.iter().map(|(&k, &v)| (k, v)).collect();
    let top_two = if entries.len() <= 2 {
        entries.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        entries
    } else {
        // Partition so the first 2 are >= the rest, then sort just those 2.
        entries.select_nth_unstable_by(1, |a, b| b.1.cmp(&a.1));
        entries.truncate(2);
        entries.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        entries
    };
    let top_str: String = top_two
        .iter()
        .map(|(tmpl, cnt)| format!("{tmpl} x{cnt}"))
        .collect::<Vec<_>>()
        .join(", ");

    let first = &trace.spans[http_indices[0]];
    let entry_endpoint = first.event.source.endpoint.clone();
    let distinct_targets = template_counts.len();

    let (window_ms, first_ts, last_ts) = super::n_plus_one::compute_window_and_bounds_iter(
        http_indices
            .iter()
            .map(|&i| trace.spans[i].event.timestamp.as_str()),
    );

    let suggestion = format!(
        "Chatty trace: {entry_endpoint} triggers {count} inter-service HTTP calls \
         (top: {top_str}). Consider aggregating calls with a batch endpoint \
         or a BFF (Backend for Frontend) layer"
    );

    vec![Finding {
        finding_type: FindingType::ChattyService,
        severity,
        trace_id: trace.trace_id.clone(),
        service: first.event.service.clone(),
        source_endpoint: entry_endpoint.clone(),
        pattern: Pattern {
            template: entry_endpoint,
            occurrences: count,
            window_ms,
            distinct_params: distinct_targets,
        },
        suggestion,
        first_timestamp: first_ts.to_string(),
        last_timestamp: last_ts.to_string(),
        green_impact: None,
        confidence: Confidence::default(),
        code_location: None,
        suggested_fix: None,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{make_http_event, make_sql_event, make_trace};

    #[test]
    fn detects_chatty_trace() {
        let events: Vec<_> = (1..=20)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc-{}/api/resource/{i}", i % 5),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::ChattyService);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 20);
    }

    #[test]
    fn critical_at_3x_threshold() {
        let events: Vec<_> = (1..=50)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc-{}/api/resource/{i}", i % 5),
                    &format!("2025-07-10T14:32:01.{:03}Z", i % 1000),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn no_finding_below_threshold() {
        let events: Vec<_> = (1..=10)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc/api/resource/{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);
        assert!(findings.is_empty());
    }

    #[test]
    fn no_finding_at_threshold() {
        let events: Vec<_> = (1..=15)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc/api/resource/{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);
        assert!(findings.is_empty());
    }

    #[test]
    fn sql_events_not_counted() {
        let events: Vec<_> = (1..=20)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);
        assert!(findings.is_empty());
    }

    #[test]
    fn mixed_events_only_counts_http() {
        let mut events: Vec<_> = (1..=10)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-sql-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();
        events.extend((1..=10).map(|i| {
            make_http_event(
                "trace-1",
                &format!("span-http-{i}"),
                &format!("http://svc/api/resource/{i}"),
                &format!("2025-07-10T14:32:02.{:03}Z", i * 10),
            )
        }));

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);
        assert!(findings.is_empty(), "10 HTTP calls <= 15 threshold");
    }

    #[test]
    fn distinct_params_counts_templates() {
        // 20 HTTP events, 5 going to template A, 15 going to template B
        let mut events: Vec<_> = (1..=5)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-a{i}"),
                    &format!("http://svc-a/api/users/{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();
        events.extend((1..=15).map(|i| {
            make_http_event(
                "trace-1",
                &format!("span-b{i}"),
                &format!("http://svc-b/api/orders/{i}"),
                &format!("2025-07-10T14:32:02.{:03}Z", i * 10),
            )
        }));

        let trace = make_trace(events);
        let findings = detect_chatty(&trace, 15);
        assert_eq!(findings.len(), 1);
        // Two distinct normalized templates
        assert_eq!(findings[0].pattern.distinct_params, 2);
    }
}
