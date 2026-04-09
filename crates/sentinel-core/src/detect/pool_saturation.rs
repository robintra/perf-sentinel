//! Connection pool saturation detection: identifies traces where many SQL spans
//! from the same service overlap in time, suggesting connection pool contention.

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::event::EventType;

use super::n_plus_one::parse_timestamp_ms;
use super::{Confidence, Finding, FindingType, Pattern, Severity};

/// Detect connection pool saturation within a trace.
///
/// Groups SQL spans by service, computes peak concurrency via a sweep line
/// algorithm. If peak concurrent spans >= `threshold`, emits a finding.
#[must_use]
pub fn detect_pool_saturation(trace: &Trace, threshold: u32) -> Vec<Finding> {
    let threshold = threshold as usize;

    // Group SQL span indices by service
    let mut sql_by_service: HashMap<&str, Vec<usize>> =
        HashMap::with_capacity(trace.spans.len().min(16));
    for (i, span) in trace.spans.iter().enumerate() {
        if span.event.event_type == EventType::Sql {
            sql_by_service
                .entry(span.event.service.as_str())
                .or_default()
                .push(i);
        }
    }

    let mut findings = Vec::new();

    for (service, indices) in &sql_by_service {
        // Fast path: can't have more concurrent than total
        if indices.len() < threshold {
            continue;
        }

        // Build sweep-line events: (time_ms, is_start).
        // Sort places ends before starts at the same instant (false < true),
        // avoiding overcounting when one span ends as another begins.
        let mut sweep: Vec<(u64, bool)> = Vec::with_capacity(indices.len() * 2);
        for &idx in indices {
            let span = &trace.spans[idx];
            if let Some(start_ms) = parse_timestamp_ms(&span.event.timestamp) {
                let end_ms = start_ms.saturating_add(span.event.duration_us / 1000);
                sweep.push((start_ms, true)); // span starts
                sweep.push((end_ms, false)); // span ends
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

        if (peak as usize) < threshold {
            continue;
        }

        let total_sql = indices.len();
        let first = &trace.spans[indices[0]];

        let (window_ms, first_ts, last_ts) = super::n_plus_one::compute_window_and_bounds_iter(
            indices
                .iter()
                .map(|&i| trace.spans[i].event.timestamp.as_str()),
        );

        findings.push(Finding {
            finding_type: FindingType::PoolSaturation,
            severity: Severity::Warning,
            trace_id: trace.trace_id.clone(),
            service: service.to_string(),
            source_endpoint: first.event.source.endpoint.clone(),
            pattern: Pattern {
                template: service.to_string(),
                occurrences: peak as usize, // safe: peak <= indices.len() which is usize
                window_ms,
                distinct_params: total_sql,
            },
            suggestion: format!(
                "Potential connection pool saturation: service {service} has {peak} concurrent \
                 SQL spans within {window_ms}ms window. Consider increasing the connection \
                 pool size, optimizing long-running queries, or using connection pool metrics \
                 (db.client.connection.pool.*) for precise monitoring"
            ),
            first_timestamp: first_ts.to_string(),
            last_timestamp: last_ts.to_string(),
            green_impact: None,
            confidence: Confidence::default(),
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
                ev.service = service.to_string();
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
