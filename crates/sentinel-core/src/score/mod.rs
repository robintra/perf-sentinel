//! Scoring stage: computes `GreenOps` I/O intensity scores.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::detect::{Finding, GreenImpact};
use crate::report::{GreenSummary, TopOffender};

/// Per-endpoint statistics accumulated during scoring.
struct EndpointStats {
    total_io_ops: usize,
    invocation_count: usize,
    service: String,
}

/// Compute `GreenOps` scores: enrich findings with `green_impact` and produce a `GreenSummary`.
///
/// I/O operation counts are used as a proxy for energy consumption.
/// This is an approximation; actual energy depends on I/O type, latency,
/// and infrastructure, and is not measured directly.
///
/// Algorithm:
/// 1. Count I/O ops per source endpoint across all traces
/// 2. Compute IIS (I/O Intensity Score) per endpoint
/// 3. Dedup avoidable I/O ops using max per trace/template pair
/// 4. Populate `green_impact` on each finding
/// 5. Build top offenders ranking sorted by IIS descending
#[must_use]
pub fn score_green(traces: &[Trace], findings: Vec<Finding>) -> (Vec<Finding>, GreenSummary) {
    // Phase 1: Count I/O ops per endpoint and invocations (distinct traces).
    // We iterate by trace first to count each trace as one invocation per endpoint,
    // avoiding unnecessary String clones in the inner loop.
    let mut endpoint_stats: HashMap<&str, EndpointStats> = HashMap::with_capacity(traces.len());
    let mut total_io_ops: usize = 0;

    for trace in traces {
        // Collect unique endpoints in this trace to count invocations
        let mut seen_endpoints: HashSet<&str> = HashSet::new();
        for span in &trace.spans {
            total_io_ops += 1;
            let key = span.event.source.endpoint.as_str();
            let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats {
                total_io_ops: 0,
                invocation_count: 0,
                service: span.event.service.clone(),
            });
            stats.total_io_ops += 1;
            seen_endpoints.insert(key);
        }
        for ep in seen_endpoints {
            if let Some(stats) = endpoint_stats.get_mut(ep) {
                stats.invocation_count += 1;
            }
        }
    }

    // Phase 2: Dedup avoidable I/O ops by (trace_id, template), taking max
    let mut dedup: HashMap<(&str, &str), usize> = HashMap::with_capacity(findings.len());
    for f in &findings {
        let avoidable = f.pattern.occurrences.saturating_sub(1);
        let entry = dedup.entry((&f.trace_id, &f.pattern.template)).or_insert(0);
        *entry = (*entry).max(avoidable);
    }
    let avoidable_io_ops: usize = dedup.values().sum();

    // Phase 3: Compute IIS per endpoint (cached for finding enrichment)
    let iis_map: HashMap<&str, f64> = endpoint_stats
        .iter()
        .map(|(&ep, stats)| {
            let invocations = stats.invocation_count.max(1) as f64;
            (ep, stats.total_io_ops as f64 / invocations)
        })
        .collect();

    // Phase 4: Enrich findings with green_impact
    let mut enriched = findings;
    for f in &mut enriched {
        let iis = iis_map
            .get(f.source_endpoint.as_str())
            .copied()
            .unwrap_or(0.0);

        f.green_impact = Some(GreenImpact {
            estimated_extra_io_ops: f.pattern.occurrences.saturating_sub(1),
            io_intensity_score: iis,
        });
    }

    // Phase 5: Build top offenders sorted by IIS descending, with alphabetical tiebreaker
    let mut top_offenders: Vec<TopOffender> = endpoint_stats
        .into_iter()
        .map(|(endpoint, stats)| {
            let iis = iis_map.get(endpoint).copied().unwrap_or(0.0);
            TopOffender {
                endpoint: endpoint.to_string(),
                service: stats.service,
                io_intensity_score: iis,
                io_ops_per_request: iis,
            }
        })
        .collect();
    top_offenders.sort_by(|a, b| {
        b.io_intensity_score
            .partial_cmp(&a.io_intensity_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });

    let green_summary = GreenSummary {
        total_io_ops,
        avoidable_io_ops,
        io_waste_ratio: if total_io_ops > 0 {
            avoidable_io_ops as f64 / total_io_ops as f64
        } else {
            0.0
        },
        top_offenders,
    };

    (enriched, green_summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{FindingType, Pattern, Severity};
    use crate::event::SpanEvent;
    use crate::test_helpers::{make_http_event, make_sql_event, make_trace};

    #[test]
    fn empty_input_returns_empty_summary() {
        let (findings, summary) = score_green(&[], vec![]);
        assert!(findings.is_empty());
        assert_eq!(summary.total_io_ops, 0);
        assert_eq!(summary.avoidable_io_ops, 0);
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert!(summary.top_offenders.is_empty());
    }

    #[test]
    fn single_trace_computes_iis() {
        // 6 SQL events in 1 trace -> IIS = 6/1 = 6.0
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "game".to_string(),
            source_endpoint: "POST /api/game/42/start".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM player WHERE game_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
        };

        let (findings, summary) = score_green(&[trace], vec![finding]);

        assert_eq!(summary.total_io_ops, 6);
        assert_eq!(summary.avoidable_io_ops, 5);
        assert!((summary.io_waste_ratio - 5.0 / 6.0).abs() < f64::EPSILON);
        assert_eq!(summary.top_offenders.len(), 1);
        assert!((summary.top_offenders[0].io_intensity_score - 6.0).abs() < f64::EPSILON);
        assert!((summary.top_offenders[0].io_ops_per_request - 6.0).abs() < f64::EPSILON);

        assert_eq!(findings.len(), 1);
        let impact = findings[0].green_impact.as_ref().unwrap();
        assert_eq!(impact.estimated_extra_io_ops, 5);
        assert!((impact.io_intensity_score - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn multiple_traces_same_endpoint() {
        // 2 traces, each with 3 events on the same endpoint -> IIS = 6/2 = 3.0
        let events_t1: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-A",
                    &format!("span-a{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let events_t2: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-B",
                    &format!("span-b{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {}", i + 10),
                    &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace_a = make_trace(events_t1);
        let trace_b = make_trace(events_t2);

        let (_, summary) = score_green(&[trace_a, trace_b], vec![]);
        assert_eq!(summary.total_io_ops, 6);
        assert_eq!(summary.top_offenders.len(), 1);
        assert!((summary.top_offenders[0].io_intensity_score - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn top_offenders_sorted_by_iis_desc() {
        // Endpoint A: 6 events in 1 trace -> IIS = 6.0
        // Endpoint B: 2 events in 1 trace -> IIS = 2.0
        let mut events_a: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-a{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let mut events_b: Vec<SpanEvent> = (1..=2)
            .map(|i| {
                let mut e = make_sql_event(
                    "trace-1",
                    &format!("span-b{i}"),
                    &format!("SELECT * FROM orders WHERE user_id = {i}"),
                    &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
                );
                e.source.endpoint = "GET /api/orders".to_string();
                e
            })
            .collect();

        let mut all_events = Vec::new();
        all_events.append(&mut events_a);
        all_events.append(&mut events_b);
        let trace = make_trace(all_events);

        let (_, summary) = score_green(&[trace], vec![]);

        assert_eq!(summary.top_offenders.len(), 2);
        assert_eq!(summary.top_offenders[0].endpoint, "POST /api/game/42/start");
        assert_eq!(summary.top_offenders[1].endpoint, "GET /api/orders");
        assert!(
            summary.top_offenders[0].io_intensity_score
                >= summary.top_offenders[1].io_intensity_score
        );
    }

    #[test]
    fn green_impact_populated_on_findings() {
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "game".to_string(),
            source_endpoint: "POST /api/game/42/start".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM player WHERE game_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
        };

        let (findings, _) = score_green(&[trace], vec![finding]);

        let impact = findings[0].green_impact.as_ref().unwrap();
        assert_eq!(impact.estimated_extra_io_ops, 5);
        assert!((impact.io_intensity_score - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn dedup_avoidable_across_finding_types() {
        // Same trace, same template: N+1 (6 occurrences, avoidable=5) + redundant (3 occurrences, avoidable=2)
        // After dedup: max(5, 2) = 5
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let template = "SELECT * FROM player WHERE game_id = ?".to_string();
        let findings = vec![
            Finding {
                finding_type: FindingType::NPlusOneSql,
                severity: Severity::Warning,
                trace_id: "trace-1".to_string(),
                service: "game".to_string(),
                source_endpoint: "POST /api/game/42/start".to_string(),
                pattern: Pattern {
                    template: template.clone(),
                    occurrences: 6,
                    window_ms: 250,
                    distinct_params: 6,
                },
                suggestion: "batch".to_string(),
                first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
                last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
                green_impact: None,
            },
            Finding {
                finding_type: FindingType::RedundantSql,
                severity: Severity::Info,
                trace_id: "trace-1".to_string(),
                service: "game".to_string(),
                source_endpoint: "POST /api/game/42/start".to_string(),
                pattern: Pattern {
                    template,
                    occurrences: 3,
                    window_ms: 100,
                    distinct_params: 1,
                },
                suggestion: "cache".to_string(),
                first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
                last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
                green_impact: None,
            },
        ];

        let (_, summary) = score_green(&[trace], findings);
        // max(5, 2) = 5
        assert_eq!(summary.avoidable_io_ops, 5);
    }

    #[test]
    fn clean_traces_zero_waste() {
        // 4 events, no findings -> waste ratio = 0
        let events = vec![
            make_sql_event(
                "trace-1",
                "span-1",
                "SELECT * FROM users WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
            ),
            make_sql_event(
                "trace-1",
                "span-2",
                "SELECT * FROM orders WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
            ),
            make_http_event(
                "trace-1",
                "span-3",
                "http://svc:5000/api/health",
                "2025-07-10T14:32:01.100Z",
            ),
            make_sql_event(
                "trace-1",
                "span-4",
                "INSERT INTO logs (msg) VALUES ('ok')",
                "2025-07-10T14:32:01.150Z",
            ),
        ];
        let trace = make_trace(events);

        let (findings, summary) = score_green(&[trace], vec![]);

        assert!(findings.is_empty());
        assert_eq!(summary.total_io_ops, 4);
        assert_eq!(summary.avoidable_io_ops, 0);
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert_eq!(summary.top_offenders.len(), 1); // 1 endpoint
    }
}
