//! Pipeline: wires all stages together.

use crate::config::Config;
use crate::correlate;
use crate::detect;
use crate::detect::{Confidence, DetectConfig};
use crate::event::SpanEvent;
use crate::normalize;
use crate::report::{Analysis, Report};
use crate::score;

/// Run the full analysis pipeline on a batch of events.
#[must_use]
pub fn analyze(events: Vec<SpanEvent>, config: &Config) -> Report {
    analyze_with_traces(events, config).0
}

/// Run the full analysis pipeline, returning both the report and the correlated traces.
///
/// Use this when you need the intermediate `Trace` structures (e.g., for tree building
/// in the TUI inspect mode) without re-running normalization and correlation.
#[must_use]
pub fn analyze_with_traces(
    events: Vec<SpanEvent>,
    config: &Config,
) -> (Report, Vec<correlate::Trace>) {
    let start = std::time::Instant::now();
    let event_count = events.len();

    let normalized = normalize::normalize_all(events);
    let traces = correlate::correlate(normalized);
    let trace_count = traces.len();

    let detect_config = DetectConfig::from(config);
    let mut findings = detect::detect(&traces, &detect_config);

    // Cross-trace slow percentile analysis
    let cross_trace = detect::slow::detect_slow_cross_trace(
        &traces,
        detect_config.slow_threshold_ms,
        detect_config.slow_min_occurrences,
    );
    findings.extend(cross_trace);

    let (mut findings, green_summary) = if config.green_enabled {
        let carbon_ctx = config.carbon_context();
        score::score_green(&traces, findings, Some(&carbon_ctx))
    } else {
        let total_io_ops = traces.iter().map(|t| t.spans.len()).sum();
        (
            findings,
            crate::report::GreenSummary::disabled(total_io_ops),
        )
    };

    // Sort findings for deterministic output (HashMap iteration order is random)
    detect::sort_findings(&mut findings);

    // stamp confidence on every finding. `analyze` is the batch
    // path — always CiBatch regardless of the daemon environment config.
    // The real daemon path (daemon::process_traces) stamps Staging or
    // Production from Config::confidence(). Detectors themselves never
    // reason about confidence — they emit Confidence::default() and the
    // pipeline caller overrides it here.
    for finding in &mut findings {
        finding.confidence = Confidence::CiBatch;
    }

    let quality_gate = crate::quality_gate::evaluate(&findings, &green_summary, config);

    let report = Report {
        analysis: Analysis {
            duration_ms: start.elapsed().as_millis() as u64,
            events_processed: event_count,
            traces_analyzed: trace_count,
        },
        findings,
        green_summary,
        quality_gate,
    };

    (report, traces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SpanEvent;

    #[test]
    fn empty_pipeline_produces_empty_report() {
        let config = Config::default();
        let report = analyze(vec![], &config);
        assert!(report.findings.is_empty());
        assert_eq!(report.analysis.events_processed, 0);
        assert_eq!(report.analysis.traces_analyzed, 0);
        assert!(report.quality_gate.passed);
    }

    #[test]
    fn waste_dedup_no_double_count() {
        use crate::test_helpers::{make_sql_event, make_sql_series_events};
        // 5 different params + 2 duplicates of param 1 = 7 events, same template
        // N+1 sees 7 occurrences with 5 distinct params -> finding (avoidable = 6)
        // Redundant sees 3 occurrences of order_id=1 -> finding (avoidable = 2)
        // Without dedup: 6 + 2 = 8. With dedup: max(6, 2) = 6.
        let mut events: Vec<SpanEvent> = make_sql_series_events(5);
        // Add 2 more with order_id = 1 (duplicates)
        for i in 6..=7 {
            events.push(make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                "SELECT * FROM order_item WHERE order_id = 1",
                &format!("2025-07-10T14:32:01.{:03}Z", i * 40),
            ));
        }

        let config = Config::default();
        let report = analyze(events, &config);
        assert!(!report.findings.is_empty());
        assert_eq!(report.green_summary.avoidable_io_ops, 6);
    }

    #[test]
    fn zero_events_waste_ratio_is_zero() {
        let config = Config::default();
        let report = analyze(vec![], &config);
        assert!((report.green_summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert_eq!(report.green_summary.total_io_ops, 0);
        assert_eq!(report.green_summary.avoidable_io_ops, 0);
    }

    #[test]
    fn clean_events_zero_waste_ratio() {
        use crate::test_helpers::make_sql_event;
        // 4 events with different templates -> no N+1 (below threshold), no redundant
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
            make_sql_event(
                "trace-1",
                "span-3",
                "SELECT * FROM products WHERE id = 3",
                "2025-07-10T14:32:01.100Z",
            ),
            make_sql_event(
                "trace-1",
                "span-4",
                "INSERT INTO logs (msg) VALUES ('ok')",
                "2025-07-10T14:32:01.150Z",
            ),
        ];

        let config = Config::default();
        let report = analyze(events, &config);

        assert!(report.findings.is_empty());
        assert_eq!(report.green_summary.total_io_ops, 4);
        assert_eq!(report.green_summary.avoidable_io_ops, 0);
        assert!((report.green_summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn pipeline_with_findings_computes_green_summary() {
        use crate::test_helpers::make_n_plus_one_events;
        // 6 events with different params -> N+1 finding
        let events = make_n_plus_one_events();

        let config = Config::default();
        let report = analyze(events, &config);

        assert!(!report.findings.is_empty());
        assert_eq!(report.green_summary.avoidable_io_ops, 5);
        assert!((report.green_summary.io_waste_ratio - 5.0_f64 / 6.0).abs() < f64::EPSILON);
        assert_eq!(report.green_summary.total_io_ops, 6);
    }

    #[test]
    fn dedup_across_traces() {
        use crate::test_helpers::make_sql_event;
        // Two traces, each with redundant queries on different templates
        let mut events = Vec::new();
        for i in 1..=3 {
            events.push(make_sql_event(
                "trace-A",
                &format!("span-a{i}"),
                "SELECT * FROM order_item WHERE order_id = 42",
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            ));
        }
        for i in 1..=3 {
            events.push(make_sql_event(
                "trace-B",
                &format!("span-b{i}"),
                "SELECT * FROM orders WHERE user_id = 7",
                &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
            ));
        }

        let config = Config::default();
        let report = analyze(events, &config);

        // Each trace has 3 redundant -> avoidable = 2 each -> total = 4
        assert_eq!(report.green_summary.avoidable_io_ops, 4);
        assert_eq!(report.green_summary.total_io_ops, 6);
    }

    #[test]
    fn pipeline_with_green_default_region_produces_co2() {
        use crate::test_helpers::make_n_plus_one_events;
        let events = make_n_plus_one_events();

        let config = Config {
            green_default_region: Some("eu-west-3".to_string()),
            ..Config::default()
        };
        let report = analyze(events, &config);

        let co2 = report
            .green_summary
            .co2
            .as_ref()
            .expect("co2 should be Some when default_region is configured");
        assert!(co2.total.mid > 0.0);
        assert!(co2.avoidable.mid > 0.0);
    }

    #[test]
    fn pipeline_empty_traces_no_co2() {
        // With 0 events, compute_carbon_report early-returns
        // (None, vec![]) — nothing meaningful to report.
        // Avoids emitting a noisy all-zeros co2 object for empty daemon ticks.
        let config = Config::default();
        let report = analyze(vec![], &config);
        assert!(
            report.green_summary.co2.is_none(),
            "co2 should be None for empty traces"
        );
        assert!(report.green_summary.regions.is_empty());
    }

    #[test]
    fn green_disabled_skips_scoring() {
        use crate::test_helpers::make_n_plus_one_events;
        // 6 events -> N+1 finding, but green scoring disabled
        let events = make_n_plus_one_events();

        let config = Config {
            green_enabled: false,
            ..Config::default()
        };
        let report = analyze(events, &config);

        // Findings are still detected
        assert!(!report.findings.is_empty());
        // But green scoring is bypassed
        assert_eq!(report.green_summary.avoidable_io_ops, 0);
        assert!((report.green_summary.io_waste_ratio - 0.0).abs() < f64::EPSILON);
        assert!(report.green_summary.top_offenders.is_empty());
        assert!(report.green_summary.co2.is_none());
        assert!(report.green_summary.regions.is_empty());
        // total_io_ops still counted
        assert_eq!(report.green_summary.total_io_ops, 6);
        // green_impact on findings should be None
        for f in &report.findings {
            assert!(f.green_impact.is_none());
        }
    }

    #[test]
    fn green_disabled_with_region_still_no_co2() {
        let config = Config {
            green_enabled: false,
            green_default_region: Some("eu-west-3".to_string()),
            ..Config::default()
        };
        let report = analyze(vec![], &config);
        assert!(report.green_summary.co2.is_none());
    }

    // --- batch mode always stamps CiBatch ---

    #[test]
    fn batch_analyze_stamps_ci_batch_confidence() {
        use crate::test_helpers::make_n_plus_one_events;
        let events = make_n_plus_one_events();
        // Even with a production environment in config, batch analyze
        // must stamp CiBatch — confidence is mode-driven, not config-driven,
        // for `analyze` (the config `daemon_environment` only affects
        // `watch` daemon mode).
        let config = Config {
            daemon_environment: crate::config::DaemonEnvironment::Production,
            ..Config::default()
        };
        let report = analyze(events, &config);
        assert!(!report.findings.is_empty());
        for f in &report.findings {
            assert_eq!(f.confidence, Confidence::CiBatch);
        }
    }
}
