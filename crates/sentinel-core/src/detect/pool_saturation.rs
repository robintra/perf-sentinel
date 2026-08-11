//! Connection pool saturation detection: identifies traces where many SQL spans
//! from the same service overlap in time, suggesting connection pool contention.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;

use super::n_plus_one::parse_timestamp_ms;
use super::{Confidence, Finding, FindingType, Pattern, Severity};

type PoolKey<'a> = (&'a str, Option<(&'a str, &'a str)>);

/// Detect connection pool saturation within a trace.
///
/// Groups SQL spans by service and effective grouping, then computes peak
/// concurrency via a sweep line. If peak concurrent spans >= `threshold`,
/// emits a finding.
#[must_use]
pub fn detect_pool_saturation(trace: &Trace, threshold: u32) -> Vec<Finding> {
    saturated_services(trace, threshold)
        .into_iter()
        .map(|(service, indices, peak)| build_saturation_finding(trace, service, &indices, peak))
        .collect()
}

/// Same detection, also returning the SQL spans active at the first peak.
#[must_use]
pub(crate) fn detect_pool_saturation_with_spans(
    trace: &Trace,
    threshold: u32,
) -> Vec<(Finding, Vec<&str>)> {
    saturated_services(trace, threshold)
        .into_iter()
        .map(|(service, indices, peak)| {
            (
                build_saturation_finding(trace, service, &indices, peak),
                peak_span_ids(trace, &indices),
            )
        })
        .collect()
}

/// Services whose SQL spans reach `threshold` concurrent, with their span
/// indices and the peak. One grouping pass shared by both entry points.
fn saturated_services(trace: &Trace, threshold: u32) -> Vec<(&str, Vec<usize>, u32)> {
    let threshold = threshold as usize;
    group_sql_indices_by_service(trace)
        .into_iter()
        .filter_map(|((service, _grouping), indices)| {
            // Fast path: can't have more concurrent than total.
            if indices.len() < threshold {
                return None;
            }
            let peak = compute_peak_concurrency(trace, &indices);
            ((peak as usize) >= threshold).then_some((service, indices, peak))
        })
        .collect()
}

/// Partition a trace's SQL span indices by service and effective grouping. HTTP
/// and other event types are skipped. Returns borrowed service names
/// (lifetime tied to `trace`) so grouping stays allocation-light.
fn group_sql_indices_by_service(trace: &Trace) -> HashMap<PoolKey<'_>, Vec<usize>> {
    let mut sql_by_service: HashMap<PoolKey<'_>, Vec<usize>> =
        HashMap::with_capacity(trace.spans.len().min(16));
    for (i, span) in trace.spans.iter().enumerate() {
        if span.event.event_type == EventType::Sql {
            sql_by_service
                .entry((span.event.service.as_ref(), span.event.grouping_identity()))
                .or_default()
                .push(i);
        }
    }
    sql_by_service
}

/// Compute the peak concurrent-span count for a subset of `trace.spans`
/// via a sweep-line pass. `indices` is the list of SQL spans belonging
/// to a single service; each one contributes a (start, +1) and (end, -1)
/// event. Sort places ends before starts at the same instant
/// (`false < true`), avoiding overcounting when one span ends as
/// another begins.
///
/// Bounds are kept in microseconds so two sub-millisecond spans starting
/// at the same `start_ms` are correctly identified as concurrent. A pure
/// ms-precision check would truncate `duration_us / 1000 = 0` and let
/// `end_ms == start_ms` slip the `false < true` ordering, undercounting
/// peak concurrency on hot reactive workloads.
fn compute_peak_concurrency(trace: &Trace, indices: &[usize]) -> u32 {
    let mut sweep: Vec<(u64, bool)> = Vec::with_capacity(indices.len() * 2);
    for &idx in indices {
        let span = &trace.spans[idx];
        if let Some(start_ms) = parse_timestamp_ms(&span.event.timestamp) {
            let start_us = start_ms.saturating_mul(1000);
            let end_us = start_us.saturating_add(span.event.duration_us);
            sweep.push((start_us, true)); // span starts
            sweep.push((end_us, false)); // span ends
        }
    }
    sweep.sort_unstable();

    let mut current: u32 = 0;
    let mut peak: u32 = 0;
    for &(_, is_start) in &sweep {
        if is_start {
            current += 1;
        } else {
            current = current.saturating_sub(1);
        }
        if current > peak {
            peak = current;
        }
    }
    peak
}

