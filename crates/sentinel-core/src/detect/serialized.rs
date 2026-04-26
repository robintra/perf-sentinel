//! Serialized-but-parallelizable calls detection: identifies sequential
//! independent sibling spans that could be executed in parallel.
//!
//! Uses dynamic programming (Weighted Interval Scheduling with unit weights)
//! to find the longest non-overlapping subsequence of sibling spans,
//! guaranteeing an optimal result in O(n log n).

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;

use super::n_plus_one::parse_timestamp_ms;
use super::{Confidence, Finding, FindingType, Pattern, Severity, TraceIndices};

/// A sibling span with parsed timing, used by the DP algorithm.
struct TimedSpan<'a> {
    start_ms: u64,
    end_ms: u64,
    template: &'a str,
    duration_us: u64,
    /// Index into `trace.spans`.
    span_idx: usize,
}

/// Detect serialized-but-parallelizable call sequences within a trace.
///
/// Finds sibling spans (same `parent_span_id`) that execute sequentially
/// (no time overlap) with different normalized templates. If a sequence of
/// at least `min_sequential` such calls is found, emits a finding.
///
/// Uses dynamic programming (a variant of Weighted Interval Scheduling
/// with unit weights) to find the **longest** non-overlapping subsequence,
/// guaranteeing an optimal result. The algorithm sorts spans by end time,
/// then for each span binary-searches for its latest compatible predecessor.
/// The recurrence `dp[i] = max(dp[i-1], dp[p(i)] + 1)` yields the optimal
/// count, and a backtrack pass reconstructs the selected spans.
///
/// Sequences where all calls share the same template are skipped (N+1 territory).
#[must_use]
pub fn detect_serialized(
    trace: &Trace,
    indices: &TraceIndices<'_>,
    min_sequential: u32,
) -> Vec<Finding> {
    let min_seq = min_sequential as usize;

    let siblings = &indices.children_by_parent;
    let span_index = &indices.span_index;

    let mut findings = Vec::new();

    for (parent_id, child_indices) in siblings {
        if child_indices.len() < min_seq {
            continue;
        }

        // Build timed entries from parsed timestamps
        let mut timed: Vec<TimedSpan<'_>> = Vec::with_capacity(child_indices.len());
        for &idx in child_indices {
            let span = &trace.spans[idx];
            if let Some(start_ms) = parse_timestamp_ms(&span.event.timestamp) {
                let dur_ms = span.event.duration_us / 1000;
                timed.push(TimedSpan {
                    start_ms,
                    end_ms: start_ms.saturating_add(dur_ms),
                    template: span.template.as_str(),
                    duration_us: span.event.duration_us,
                    span_idx: idx,
                });
            }
        }

        if timed.len() < min_seq {
            continue;
        }

        // Sort by end time for the DP approach.
        timed.sort_by_key(|s| s.end_ms);

        // Find the longest non-overlapping subsequence via DP, then
        // pass it to evaluate_sequence for threshold / template checks.
        let best_seq = longest_non_overlapping(&timed);

        evaluate_sequence(
            &timed,
            &best_seq,
            min_seq,
            trace,
            span_index,
            parent_id,
            &mut findings,
        );
    }

    findings
}

