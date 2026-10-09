//! Chatty service detection: identifies traces with excessive inter-service HTTP calls.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect chatty service patterns within a trace.
///
/// A trace with more than `min_calls` HTTP outbound spans is flagged.
/// Severity is `Warning` if > `min_calls`, `Critical` if > 3x `min_calls`.
#[must_use]
pub fn detect_chatty(trace: &Trace, min_calls: u32) -> Vec<Finding> {
    // Partitioned by grouping like every other per-trace detector: counting
    // one deployment's calls into another's finding bills the wrong owner,
    // and the HTML evidence filter would light up the other's spans. Ordered,
    // so findings tied on the entry endpoint keep the grouping order.
    let mut by_grouping: BTreeMap<Option<(&str, &str)>, Vec<usize>> = BTreeMap::new();
    for (i, span) in trace.spans.iter().enumerate() {
        if span.event.event_type == EventType::HttpOut {
            by_grouping
                .entry(span.event.grouping_identity())
                .or_default()
                .push(i);
        }
    }
    let mut findings: Vec<Finding> = by_grouping
        .into_values()
        .filter_map(|indices| chatty_finding(trace, &indices, min_calls))
        .collect();
    findings.sort_by(|a, b| a.pattern.template.cmp(&b.pattern.template));
    findings
}

