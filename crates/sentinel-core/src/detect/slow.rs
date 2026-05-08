//! Slow query/call detection.
//!
//! Detects recurring slow operations within a single trace where
//! `duration_us` exceeds a configurable threshold and the same
//! normalized template occurs >= N times.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;

use super::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect recurring slow operations in a single trace.
///
/// Flags operations where `duration_us > threshold_ms * 1000` and
/// the same normalized template occurs >= `min_occurrences` times.
#[must_use]
pub fn detect_slow(trace: &Trace, threshold_ms: u64, min_occurrences: u32) -> Vec<Finding> {
    let threshold_us = threshold_ms.saturating_mul(1000);
    let min_occ = min_occurrences as usize;

    // Group slow spans by (event_type, template)
    let mut groups: HashMap<(&EventType, &str), Vec<usize>> =
        HashMap::with_capacity(trace.spans.len().min(64));
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

        findings.push(super::build_per_trace_finding(super::PerTraceFindingArgs {
            finding_type: FindingType::from_event_type_slow(event_type),
            severity,
            trace_id: &trace.trace_id,
            first_span: first,
            template,
            occurrences: indices.len(),
            window_ms,
            distinct_params: distinct_params.len(),
            suggestion,
            first_timestamp: min_ts,
            last_timestamp: max_ts,
            code_location: first.event.code_location(),
            instrumentation_scopes: first
                .event
                .instrumentation_scopes
                .iter()
                .map(ToString::to_string)
                .collect(),
            classification_method: None,
        }));
    }

    findings
}

/// Nearest-rank percentile index: `ceil(p * n / 100) - 1`, clamped to `[0, n-1]`.
fn percentile_index(n: usize, p: usize) -> usize {
    let rank = (p * n).div_ceil(100);
    rank.saturating_sub(1).min(n - 1)
}

/// Detect slow operations across multiple traces by computing percentiles.
///
/// Groups all spans across all traces by `(event_type, template)`, then computes
/// p50/p95/p99 durations. Emits a finding for each template whose p99 exceeds
/// the threshold. Only emits for templates that span at least 2 distinct traces
/// (per-trace detection handles single-trace cases).
///
/// This function is designed for batch mode where multiple traces are available
/// simultaneously. In daemon/streaming mode, traces are processed individually
/// or in small eviction batches, limiting cross-trace visibility.
#[must_use]
pub fn detect_slow_cross_trace(
    traces: &[Trace],
    threshold_ms: u64,
    min_occurrences: u32,
) -> Vec<Finding> {
    let threshold_us = threshold_ms.saturating_mul(1000);
    let min_occ = min_occurrences as usize;

    // Entries: (duration_us, trace_id, timestamp, &SpanEvent). The
    // span reference carries `service`, `source.endpoint`, code_location
    // and instrumentation_scopes for the worst-trace finding.
    // Pre-filter: only collect spans that exceed the threshold to avoid processing
    // non-slow spans (reduces HashMap size from N to ~1% of N in typical workloads).
    #[allow(clippy::type_complexity)]
    let mut groups: HashMap<
        (&EventType, &str),
        Vec<(u64, &str, &str, &crate::event::SpanEvent)>,
    > = HashMap::with_capacity(traces.len().min(256));
    for trace in traces {
        for span in &trace.spans {
            if span.event.duration_us <= threshold_us {
                continue;
            }
            groups
                .entry((&span.event.event_type, &span.template))
                .or_default()
                .push((
                    span.event.duration_us,
                    trace.trace_id.as_str(),
                    span.event.timestamp.as_str(),
                    &span.event,
                ));
        }
    }

    let mut findings = Vec::new();
    for ((event_type, template), mut entries) in groups {
        if let Some(finding) =
            build_cross_trace_finding(event_type, template, &mut entries, min_occ, threshold_us)
        {
            findings.push(finding);
        }
    }

    findings
}