/// Find the longest non-overlapping subsequence via dynamic programming.
///
/// This is a variant of **Weighted Interval Scheduling** with unit weights
/// (we maximize the *count* of selected intervals, not a weighted sum).
///
/// ## Precondition
///
/// `timed` must be sorted by end time (ascending).
///
/// ## Algorithm
///
/// 1. **Predecessor computation.** For each span `i`, binary-search the
///    sorted `timed` slice to find `p(i)`: the index of the rightmost span
///    whose end time is <= span `i`'s start time. This is the latest
///    span that does not overlap with `i`. O(log n) per span.
///
/// 2. **DP recurrence.** `dp[i]` = length of the longest non-overlapping
///    subsequence considering only spans `0..=i`.
///    - If we **exclude** span `i`: `dp[i] = dp[i-1]`
///    - If we **include** span `i`: `dp[i] = dp[p(i)] + 1`
///      (where `dp[-1] = 0` by convention, handled via the `Option`)
///    - Take the max of both choices. Track the choice in `included[i]`.
///
/// 3. **Backtrack.** Walk backwards from `i = n-1`. If `included[i]` is
///    true, add `i` to the result and jump to `p(i)`. Otherwise, move to
///    `i-1`. Reverse the result to get chronological order.
///
/// ## Complexity
///
/// - Sort: O(n log n) (done by caller)
/// - Predecessor search: O(n log n) total (n binary searches)
/// - DP fill: O(n)
/// - Backtrack: O(n)
/// - **Total: O(n log n)**
fn longest_non_overlapping(timed: &[TimedSpan<'_>]) -> Vec<usize> {
    let n = timed.len();
    if n == 0 {
        return vec![];
    }
    let pred = compute_predecessors(timed);
    let (dp, included) = fill_dp_table(&pred, n);
    backtrack_selection(&included, &pred, dp[n - 1], n)
}

/// Build the predecessor array for Weighted Interval Scheduling DP.
///
/// `p[i]` is the index of the rightmost span `j` (`j < i`) whose end
/// is `<= timed[i].start`, or `None` if no such span exists. The
/// `j < i` constraint is critical: without it, a span could be its
/// own predecessor (e.g., zero-duration spans with identical
/// timestamps), causing an infinite backtrack loop.
///
/// Binary search runs directly on the sorted `timed` slice, so this
/// is O(n log n) total (n binary searches).
fn compute_predecessors(timed: &[TimedSpan<'_>]) -> Vec<Option<usize>> {
    (0..timed.len())
        .map(|i| {
            let start = timed[i].start_ms;
            // partition_point returns the first index where end > start,
            // so the predecessor candidate is at partition_point - 1.
            let pos = timed.partition_point(|s| s.end_ms <= start);
            if pos == 0 {
                return None;
            }
            let p = pos - 1;
            // Ensure predecessor is strictly before current span.
            if p < i { Some(p) } else { None }
        })
        .collect()
}

/// Fill the O(n) Weighted Interval Scheduling DP table.
///
/// `dp[i]` = length of the longest non-overlapping subsequence in
/// `timed[0..=i]`. `included[i]` = whether span `i` is part of the
/// optimal solution at position `i`. Precondition: `n >= 1`.
fn fill_dp_table(pred: &[Option<usize>], n: usize) -> (Vec<usize>, Vec<bool>) {
    let mut dp = vec![0usize; n];
    let mut included = vec![false; n];

    // Base case: span 0 alone forms a subsequence of length 1.
    dp[0] = 1;
    included[0] = true;

    for i in 1..n {
        // Option A: skip span i, keep previous best.
        let without = dp[i - 1];
        // Option B: include span i. Best compatible prefix is dp[p(i)],
        // or 0 if no predecessor exists.
        let with = match pred[i] {
            Some(p) => dp[p] + 1,
            None => 1,
        };
        if with >= without {
            dp[i] = with;
            included[i] = true;
        } else {
            dp[i] = without;
            // included[i] remains false
        }
    }
    (dp, included)
}

/// Walk the `included` + `pred` arrays backwards from the last span
/// to reconstruct the chosen subsequence, then reverse it to get
/// chronological order.
///
/// Safety: the predecessor index `p` must always be strictly less
/// than the current index `i` (sorted by end time, `p(i)` is the
/// rightmost span ending before `i` starts). The explicit guard
/// `p < i` in the match arm guarantees termination even on
/// degenerate input.
fn backtrack_selection(
    included: &[bool],
    pred: &[Option<usize>],
    optimal_len: usize,
    n: usize,
) -> Vec<usize> {
    let mut selected = Vec::with_capacity(optimal_len);
    let mut i = n - 1;
    loop {
        if included[i] {
            selected.push(i);
            match pred[i] {
                Some(p) if p < i => i = p,
                _ => break, // no predecessor, or degenerate case: stop
            }
        } else if i == 0 {
            break;
        } else {
            i -= 1;
        }
    }
    selected.reverse(); // chronological order
    selected
}

/// Evaluate a candidate sequence for emission as a finding.
fn evaluate_sequence(
    timed: &[TimedSpan<'_>],
    seq: &[usize],
    min_seq: usize,
    trace: &Trace,
    span_index: &HashMap<&str, usize>,
    parent_id: &str,
    findings: &mut Vec<Finding>,
) {
    if seq.len() < min_seq {
        return;
    }

    let distinct: HashSet<&str> = seq.iter().map(|&i| timed[i].template).collect();

    // False positive guard: skip if ALL calls share the same template (N+1 territory)
    if distinct.len() <= 1 {
        return;
    }

    // Single pass: accumulate total duration and track max duration.
    let (total_sequential_us, max_duration_us) =
        seq.iter().fold((0u64, 0u64), |(total, max), &i| {
            let d = timed[i].duration_us;
            (total + d, max.max(d))
        });

    let total_ms = total_sequential_us / 1000;
    let parallel_ms = max_duration_us / 1000;

    // Build descriptive call chain
    let calls_str: String = seq
        .iter()
        .map(|&i| {
            let s = &timed[i];
            let dur_ms = s.duration_us / 1000;
            format!("{} ({dur_ms}ms)", s.template)
        })
        .collect::<Vec<_>>()
        .join(" -> ");

    // Parent endpoint and service
    let parent_span = span_index.get(parent_id).map(|&i| &trace.spans[i]);
    let first_child = &trace.spans[timed[seq[0]].span_idx];

    let parent_endpoint = parent_span.map_or_else(
        || first_child.event.source.endpoint.clone(),
        |s| s.event.source.endpoint.clone(),
    );
    let service = parent_span.map_or_else(
        || first_child.event.service.clone(),
        |s| s.event.service.clone(),
    );

    let count = seq.len();

    let (window_ms, first_ts, last_ts) = super::n_plus_one::compute_window_and_bounds_iter(
        seq.iter()
            .map(|&i| trace.spans[timed[i].span_idx].event.timestamp.as_str()),
    );

    let template = parent_endpoint.clone();
    findings.push(Finding {
        finding_type: FindingType::SerializedCalls,
        severity: Severity::Info,
        trace_id: trace.trace_id.clone(),
        service,
        source_endpoint: parent_endpoint,
        pattern: Pattern {
            template,
            occurrences: count,
            window_ms,
            distinct_params: distinct.len(),
        },
        suggestion: format!(
            "{count} sequential independent calls could potentially be parallelized: \
             {calls_str}. Total sequential: {total_ms}ms, potential parallel: ~{parallel_ms}ms. \
             If these calls are independent, consider executing them in parallel \
             (e.g., tokio::join!, CompletableFuture.allOf(), Task.WhenAll())"
        ),
        first_timestamp: first_ts.to_string(),
        last_timestamp: last_ts.to_string(),
        green_impact: None,
        confidence: Confidence::default(),
        classification_method: None,
        code_location: None,
        instrumentation_scopes: Vec::new(),
        suggested_fix: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{
        make_http_event_with_duration, make_sql_event_with_duration, make_trace,
    };

    // Structurally different URLs so they normalize to distinct templates.
    const DISTINCT_URLS: &[&str] = &[
        "http://user-svc/api/users/42",
        "http://inventory-svc/api/inventory/check",
        "http://pricing-svc/api/pricing/quote",
        "http://notif-svc/api/notifications/send",
        "http://shipping-svc/api/shipping/estimate",
        "http://billing-svc/api/billing/charge",
    ];

    /// Create a parent span + N sequential children with different services/templates.
    fn make_sequential_children(
        trace_id: &str,
        parent_id: &str,
        count: usize,
    ) -> Vec<crate::event::SpanEvent> {
        let mut events = Vec::new();

        // Parent span
        let mut root = make_http_event_with_duration(
            trace_id,
            parent_id,
            "http://gateway/api/orders/42/submit",
            "2025-07-10T14:32:01.000Z",
            1_000_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // Sequential children: each 100ms, starting after the previous ends
        for i in 0..count {
            let start_ms = 100 + i * 120;
            let mut child = make_http_event_with_duration(
                trace_id,
                &format!("child-{i}"),
                DISTINCT_URLS[i % DISTINCT_URLS.len()],
                &format!("2025-07-10T14:32:01.{start_ms:03}Z"),
                100_000, // 100ms each
            );
            child.parent_span_id = Some(parent_id.to_string());
            child.service = format!("svc-{i}");
            events.push(child);
        }
        events
    }

    #[test]
    fn detects_sequential_siblings() {
        let events = make_sequential_children("trace-1", "root", 4);
        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::SerializedCalls);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].pattern.occurrences, 4);
    }

    #[test]
    fn no_finding_below_threshold() {
        let events = make_sequential_children("trace-1", "root", 2);
        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert!(findings.is_empty());
    }

    #[test]
    fn overlapping_siblings_no_finding() {
        let mut events = Vec::new();

        // Parent
        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:01.000Z",
            500_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // 4 children all starting at the same time (overlapping)
        for (i, url) in DISTINCT_URLS.iter().enumerate().take(4) {
            let mut child = make_http_event_with_duration(
                "trace-1",
                &format!("child-{i}"),
                url,
                "2025-07-10T14:32:01.100Z",
                100_000,
            );
            child.parent_span_id = Some("root".to_string());
            child.service = format!("svc-{i}");
            events.push(child);
        }

        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert!(findings.is_empty(), "overlapping spans should not trigger");
    }

    #[test]
    fn same_template_skipped() {
        let mut events = Vec::new();

        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:01.000Z",
            500_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // 5 sequential children, ALL with the same target template
        for i in 0..5 {
            let start_ms = 100 + i * 120;
            let mut child = make_http_event_with_duration(
                "trace-1",
                &format!("child-{i}"),
                &format!("http://svc/api/users/{}", i + 1),
                &format!("2025-07-10T14:32:01.{start_ms:03}Z"),
                100_000,
            );
            child.parent_span_id = Some("root".to_string());
            events.push(child);
        }

        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert!(
            findings.is_empty(),
            "same template = N+1 territory, should be skipped"
        );
    }

    #[test]
    fn mixed_overlap_partial_sequence() {
        let mut events = Vec::new();

        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:01.000Z",
            1_000_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // First 3 children: sequential (different templates)
        for (i, url) in DISTINCT_URLS.iter().enumerate().take(3) {
            let start_ms = 100 + i * 120;
            let mut child = make_http_event_with_duration(
                "trace-1",
                &format!("child-{i}"),
                url,
                &format!("2025-07-10T14:32:01.{start_ms:03}Z"),
                100_000,
            );
            child.parent_span_id = Some("root".to_string());
            child.service = format!("svc-{i}");
            events.push(child);
        }

        // Next 3 children: overlapping with each other AND with last sequential span.
        // child-2 ends at 340+100=440ms. These start at 400ms to overlap with child-2.
        for (i, url) in DISTINCT_URLS.iter().enumerate().take(6).skip(3) {
            let mut child = make_http_event_with_duration(
                "trace-1",
                &format!("child-{i}"),
                url,
                "2025-07-10T14:32:01.400Z",
                100_000,
            );
            child.parent_span_id = Some("root".to_string());
            child.service = format!("svc-{i}");
            events.push(child);
        }

        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert_eq!(findings.len(), 1);
        // First 3 are sequential (100-200, 220-320, 340-440), then overlap breaks at 400
        assert_eq!(findings[0].pattern.occurrences, 3);
    }

    #[test]
    fn no_parent_span_id_no_finding() {
        // Spans without parent_span_id
        let events: Vec<_> = (0..5)
            .map(|i| {
                make_http_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc-{i}/api/{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 120),
                    100_000,
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert!(findings.is_empty());
    }

    #[test]
    fn potential_time_savings_in_suggestion() {
        // 3 sequential children: 120ms, 95ms, 80ms
        let mut events = Vec::new();

        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:01.000Z",
            500_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // child-0: starts at 100ms, duration 120ms (ends at 220ms)
        let mut c0 = make_http_event_with_duration(
            "trace-1",
            "child-0",
            "http://user-svc/api/users/42",
            "2025-07-10T14:32:01.100Z",
            120_000,
        );
        c0.parent_span_id = Some("root".to_string());
        c0.service = "user-svc".to_string();
        events.push(c0);

        // child-1: starts at 220ms, duration 95ms (ends at 315ms)
        let mut c1 = make_http_event_with_duration(
            "trace-1",
            "child-1",
            "http://inventory-svc/api/inventory/check",
            "2025-07-10T14:32:01.220Z",
            95_000,
        );
        c1.parent_span_id = Some("root".to_string());
        c1.service = "inventory-svc".to_string();
        events.push(c1);

        // child-2: starts at 315ms, duration 80ms (ends at 395ms)
        let mut c2 = make_http_event_with_duration(
            "trace-1",
            "child-2",
            "http://pricing-svc/api/pricing/quote",
            "2025-07-10T14:32:01.315Z",
            80_000,
        );
        c2.parent_span_id = Some("root".to_string());
        c2.service = "pricing-svc".to_string();
        events.push(c2);

        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert_eq!(findings.len(), 1);
        // Total: 120 + 95 + 80 = 295ms, parallel: ~120ms
        assert!(findings[0].suggestion.contains("Total sequential: 295ms"));
        assert!(
            findings[0]
                .suggestion
                .contains("potential parallel: ~120ms")
        );
    }

    #[test]
    fn sql_and_http_mixed_siblings() {
        let mut events = Vec::new();

        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:01.000Z",
            500_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // Mix of SQL and HTTP children, sequential
        let mut c0 = make_sql_event_with_duration(
            "trace-1",
            "child-0",
            "SELECT * FROM users WHERE id = 42",
            "2025-07-10T14:32:01.100Z",
            100_000,
        );
        c0.parent_span_id = Some("root".to_string());
        events.push(c0);

        let mut c1 = make_http_event_with_duration(
            "trace-1",
            "child-1",
            "http://inventory-svc/api/check",
            "2025-07-10T14:32:01.220Z",
            100_000,
        );
        c1.parent_span_id = Some("root".to_string());
        c1.service = "inventory-svc".to_string();
        events.push(c1);

        let mut c2 = make_sql_event_with_duration(
            "trace-1",
            "child-2",
            "INSERT INTO audit_log VALUES (1, 'order_created')",
            "2025-07-10T14:32:01.340Z",
            100_000,
        );
        c2.parent_span_id = Some("root".to_string());
        events.push(c2);

        let trace = make_trace(events);
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert_eq!(
            findings.len(),
            1,
            "mixed SQL/HTTP siblings should be detected"
        );
    }

    #[test]
    fn dp_finds_longer_sequence_than_greedy_would() {
        // This test demonstrates the DP advantage over a greedy approach.
        //
        // Timeline:
        //   A: [0-200ms]  (long span, blocks B and C if chosen greedily)
        //   B: [100-150ms] (short, overlaps with A but not C)
        //   C: [160-300ms] (starts after B ends)
        //   D: [310-400ms] (starts after C ends)
        //
        // A greedy scan sorted by start time would pick A first, skip B and C
        // (overlap with A), then pick D. Result: {A, D} = 2 spans.
        //
        // The DP algorithm finds the optimal {B, C, D} = 3 spans, which is
        // strictly longer. With threshold=3, only the DP triggers a finding.
        let mut events = Vec::new();

        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:00.000Z",
            2_000_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // A: [0-200ms], long span
        let mut a = make_http_event_with_duration(
            "trace-1",
            "child-a",
            "http://auth-svc/api/auth/validate",
            "2025-07-10T14:32:01.000Z",
            200_000,
        );
        a.parent_span_id = Some("root".to_string());
        a.service = "auth-svc".to_string();
        events.push(a);

        // B: [100-150ms], short, overlaps with A
        let mut b = make_http_event_with_duration(
            "trace-1",
            "child-b",
            "http://user-svc/api/users/42",
            "2025-07-10T14:32:01.100Z",
            50_000,
        );
        b.parent_span_id = Some("root".to_string());
        b.service = "user-svc".to_string();
        events.push(b);

        // C: [160-300ms], starts after B ends, overlaps with A
        let mut c = make_http_event_with_duration(
            "trace-1",
            "child-c",
            "http://inventory-svc/api/inventory/check",
            "2025-07-10T14:32:01.160Z",
            140_000,
        );
        c.parent_span_id = Some("root".to_string());
        c.service = "inventory-svc".to_string();
        events.push(c);

        // D: [310-400ms], starts after both A and C end
        let mut d = make_http_event_with_duration(
            "trace-1",
            "child-d",
            "http://pricing-svc/api/pricing/quote",
            "2025-07-10T14:32:01.310Z",
            90_000,
        );
        d.parent_span_id = Some("root".to_string());
        d.service = "pricing-svc".to_string();
        events.push(d);

        let trace = make_trace(events);

        // With threshold=3, a greedy approach would find only 2 spans ({A,D})
        // and would NOT emit a finding. The DP finds {B,C,D} = 3, which triggers.
        let findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
        assert_eq!(
            findings.len(),
            1,
            "DP should find the optimal B,C,D sequence of length 3"
        );
        assert_eq!(findings[0].pattern.occurrences, 3);
    }

    #[test]
    fn identical_timestamps_does_not_hang() {
        // Regression test: spans with identical timestamps could cause the
        // predecessor to point to itself (pred[i] == i), creating an infinite
        // backtrack loop that consumes all memory. This test verifies
        // termination with degenerate input.
        let mut events = Vec::new();

        let mut root = make_http_event_with_duration(
            "trace-1",
            "root",
            "http://gateway/api/orders",
            "2025-07-10T14:32:01.000Z",
            1_000_000,
        );
        root.parent_span_id = None;
        events.push(root);

        // 5 children all with the SAME timestamp and 0ms duration
        for (i, url) in DISTINCT_URLS.iter().enumerate().take(5) {
            let mut child = make_http_event_with_duration(
                "trace-1",
                &format!("child-{i}"),
                url,
                "2025-07-10T14:32:01.100Z",
                0, // zero duration
            );
            child.parent_span_id = Some("root".to_string());
            child.service = format!("svc-{i}");
            events.push(child);
        }

        let trace = make_trace(events);
        // Must terminate. The result doesn't matter, only that it doesn't hang.
        let _findings = detect_serialized(&trace, &TraceIndices::build(&trace), 3);
    }
}