/// Identity-tracking twin of [`compute_peak_concurrency`]. Both sweep the
/// same `(timestamp, is_start)` ordering, so `peak_span_ids(..).len()`
/// must equal `compute_peak_concurrency(..)`, pinned by
/// `both_sweeps_agree_on_the_peak`. The count-only version stays separate
/// because detection runs it per service per trace and does not need the
/// set.
fn peak_span_ids<'a>(trace: &'a Trace, indices: &[usize]) -> Vec<&'a str> {
    let mut sweep: Vec<(u64, bool, usize)> = Vec::with_capacity(indices.len() * 2);
    for &idx in indices {
        let span = &trace.spans[idx];
        // A zero-duration span occupies no interval. `false < true` sorts its
        // end before its own start, which `compute_peak_concurrency` absorbs
        // as a net zero but which would strand the index in `active` here,
        // inflating every later peak. Skipping keeps the two sweeps equal.
        if span.event.duration_us == 0 {
            continue;
        }
        if let Some(start_ms) = parse_timestamp_ms(&span.event.timestamp) {
            let start_us = start_ms.saturating_mul(1000);
            sweep.push((start_us, true, idx));
            sweep.push((start_us.saturating_add(span.event.duration_us), false, idx));
        }
    }
    sweep.sort_unstable();

    let mut active = HashSet::with_capacity(indices.len());
    let mut peak = Vec::new();
    for (_, is_start, idx) in sweep {
        if is_start {
            active.insert(idx);
            if active.len() > peak.len() {
                peak = active.iter().copied().collect();
                peak.sort_unstable();
            }
        } else {
            active.remove(&idx);
        }
    }
    peak.into_iter()
        .map(|idx| trace.spans[idx].event.span_id.as_str())
        .collect()
}