/// One chatty finding for one grouping's outbound calls.
fn chatty_finding(trace: &Trace, http_indices: &[usize], min_calls: u32) -> Option<Finding> {
    let count = http_indices.len();
    if count <= min_calls as usize {
        return None;
    }

    let severity = if count > (min_calls as usize) * 3 {
        Severity::Critical
    } else {
        Severity::Warning
    };

    // Count occurrences per normalized template for "top N" display
    let mut template_counts: HashMap<&str, usize> =
        HashMap::with_capacity(http_indices.len().min(64));
    for &idx in http_indices {
        *template_counts
            .entry(trace.spans[idx].template.as_ref())
            .or_default() += 1;
    }

    // Top-2 by count: partial partition is O(k) vs a full O(k log k) sort
    // for traces with high endpoint cardinality. Ties break on the template,
    // so the pair does not follow the map's iteration order.
    let by_count_then_template =
        |a: &(&str, usize), b: &(&str, usize)| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0));
    let mut entries: Vec<(&str, usize)> = template_counts.iter().map(|(&k, &v)| (k, v)).collect();
    if entries.len() > 2 {
        entries.select_nth_unstable_by(1, by_count_then_template);
        entries.truncate(2);
    }
    entries.sort_unstable_by(by_count_then_template);
    let mut top_str = String::with_capacity(64);
    for (i, (tmpl, cnt)) in entries.iter().enumerate() {
        if i > 0 {
            top_str.push_str(", ");
        }
        let _ = write!(top_str, "{tmpl} x{cnt}");
    }

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

    Some(Finding {
        finding_type: FindingType::ChattyService,
        severity,
        trace_id: trace.trace_id.clone(),
        service: first.event.service.to_string(),
        grouping: first.event.grouping.clone(),
        source_endpoint: entry_endpoint.clone(),
        pattern: Pattern {
            template: entry_endpoint,
            occurrences: count,
            window_ms,
            distinct_params: distinct_targets,
            ..Default::default()
        },
        suggestion,
        first_timestamp: first_ts.to_string(),
        last_timestamp: last_ts.to_string(),
        green_impact: None,
        confidence: Confidence::default(),
        classification_method: None,
        // Representative call: the first outbound HTTP call.
        code_location: first.event.code_location(),
        instrumentation_scopes: first.event.scope_names(),
        signature: String::new(),
        suggested_fix: None,
    })
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
    fn carries_first_call_scopes_and_code_location() {
        let events: Vec<_> = (1..=20)
            .map(|i| {
                let mut ev = make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc-{}/api/resource/{i}", i % 5),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                );
                if i == 1 {
                    ev.instrumentation_scopes = vec![std::sync::Arc::from(
                        "io.opentelemetry.apache-httpclient-5.0",
                    )];
                    ev.code_namespace = Some(std::sync::Arc::from("com.example.StockClient"));
                }
                ev
            })
            .collect();
        let trace = make_trace(events);
        let mut findings = detect_chatty(&trace, 15);
        crate::test_helpers::assert_java_representative_call(
            &mut findings,
            "io.opentelemetry.apache-httpclient-5.0",
            "com.example.StockClient",
        );
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
        assert_eq!(findings, []);
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
        assert_eq!(findings, []);
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
        assert_eq!(findings, []);
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

    /// One trace crossing two deployments must not bill all its outbound
    /// calls to whichever emitted the first span.
    #[test]
    fn calls_are_counted_per_grouping_not_per_trace() {
        let mut events = Vec::new();
        for (i, ns) in ["commerce", "finance"].into_iter().enumerate() {
            for j in 0..4 {
                let mut event = make_http_event(
                    "t1",
                    &format!("s-{ns}-{j}"),
                    &format!("http://{ns}-svc/api/items/{j}"),
                    &format!("2025-07-10T14:32:0{}.000Z", i * 4 + j),
                );
                event.grouping = crate::test_helpers::grouping("service.namespace", ns);
                events.push(event);
            }
        }
        let trace = make_trace(events);

        let findings = detect_chatty(&trace, 3);

        assert_eq!(findings.len(), 2, "{findings:#?}");
        assert!(
            findings.iter().all(|f| f.pattern.occurrences == 4),
            "neither deployment may absorb the other's calls: {findings:#?}"
        );
    }

    /// Runs per determinism check. Every `HashMap` draws fresh hash keys, so
    /// an order that leaked from one would differ across these runs.
    const RUNS: usize = 32;

    /// The `(top: ...)` part of the suggestion for one trace calling each
    /// `(name, count)` target `count` times, in the given order, asserted
    /// identical over `RUNS` detections.
    fn top_calls(calls: &[(&str, usize)], min_calls: u32) -> String {
        let events: Vec<_> = calls
            .iter()
            .flat_map(|&(name, count)| (0..count).map(move |j| (name, j)))
            .enumerate()
            .map(|(i, (name, j))| {
                make_http_event(
                    "trace-1",
                    &format!("span-{name}-{j}"),
                    &format!("http://{name}-svc/api/{name}"),
                    &format!("2025-07-10T14:32:01.{i:03}Z"),
                )
            })
            .collect();
        let trace = make_trace(events);
        let top = || {
            let findings = detect_chatty(&trace, min_calls);
            assert_eq!(findings.len(), 1, "{findings:#?}");
            let suggestion = &findings[0].suggestion;
            let start = suggestion.find("(top: ").expect("top segment") + "(top: ".len();
            let end = start + suggestion[start..].find(')').expect("closing paren");
            suggestion[start..end].to_string()
        };
        let first = top();
        for _ in 1..RUNS {
            assert_eq!(top(), first);
        }
        first
    }

    #[test]
    fn top_calls_break_ties_on_the_template() {
        let names = [
            "papa", "oscar", "november", "mike", "lima", "kilo", "juliet", "india", "hotel",
            "golf", "foxtrot", "echo", "delta", "charlie", "bravo", "alpha",
        ];
        let calls: Vec<_> = names.iter().map(|&name| (name, 1)).collect();
        assert_eq!(
            top_calls(&calls, 15),
            "GET alpha-svc/api/alpha x1, GET bravo-svc/api/bravo x1"
        );
    }

    #[test]
    fn top_calls_rank_by_count_before_the_template() {
        let calls = [
            ("mike", 2),
            ("kilo", 2),
            ("zulu", 3),
            ("echo", 2),
            ("alpha", 1),
        ];
        assert_eq!(
            top_calls(&calls, 3),
            "GET zulu-svc/api/zulu x3, GET echo-svc/api/echo x2"
        );
    }

    #[test]
    fn two_tied_templates_print_in_template_order() {
        assert_eq!(
            top_calls(&[("bravo", 2), ("alpha", 2)], 3),
            "GET alpha-svc/api/alpha x2, GET bravo-svc/api/bravo x2"
        );
    }

    #[test]
    fn findings_tied_on_the_entry_endpoint_follow_the_grouping() {
        let mut events = Vec::new();
        for (i, ns) in ["finance", "commerce"].into_iter().enumerate() {
            for j in 0..4 {
                let mut event = make_http_event(
                    "t1",
                    &format!("s-{ns}-{j}"),
                    &format!("http://{ns}-svc/api/items/{j}"),
                    &format!("2025-07-10T14:32:0{}.000Z", i * 4 + j),
                );
                event.grouping = crate::test_helpers::grouping("service.namespace", ns);
                events.push(event);
            }
        }
        let trace = make_trace(events);

        for _ in 0..RUNS {
            let findings = detect_chatty(&trace, 3);
            let order: Vec<_> = findings.iter().map(Finding::grouping_identity).collect();
            assert_eq!(
                order,
                [
                    Some(("service.namespace", "commerce")),
                    Some(("service.namespace", "finance")),
                ]
            );
        }
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
