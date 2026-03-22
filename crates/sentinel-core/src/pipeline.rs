//! Pipeline: wires all stages together.

use crate::config::Config;
use crate::correlate;
use crate::detect;
use crate::event::SpanEvent;
use crate::normalize;
use crate::report::{Analysis, QualityGate, Report};
use crate::score;

/// Run the full analysis pipeline on a batch of events.
#[must_use]
pub fn analyze(events: Vec<SpanEvent>, config: &Config) -> Report {
    let start = std::time::Instant::now();
    let event_count = events.len();

    let normalized = normalize::normalize_all(events);
    let traces = correlate::correlate(normalized);
    let trace_count = traces.len();
    let findings = detect::detect(
        &traces,
        config.n_plus_one_threshold,
        config.window_duration_ms,
    );

    let (findings, green_summary) = score::score_green(&traces, findings);

    let quality_gate = QualityGate {
        passed: true,
        rules: vec![],
    };

    Report {
        analysis: Analysis {
            duration_ms: start.elapsed().as_millis() as u64,
            events_processed: event_count,
            traces_analyzed: trace_count,
        },
        findings,
        green_summary,
        quality_gate,
    }
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
        use crate::test_helpers::make_sql_event;
        // 5 different params + 2 duplicates of param 1 = 7 events, same template
        // N+1 sees 7 occurrences with 5 distinct params -> finding (avoidable = 6)
        // Redundant sees 3 occurrences of game_id=1 -> finding (avoidable = 2)
        // Without dedup: 6 + 2 = 8. With dedup: max(6, 2) = 6.
        let mut events: Vec<SpanEvent> = (1..=5)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM player WHERE game_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        // Add 2 more with game_id = 1 (duplicates)
        for i in 6..=7 {
            events.push(make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                "SELECT * FROM player WHERE game_id = 1",
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
        use crate::test_helpers::make_sql_event;
        // 6 events with different params -> N+1 finding
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
                "SELECT * FROM player WHERE game_id = 42",
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
}
