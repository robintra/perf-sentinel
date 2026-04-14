//! Fanout detection: identifies parent spans generating excessive child spans.

use crate::correlate::Trace;
use crate::detect::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect excessive fanout within a trace.
///
/// A parent span with more than `max_fanout` children is flagged.
/// Severity is `Warning` if > `max_fanout`, `Critical` if > 3x `max_fanout`.
#[must_use]
pub fn detect_fanout(trace: &Trace, max_fanout: u32) -> Vec<Finding> {
    let children_by_parent = super::group_children_by_parent(trace);
    let span_index = super::build_span_index(trace);

    let mut findings = Vec::new();

    for (parent_id, child_indices) in &children_by_parent {
        let count = child_indices.len();
        if count <= max_fanout as usize {
            continue;
        }

        let severity = if count > (max_fanout as usize) * 3 {
            Severity::Critical
        } else {
            Severity::Warning
        };

        // Find the parent span for context (service, endpoint)
        let parent_span = span_index.get(*parent_id).map(|&i| &trace.spans[i]);

        let service = parent_span.map_or_else(
            || trace.spans[child_indices[0]].event.service.clone(),
            |s| s.event.service.clone(),
        );

        let endpoint = parent_span.map_or_else(
            || trace.spans[child_indices[0]].event.source.endpoint.clone(),
            |s| s.event.source.endpoint.clone(),
        );

        // Compute window from children timestamps in one pass (no intermediate Vec)
        let (window_ms, first_ts, last_ts) =
            crate::detect::n_plus_one::compute_window_and_bounds_iter(
                child_indices
                    .iter()
                    .map(|&i| trace.spans[i].event.timestamp.as_str()),
            );
        let first_ts = first_ts.to_string();
        let last_ts = last_ts.to_string();

        // Parent template (operation name or span template)
        let template =
            parent_span.map_or_else(|| format!("parent:{parent_id}"), |s| s.template.clone());

        findings.push(Finding {
            finding_type: FindingType::ExcessiveFanout,
            severity,
            trace_id: trace.trace_id.clone(),
            service,
            source_endpoint: endpoint,
            pattern: Pattern {
                template,
                occurrences: count,
                window_ms,
                distinct_params: count,
            },
            suggestion: format!(
                "Parent span has {count} children (threshold: {max_fanout}). \
                 Consider batching child operations to reduce fanout."
            ),
            first_timestamp: first_ts,
            last_timestamp: last_ts,
            green_impact: None,
            confidence: Confidence::default(),
            code_location: None,
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{make_sql_event, make_trace};

    fn make_events_with_parent(
        trace_id: &str,
        parent_id: &str,
        count: usize,
    ) -> Vec<crate::event::SpanEvent> {
        let mut events = Vec::new();
        // Add a root span (the parent)
        let mut root = make_sql_event(trace_id, parent_id, "SELECT 1", "2025-07-10T14:32:01.000Z");
        root.parent_span_id = None;
        events.push(root);

        // Add child spans
        for i in 0..count {
            let mut child = make_sql_event(
                trace_id,
                &format!("child-{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", (i + 1) * 10),
            );
            child.parent_span_id = Some(parent_id.to_string());
            events.push(child);
        }
        events
    }

    #[test]
    fn detects_excessive_fanout() {
        let events = make_events_with_parent("trace-1", "root", 25);
        let trace = make_trace(events);
        let findings = detect_fanout(&trace, 20);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::ExcessiveFanout);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 25);
    }

    #[test]
    fn critical_at_3x_threshold() {
        let events = make_events_with_parent("trace-1", "root", 65);
        let trace = make_trace(events);
        let findings = detect_fanout(&trace, 20);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn no_finding_below_threshold() {
        let events = make_events_with_parent("trace-1", "root", 10);
        let trace = make_trace(events);
        let findings = detect_fanout(&trace, 20);

        assert!(findings.is_empty());
    }

    #[test]
    fn no_finding_at_threshold() {
        let events = make_events_with_parent("trace-1", "root", 20);
        let trace = make_trace(events);
        let findings = detect_fanout(&trace, 20);

        assert!(findings.is_empty());
    }

    #[test]
    fn parent_not_in_trace_uses_child_metadata() {
        // Parent ID references a span that doesn't exist in the trace
        let mut events = Vec::new();
        for i in 0..25 {
            let mut child = make_sql_event(
                "trace-1",
                &format!("child-{i}"),
                &format!("SELECT * FROM t WHERE id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", (i + 1) * 10),
            );
            child.parent_span_id = Some("nonexistent-parent".to_string());
            events.push(child);
        }
        let trace = make_trace(events);
        let findings = detect_fanout(&trace, 20);

        assert_eq!(findings.len(), 1);
        // Service and endpoint should come from the first child span
        assert_eq!(findings[0].service, "order-svc");
    }

    #[test]
    fn no_finding_without_parent_ids() {
        // Events without parent_span_id set
        let events: Vec<_> = (1..=10)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_fanout(&trace, 5);

        assert!(findings.is_empty());
    }
}