/// Assemble the `Finding` value for a service that exceeded the pool
/// saturation threshold. Extracted so `detect_pool_saturation` stays
/// a simple loop.
fn build_saturation_finding(trace: &Trace, service: &str, indices: &[usize], peak: u32) -> Finding {
    let total_sql = indices.len();
    let first = &trace.spans[indices[0]];
    let (window_ms, first_ts, last_ts) = super::n_plus_one::compute_window_and_bounds_iter(
        indices
            .iter()
            .map(|&i| trace.spans[i].event.timestamp.as_str()),
    );
    Finding {
        finding_type: FindingType::PoolSaturation,
        severity: Severity::Warning,
        trace_id: trace.trace_id.clone(),
        service: service.to_string(),
        grouping: first.event.grouping.clone(),
        source_endpoint: first.event.source.endpoint.clone(),
        pattern: Pattern {
            template: service.to_string(),
            occurrences: peak as usize, // safe: peak <= indices.len() which is usize
            window_ms,
            distinct_params: total_sql,
            ..Default::default()
        },
        suggestion: format!(
            "Potential connection pool saturation: service {service} has {peak} concurrent \
             SQL spans within {window_ms}ms window. Consider increasing the connection \
             pool size, optimizing long-running queries or using connection pool metrics \
             (db.client.connection.pool.*) for precise monitoring"
        ),
        first_timestamp: first_ts.to_string(),
        last_timestamp: last_ts.to_string(),
        green_impact: None,
        confidence: Confidence::default(),
        classification_method: None,
        signature: String::new(),
        code_location: None,
        instrumentation_scopes: Vec::new(),
        suggested_fix: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_helpers::{
        make_http_event_with_duration, make_sql_event_with_duration, make_trace,
    };

    /// Create overlapping SQL spans: all start at the same time with given duration.
    fn make_concurrent_sql(
        trace_id: &str,
        service: &str,
        count: usize,
        duration_us: u64,
    ) -> Vec<crate::event::SpanEvent> {
        (0..count)
            .map(|i| {
                let mut ev = make_sql_event_with_duration(
                    trace_id,
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t{i} WHERE id = {i}"),
                    "2025-07-10T14:32:01.000Z",
                    duration_us,
                );
                ev.service = Arc::from(service);
                ev
            })
            .collect()
    }

    #[test]
    fn detects_concurrent_sql_spans() {
        let events = make_concurrent_sql("trace-1", "order-svc", 12, 200_000);
        let trace = make_trace(events);
        let findings = detect_pool_saturation(&trace, 10);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::PoolSaturation);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 12); // peak concurrent
        assert_eq!(findings[0].pattern.distinct_params, 12); // total SQL
    }

    #[test]
    fn no_finding_below_threshold() {
        let events = make_concurrent_sql("trace-1", "order-svc", 5, 200_000);
        let trace = make_trace(events);
        let findings = detect_pool_saturation(&trace, 10);
        assert!(findings.is_empty());
    }

    #[test]
    fn sequential_spans_peak_one() {
        // 10 non-overlapping SQL spans: each 100ms, starting 100ms apart
        let events: Vec<_> = (0..10)
            .map(|i| {
                make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 100),
                    100_000, // 100ms
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_pool_saturation(&trace, 2);
        assert!(findings.is_empty(), "sequential spans should have peak=1");
    }

    #[test]
    fn partial_overlap() {
        // Spans: [0-100ms, 50-150ms, 120-220ms, 200-300ms]
        // Peak concurrency = 2 (at 50-100ms: spans 0 and 1 overlap)
        let events = vec![
            make_sql_event_with_duration(
                "trace-1",
                "s0",
                "SELECT 1",
                "2025-07-10T14:32:01.000Z",
                100_000,
            ),
            make_sql_event_with_duration(
                "trace-1",
                "s1",
                "SELECT 2",
                "2025-07-10T14:32:01.050Z",
                100_000,
            ),
            make_sql_event_with_duration(
                "trace-1",
                "s2",
                "SELECT 3",
                "2025-07-10T14:32:01.120Z",
                100_000,
            ),
            make_sql_event_with_duration(
                "trace-1",
                "s3",
                "SELECT 4",
                "2025-07-10T14:32:01.200Z",
                100_000,
            ),
        ];
        let trace = make_trace(events);

        // With threshold 2: should trigger (peak=2)
        let findings = detect_pool_saturation(&trace, 2);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern.occurrences, 2);

        // With threshold 3: should not trigger
        let findings = detect_pool_saturation(&trace, 3);
        assert!(findings.is_empty());
    }

    #[test]
    fn evidence_names_only_the_first_peak_concurrency_set() {
        let events = vec![
            make_sql_event_with_duration(
                "trace-1",
                "s0",
                "SELECT 1",
                "2025-07-10T14:32:01.000Z",
                100_000,
            ),
            make_sql_event_with_duration(
                "trace-1",
                "s1",
                "SELECT 2",
                "2025-07-10T14:32:01.050Z",
                100_000,
            ),
            make_sql_event_with_duration(
                "trace-1",
                "s2",
                "SELECT 3",
                "2025-07-10T14:32:01.120Z",
                100_000,
            ),
        ];
        let trace = make_trace(events);

        let found = detect_pool_saturation_with_spans(&trace, 2);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, vec!["s0", "s1"]);
    }

    #[test]
    fn different_services_counted_separately() {
        let mut events = make_concurrent_sql("trace-1", "svc-a", 12, 200_000);
        let mut svc_b = make_concurrent_sql("trace-1", "svc-b", 12, 200_000);
        // Fix span IDs to avoid collision
        for (i, ev) in svc_b.iter_mut().enumerate() {
            ev.span_id = format!("span-b-{i}");
        }
        events.extend(svc_b);

        let trace = make_trace(events);
        let findings = detect_pool_saturation(&trace, 10);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn equal_grouping_values_from_different_keys_do_not_share_one_pool() {
        let mut events = make_concurrent_sql("trace-1", "svc", 6, 200_000);
        crate::test_helpers::split_grouping_keys_same_value(&mut events);
        let trace = make_trace(events);

        let findings = detect_pool_saturation(&trace, 4);

        assert!(
            findings.is_empty(),
            "each grouping peaks at three connections: {findings:#?}"
        );
    }

    /// Two sweeps over the same events: if the tie-break ordering drifts in
    /// one, the HTML evidence set and the reported peak silently disagree.
    #[test]
    fn both_sweeps_agree_on_the_peak() {
        // Overlapping, nested, and back-to-back spans all in one trace, so
        // the equal-timestamp tie-break is exercised.
        let starts = [
            ("2025-07-10T14:32:01.000Z", 5_000_u64),
            ("2025-07-10T14:32:01.000Z", 1_000),
            ("2025-07-10T14:32:01.001Z", 3_000),
            ("2025-07-10T14:32:01.002Z", 500),
            ("2025-07-10T14:32:01.006Z", 2_000),
            // Zero duration: its end sorts before its own start.
            ("2025-07-10T14:32:01.001Z", 0),
            ("2025-07-10T14:32:01.003Z", 0),
        ];
        let events: Vec<_> = starts
            .iter()
            .enumerate()
            .map(|(i, (ts, dur))| {
                make_sql_event_with_duration("t1", &format!("s{i}"), "SELECT 1", ts, *dur)
            })
            .collect();
        let trace = make_trace(events);
        let indices: Vec<usize> = (0..trace.spans.len()).collect();

        assert_eq!(
            peak_span_ids(&trace, &indices).len(),
            compute_peak_concurrency(&trace, &indices) as usize
        );
    }

    #[test]
    fn http_events_ignored() {
        let events: Vec<_> = (0..15)
            .map(|i| {
                make_http_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://svc/api/{i}"),
                    "2025-07-10T14:32:01.000Z",
                    200_000,
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_pool_saturation(&trace, 10);
        assert!(findings.is_empty());
    }
}
