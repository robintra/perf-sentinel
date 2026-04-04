//! Scoring stage: computes `GreenOps` I/O intensity scores.

pub mod carbon;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::detect::{Finding, GreenImpact};
use crate::report::{GreenSummary, TopOffender};

/// Per-endpoint statistics accumulated during scoring.
struct EndpointStats<'a> {
    total_io_ops: usize,
    invocation_count: usize,
    service: &'a str,
}

/// Count I/O ops per endpoint and invocations (distinct traces per endpoint).
fn count_endpoint_stats(traces: &[Trace]) -> (HashMap<&str, EndpointStats<'_>>, usize) {
    let mut endpoint_stats: HashMap<&str, EndpointStats<'_>> =
        HashMap::with_capacity(traces.len().min(64));
    let mut total_io_ops: usize = 0;
    let mut seen_endpoints: HashSet<&str> = HashSet::new();

    for trace in traces {
        seen_endpoints.clear();
        for span in &trace.spans {
            total_io_ops += 1;
            let key = span.event.source.endpoint.as_str();
            let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats {
                total_io_ops: 0,
                invocation_count: 0,
                service: span.event.service.as_str(),
            });
            stats.total_io_ops += 1;
            seen_endpoints.insert(key);
        }
        for &ep in &seen_endpoints {
            if let Some(stats) = endpoint_stats.get_mut(ep) {
                stats.invocation_count += 1;
            }
        }
    }

    (endpoint_stats, total_io_ops)
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
pub fn score_green(
    traces: &[Trace],
    findings: Vec<Finding>,
    region: Option<&str>,
) -> (Vec<Finding>, GreenSummary) {
    let (endpoint_stats, total_io_ops) = count_endpoint_stats(traces);

    // Phase 2: Dedup avoidable I/O ops by (trace_id, template), taking max.
    // Slow findings are excluded: slow queries are not "avoidable" I/O, they are
    // necessary operations that happen to be slow.
    let mut dedup: HashMap<(&str, &str, &str), usize> = HashMap::with_capacity(findings.len());
    for f in &findings {
        if !f.finding_type.is_avoidable_io() {
            continue;
        }
        let avoidable = f.pattern.occurrences.saturating_sub(1);
        let entry = dedup
            .entry((&f.trace_id, &f.pattern.template, &f.source_endpoint))
            .or_insert(0);
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

        let extra = if f.finding_type.is_avoidable_io() {
            f.pattern.occurrences.saturating_sub(1)
        } else {
            0
        };
        f.green_impact = Some(GreenImpact {
            estimated_extra_io_ops: extra,
            io_intensity_score: iis,
        });
    }

    // Phase 5: Build top offenders sorted by IIS descending, with alphabetical tiebreaker
    // Pre-lowercase region once to avoid repeated allocation in lookup_region
    let region_lower = region.map(str::to_ascii_lowercase);
    let region_lower_ref = region_lower.as_deref();
    let mut top_offenders: Vec<TopOffender> = endpoint_stats
        .into_iter()
        .map(|(endpoint, stats)| {
            let iis = iis_map.get(endpoint).copied().unwrap_or(0.0);
            let co2_grams =
                region_lower_ref.and_then(|r| carbon::io_ops_to_co2_grams(stats.total_io_ops, r));
            TopOffender {
                endpoint: endpoint.to_string(),
                service: stats.service.to_string(),
                io_intensity_score: iis,
                co2_grams,
            }
        })
        .collect();
    top_offenders.sort_by(|a, b| {
        b.io_intensity_score
            .partial_cmp(&a.io_intensity_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });

    let estimated_co2_grams =
        region_lower_ref.and_then(|r| carbon::io_ops_to_co2_grams(total_io_ops, r));
    let avoidable_co2_grams =
        region_lower_ref.and_then(|r| carbon::io_ops_to_co2_grams(avoidable_io_ops, r));

    let green_summary = GreenSummary {
        total_io_ops,
        avoidable_io_ops,
        io_waste_ratio: if total_io_ops > 0 {
            avoidable_io_ops as f64 / total_io_ops as f64
        } else {
            0.0
        },
        top_offenders,
        estimated_co2_grams,
        avoidable_co2_grams,
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
        let (findings, summary) = score_green(&[], vec![], None);
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
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
        };

        let (findings, summary) = score_green(&[trace], vec![finding], None);

        assert_eq!(summary.total_io_ops, 6);
        assert_eq!(summary.avoidable_io_ops, 5);
        assert!((summary.io_waste_ratio - 5.0 / 6.0).abs() < f64::EPSILON);
        assert_eq!(summary.top_offenders.len(), 1);
        assert!((summary.top_offenders[0].io_intensity_score - 6.0).abs() < f64::EPSILON);

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
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let events_t2: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-B",
                    &format!("span-b{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {}", i + 10),
                    &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace_a = make_trace(events_t1);
        let trace_b = make_trace(events_t2);

        let (_, summary) = score_green(&[trace_a, trace_b], vec![], None);
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
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
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

        let (_, summary) = score_green(&[trace], vec![], None);

        assert_eq!(summary.top_offenders.len(), 2);
        assert_eq!(
            summary.top_offenders[0].endpoint,
            "POST /api/orders/42/submit"
        );
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
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
        };

        let (findings, _) = score_green(&[trace], vec![finding], None);

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
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let template = "SELECT * FROM order_item WHERE order_id = ?".to_string();
        let findings = vec![
            Finding {
                finding_type: FindingType::NPlusOneSql,
                severity: Severity::Warning,
                trace_id: "trace-1".to_string(),
                service: "order-svc".to_string(),
                source_endpoint: "POST /api/orders/42/submit".to_string(),
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
                service: "order-svc".to_string(),
                source_endpoint: "POST /api/orders/42/submit".to_string(),
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

        let (_, summary) = score_green(&[trace], findings, None);
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

        let (findings, summary) = score_green(&[trace], vec![], None);

        assert!(findings.is_empty());
        assert_eq!(summary.total_io_ops, 4);
        assert_eq!(summary.avoidable_io_ops, 0);
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert_eq!(summary.top_offenders.len(), 1); // 1 endpoint
    }

    #[test]
    fn co2_computed_when_region_set() {
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
        };

        let (_, summary) = score_green(&[trace], vec![finding], Some("eu-west-3"));

        assert!(summary.estimated_co2_grams.is_some());
        assert!(summary.avoidable_co2_grams.is_some());
        assert!(summary.estimated_co2_grams.unwrap() > 0.0);
        assert!(summary.avoidable_co2_grams.unwrap() > 0.0);
        // Top offender should also have CO2
        assert_eq!(summary.top_offenders.len(), 1);
        assert!(summary.top_offenders[0].co2_grams.is_some());
        assert!(summary.top_offenders[0].co2_grams.unwrap() > 0.0);
    }

    #[test]
    fn co2_none_when_no_region() {
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);

        let (_, summary) = score_green(&[trace], vec![], None);

        assert!(summary.estimated_co2_grams.is_none());
        assert!(summary.avoidable_co2_grams.is_none());
        for offender in &summary.top_offenders {
            assert!(offender.co2_grams.is_none());
        }
    }

    #[test]
    fn co2_none_for_unknown_region() {
        let events = vec![make_sql_event(
            "trace-1",
            "span-1",
            "SELECT 1",
            "2025-07-10T14:32:01.000Z",
        )];
        let trace = make_trace(events);

        let (_, summary) = score_green(&[trace], vec![], Some("mars-1"));

        assert!(summary.estimated_co2_grams.is_none());
        assert!(summary.avoidable_co2_grams.is_none());
        for offender in &summary.top_offenders {
            assert!(offender.co2_grams.is_none());
        }
    }

    #[test]
    fn slow_findings_do_not_inflate_waste_ratio() {
        // 3 slow SQL events (same template) -> slow_sql finding with 3 occurrences
        // These should NOT count as avoidable I/O.
        use crate::test_helpers::make_sql_event_with_duration;
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM t WHERE id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                    600_000,
                )
            })
            .collect();
        let trace = make_trace(events);

        let slow_finding = Finding {
            finding_type: FindingType::SlowSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 3,
                window_ms: 100,
                distinct_params: 3,
            },
            suggestion: "Consider adding an index".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.150Z".to_string(),
            green_impact: None,
        };

        let (findings, summary) = score_green(&[trace], vec![slow_finding], None);

        // Slow findings should NOT contribute to avoidable ops
        assert_eq!(summary.avoidable_io_ops, 0, "slow ops are not avoidable");
        assert!((summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);

        // green_impact.estimated_extra_io_ops should be 0 for slow findings
        let impact = findings[0].green_impact.as_ref().unwrap();
        assert_eq!(impact.estimated_extra_io_ops, 0);
    }

    #[test]
    fn slow_and_n_plus_one_waste_separate() {
        // Mix: 6 N+1 events + 3 slow events on same trace, different templates
        // Only N+1 should contribute to waste, not slow.
        use crate::test_helpers::make_sql_event_with_duration;
        let mut events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        for i in 7..=9 {
            events.push(make_sql_event_with_duration(
                "trace-1",
                &format!("span-{i}"),
                &format!("SELECT * FROM slow_table WHERE id = {}", i - 6),
                &format!("2025-07-10T14:32:02.{:03}Z", (i - 6) * 50),
                600_000,
            ));
        }
        let trace = make_trace(events);

        let n1_finding = Finding {
            finding_type: FindingType::NPlusOneSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.300Z".to_string(),
            green_impact: None,
        };
        let slow_finding = Finding {
            finding_type: FindingType::SlowSql,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM slow_table WHERE id = ?".to_string(),
                occurrences: 3,
                window_ms: 100,
                distinct_params: 3,
            },
            suggestion: "Consider adding an index".to_string(),
            first_timestamp: "2025-07-10T14:32:02.050Z".to_string(),
            last_timestamp: "2025-07-10T14:32:02.150Z".to_string(),
            green_impact: None,
        };

        let (findings, summary) = score_green(&[trace], vec![n1_finding, slow_finding], None);

        // Only the N+1 finding's occurrences - 1 = 5 should be avoidable
        assert_eq!(summary.avoidable_io_ops, 5);
        // N+1 finding should have extra_io_ops = 5
        let n1 = findings
            .iter()
            .find(|f| f.finding_type == FindingType::NPlusOneSql)
            .unwrap();
        assert_eq!(n1.green_impact.as_ref().unwrap().estimated_extra_io_ops, 5);
        // Slow finding should have extra_io_ops = 0
        let slow = findings
            .iter()
            .find(|f| f.finding_type == FindingType::SlowSql)
            .unwrap();
        assert_eq!(
            slow.green_impact.as_ref().unwrap().estimated_extra_io_ops,
            0
        );
    }
}