/// Build a cross-trace slow finding from a group of entries for the same template.
/// Returns `None` if the group doesn't meet the criteria (too few occurrences,
/// single trace, or p99 below threshold).
fn build_cross_trace_finding(
    event_type: &EventType,
    template: &str,
    entries: &mut [(u64, &str, &str, &crate::event::SpanEvent)],
    min_occ: usize,
    threshold_us: u64,
) -> Option<Finding> {
    if entries.len() < min_occ {
        return None;
    }

    let distinct_traces: HashSet<&str> = entries.iter().map(|&(_, tid, _, _)| tid).collect();
    if distinct_traces.len() < 2 {
        return None;
    }

    entries.sort_by_key(|&(dur, _, _, _)| dur);
    let n = entries.len();
    let p50 = entries[percentile_index(n, 50)].0;
    let p95 = entries[percentile_index(n, 95)].0;
    let p99 = entries[percentile_index(n, 99)].0;

    if p99 <= threshold_us {
        return None;
    }

    let max_dur = entries[n - 1].0;
    let (_, worst_trace_id, _, worst_event) = entries[n - 1];
    let (window_ms, first_ts, last_ts) =
        super::n_plus_one::compute_window_and_bounds_iter(entries.iter().map(|e| e.2));

    let severity = if max_dur > threshold_us.saturating_mul(5) {
        Severity::Critical
    } else {
        Severity::Warning
    };

    let label = match event_type {
        EventType::Sql => "adding an index or optimizing query",
        EventType::HttpOut => "caching or optimizing endpoint",
    };
    let suggestion = format!(
        "Cross-trace analysis: p50={:.1}ms, p95={:.1}ms, p99={:.1}ms across {n} occurrences. Consider {label}",
        p50 as f64 / 1000.0,
        p95 as f64 / 1000.0,
        p99 as f64 / 1000.0,
    );

    Some(Finding {
        finding_type: FindingType::from_event_type_slow(event_type),
        severity,
        trace_id: worst_trace_id.to_string(),
        service: worst_event.service.to_string(),
        source_endpoint: worst_event.source.endpoint.clone(),
        pattern: Pattern {
            template: template.to_string(),
            occurrences: n,
            window_ms,
            distinct_params: 0,
        },
        suggestion,
        first_timestamp: first_ts.to_string(),
        last_timestamp: last_ts.to_string(),
        green_impact: None,
        confidence: Confidence::default(),
        classification_method: None,
        code_location: worst_event.code_location(),
        instrumentation_scopes: worst_event
            .instrumentation_scopes
            .iter()
            .map(ToString::to_string)
            .collect(),
        suggested_fix: None,
        signature: String::new(),
    })
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

    #[test]
    fn cross_trace_detects_slow_across_traces() {
        // 3 traces, each with 1 slow query of the same template
        // Per-trace: only 1 occurrence each (below min_occurrences=3)
        // Cross-trace: 3 occurrences total (meets threshold)
        let traces: Vec<_> = (1..=3)
            .map(|i| {
                let events = vec![make_sql_event_with_duration(
                    &format!("trace-{i}"),
                    &format!("span-{i}"),
                    &format!("SELECT * FROM big_table WHERE id = {i}"),
                    &format!("2025-07-10T14:32:0{i}.000Z"),
                    600_000, // 600ms, above 500ms threshold
                )];
                make_trace(events)
            })
            .collect();

        let findings = detect_slow_cross_trace(&traces, 500, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::SlowSql);
        assert_eq!(findings[0].pattern.occurrences, 3);
        assert!(findings[0].suggestion.contains("Cross-trace"));
        assert!(findings[0].suggestion.contains("p50="));
    }

    #[test]
    fn cross_trace_below_threshold_no_finding() {
        let traces: Vec<_> = (1..=3)
            .map(|i| {
                let events = vec![make_sql_event_with_duration(
                    &format!("trace-{i}"),
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:0{i}.000Z"),
                    300_000, // 300ms, below 500ms threshold
                )];
                make_trace(events)
            })
            .collect();

        let findings = detect_slow_cross_trace(&traces, 500, 3);
        assert!(findings.is_empty());
    }

    #[test]
    fn cross_trace_critical_severity_5x() {
        let traces: Vec<_> = (1..=3)
            .map(|i| {
                let dur = if i == 3 { 3_000_000 } else { 600_000 }; // 3rd is 3000ms > 5x500ms
                let events = vec![make_sql_event_with_duration(
                    &format!("trace-{i}"),
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:0{i}.000Z"),
                    dur,
                )];
                make_trace(events)
            })
            .collect();

        let findings = detect_slow_cross_trace(&traces, 500, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn percentile_index_small_n() {
        // For n=3, p99 should return the max element (index 2), not the median
        assert_eq!(percentile_index(3, 99), 2);
        assert_eq!(percentile_index(3, 95), 2);
        assert_eq!(percentile_index(3, 50), 1);
        // Edge cases
        assert_eq!(percentile_index(1, 99), 0);
        assert_eq!(percentile_index(2, 99), 1);
        assert_eq!(percentile_index(100, 99), 98);
        assert_eq!(percentile_index(100, 50), 49);
    }

    #[test]
    fn cross_trace_p99_is_max_for_small_n() {
        // 3 traces, 1 slow span each: durations 600ms, 800ms, 1000ms
        // p99 with n=3 should be 1000ms (the max), not 800ms (the median)
        let traces: Vec<_> = vec![(1, 600_000u64), (2, 800_000), (3, 1_000_000)]
            .into_iter()
            .map(|(i, dur)| {
                let events = vec![make_sql_event_with_duration(
                    &format!("trace-{i}"),
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:0{i}.000Z"),
                    dur,
                )];
                make_trace(events)
            })
            .collect();

        let findings = detect_slow_cross_trace(&traces, 500, 3);
        assert_eq!(findings.len(), 1);
        // The suggestion should include p99=1000.0ms (the max), not 800.0ms
        assert!(
            findings[0].suggestion.contains("p99=1000.0ms"),
            "p99 should be max for n=3, got: {}",
            findings[0].suggestion
        );
    }

    #[test]
    fn cross_trace_http_slow_uses_http_label() {
        let traces: Vec<Trace> = (1..=3)
            .map(|t| {
                let events = vec![make_http_event_with_duration(
                    &format!("trace-{t}"),
                    "span-1",
                    "http://user-svc:5000/api/users/1",
                    "2025-07-10T14:32:01.000Z",
                    600_000,
                )];
                make_trace(events)
            })
            .collect();

        let findings = detect_slow_cross_trace(&traces, 500, 3);
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0]
                .suggestion
                .contains("caching or optimizing endpoint"),
            "HTTP slow should suggest caching, got: {}",
            findings[0].suggestion
        );
    }

    #[test]
    fn cross_trace_skipped_when_p99_below_threshold() {
        // All spans just above threshold individually but we set threshold very high
        let traces: Vec<Trace> = (1..=3)
            .map(|t| {
                let events = vec![make_sql_event_with_duration(
                    &format!("trace-{t}"),
                    "span-1",
                    "SELECT * FROM t WHERE id = 1",
                    "2025-07-10T14:32:01.000Z",
                    100_000, // 100ms, well below 500ms threshold
                )];
                make_trace(events)
            })
            .collect();

        let findings = detect_slow_cross_trace(&traces, 500, 3);
        assert!(
            findings.is_empty(),
            "should produce no findings when p99 < threshold"
        );
    }

    // -- Boundary condition tests --

    #[test]
    fn exactly_at_threshold_not_slow() {
        // duration_us == threshold_ms * 1000 exactly → NOT slow (uses strict >)
        let events: Vec<_> = (0..3)
            .map(|i| {
                make_sql_event_with_duration(
                    "t1",
                    &format!("s{i}"),
                    "SELECT * FROM t WHERE id = 1",
                    "2025-07-10T14:32:01.000Z",
                    500_000, // exactly 500ms threshold
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);
        assert!(
            findings.is_empty(),
            "exactly at threshold should not be flagged"
        );
    }

    #[test]
    fn one_microsecond_above_threshold_is_slow() {
        let events: Vec<_> = (0..3)
            .map(|i| {
                make_sql_event_with_duration(
                    "t1",
                    &format!("s{i}"),
                    "SELECT * FROM t WHERE id = 1",
                    "2025-07-10T14:32:01.000Z",
                    500_001, // 1µs above threshold
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn exactly_5x_threshold_is_warning_not_critical() {
        // 5x threshold exactly → still warning (uses strict >)
        let events: Vec<_> = (0..3)
            .map(|i| {
                make_sql_event_with_duration(
                    "t1",
                    &format!("s{i}"),
                    "SELECT * FROM t WHERE id = 1",
                    "2025-07-10T14:32:01.000Z",
                    2_500_000, // exactly 5x 500ms
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn above_5x_threshold_is_critical() {
        let events: Vec<_> = (0..3)
            .map(|i| {
                make_sql_event_with_duration(
                    "t1",
                    &format!("s{i}"),
                    "SELECT * FROM t WHERE id = 1",
                    "2025-07-10T14:32:01.000Z",
                    2_500_001, // 1µs above 5x
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn min_occurrences_one() {
        let events = vec![make_sql_event_with_duration(
            "t1",
            "s1",
            "SELECT * FROM t WHERE id = 1",
            "2025-07-10T14:32:01.000Z",
            600_000,
        )];
        let trace = make_trace(events);
        let findings = detect_slow(&trace, 500, 1);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn empty_trace_no_slow_findings() {
        let trace = Trace {
            trace_id: "empty".to_string(),
            spans: vec![],
        };
        let findings = detect_slow(&trace, 500, 3);
        assert!(findings.is_empty());
    }
}
