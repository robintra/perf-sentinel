//! Quality gate evaluation: checks findings and `GreenOps` metrics against thresholds.

use crate::config::ThresholdsConfig;
use crate::detect::{Finding, FindingType, Severity};
use crate::report::{GreenSummary, IngestStats, QualityGate, QualityRule};

/// Human-readable label for a gate rule key, or `None` for a key this
/// build does not know.
///
/// `None` matters for security as much as for display: a rule name read
/// back from `/api/export/report` is attacker-influenced, so a caller that
/// falls through to the raw key must sanitize it. Known keys are literals
/// and need no sanitizing.
///
/// The HTML dashboard carries its own copy in `GATE_RULE_LABELS`, because
/// it renders standalone with no Rust at hand. Keep the two in step.
#[must_use]
pub fn rule_label(rule: &str) -> Option<&'static str> {
    match rule {
        "n_plus_one_sql_critical_max" => Some("Max critical N+1 SQL"),
        "n_plus_one_http_warning_max" => Some("Max N+1 HTTP (warning+)"),
        "n_plus_one_messaging_warning_max" => Some("Max N+1 messaging (warning+)"),
        "io_waste_ratio_max" => Some("Max I/O waste ratio"),
        "min_usable_span_ratio" => Some("Min usable span ratio"),
        _ => None,
    }
}

/// Evaluate quality gate rules against findings, green summary and the
/// ingest tally. `ingest` is `None` on inputs with no OTLP filter stats
/// (native, Jaeger, Zipkin, daemon window reports); the usable-span rule
/// is skipped there rather than passed, so its absence is visible.
#[must_use]
pub fn evaluate(
    findings: &[Finding],
    green_summary: &GreenSummary,
    thresholds: &ThresholdsConfig,
    ingest: Option<&IngestStats>,
) -> QualityGate {
    let mut rules = Vec::with_capacity(5);

    // Rule 1: n_plus_one_sql_critical_max
    let critical_sql_count = findings
        .iter()
        .filter(|f| f.finding_type == FindingType::NPlusOneSql && f.severity == Severity::Critical)
        .count();
    let threshold_sql = thresholds.n_plus_one_sql_critical_max;
    rules.push(QualityRule {
        rule: "n_plus_one_sql_critical_max".to_string(),
        threshold: f64::from(threshold_sql),
        actual: critical_sql_count as f64,
        passed: critical_sql_count <= threshold_sql as usize,
    });

    // Rule 2: n_plus_one_http_warning_max (counts warning+ severity, i.e. warning and critical)
    let warning_plus_http_count = findings
        .iter()
        .filter(|f| {
            f.finding_type == FindingType::NPlusOneHttp
                && matches!(f.severity, Severity::Warning | Severity::Critical)
        })
        .count();
    let threshold_http = thresholds.n_plus_one_http_warning_max;
    rules.push(QualityRule {
        rule: "n_plus_one_http_warning_max".to_string(),
        threshold: f64::from(threshold_http),
        actual: warning_plus_http_count as f64,
        passed: warning_plus_http_count <= threshold_http as usize,
    });

    // Rule 3: n_plus_one_messaging_warning_max. Warning+ like HTTP rather
    // than critical-only like SQL: a Kafka client may already batch the
    // publishes it buffers, so the count is an upper bound.
    let warning_plus_messaging_count = findings
        .iter()
        .filter(|f| {
            f.finding_type == FindingType::NPlusOneMessaging
                && matches!(f.severity, Severity::Warning | Severity::Critical)
        })
        .count();
    let threshold_messaging = thresholds.n_plus_one_messaging_warning_max;
    rules.push(QualityRule {
        rule: "n_plus_one_messaging_warning_max".to_string(),
        threshold: f64::from(threshold_messaging),
        actual: warning_plus_messaging_count as f64,
        passed: warning_plus_messaging_count <= threshold_messaging as usize,
    });

    // Rule 4: io_waste_ratio_max
    rules.push(QualityRule {
        rule: "io_waste_ratio_max".to_string(),
        threshold: thresholds.io_waste_ratio_max,
        actual: green_summary.io_waste_ratio,
        passed: green_summary.io_waste_ratio <= thresholds.io_waste_ratio_max,
    });

    // Rule 5: min_usable_span_ratio, opt-in and lower-bound. A report thin
    // because SQL spans ship without db.statement (or HTTP without http.url)
    // must fail loudly instead of passing as a false green. Evaluated only
    // when the operator set the threshold AND the input carried an OTLP
    // tally with at least one I/O-shaped span.
    if let Some(threshold) = thresholds.min_usable_span_ratio
        && let Some(ratio) = ingest.and_then(|i| i.usable_span_ratio)
    {
        rules.push(QualityRule {
            rule: "min_usable_span_ratio".to_string(),
            threshold,
            actual: ratio,
            passed: ratio >= threshold,
        });
    }

    let passed = rules.iter().all(|r| r.passed);
    QualityGate { passed, rules }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_helpers::{make_finding, make_test_green_summary};

    fn empty_green_summary() -> GreenSummary {
        GreenSummary::disabled(0)
    }

    #[test]
    fn all_rules_pass_with_no_findings() {
        let config = Config::default();
        let summary = empty_green_summary();
        let gate = evaluate(&[], &summary, &config.thresholds, None);

        assert!(gate.passed);
        assert_eq!(gate.rules.len(), 4);
        assert!(gate.rules.iter().all(|r| r.passed));
    }

    #[test]
    fn every_evaluated_rule_has_a_label() {
        // The CLI, the TUI and the dashboard all display these keys. A rule
        // added without its label would surface as raw snake_case.
        let config = Config::default();
        let gate = evaluate(&[], &empty_green_summary(), &config.thresholds, None);
        for r in &gate.rules {
            assert!(
                rule_label(&r.rule).is_some(),
                "rule {} has no display label",
                r.rule
            );
        }
        assert!(rule_label("some_future_rule").is_none());
    }

    #[test]
    fn messaging_n_plus_one_fails_its_own_rule() {
        // Warning+ counts, like HTTP: the gate must name the messaging rule
        // rather than only moving the global waste ratio.
        let config = Config::default(); // n_plus_one_messaging_warning_max = 3
        let summary = empty_green_summary();
        let findings: Vec<_> = (0..4)
            .map(|_| make_finding(FindingType::NPlusOneMessaging, Severity::Warning))
            .collect();
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "n_plus_one_messaging_warning_max")
            .expect("the messaging rule is evaluated");
        assert!(!rule.passed, "4 warnings exceed the default of 3");
        assert!((rule.actual - 4.0).abs() < f64::EPSILON);
        assert!(!gate.passed);
    }

    #[test]
    fn critical_sql_fails_gate() {
        let config = Config::default(); // n_plus_one_sql_critical_max = 0
        let findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        assert!(!gate.passed);
        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "n_plus_one_sql_critical_max")
            .unwrap();
        assert!(!rule.passed);
        assert!((rule.actual - 1.0).abs() < f64::EPSILON);
        assert!((rule.threshold - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn warning_sql_does_not_fail_sql_critical_rule() {
        let config = Config::default();
        let findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "n_plus_one_sql_critical_max")
            .unwrap();
        assert!(
            rule.passed,
            "warning SQL should not trigger critical-only rule"
        );
    }

    #[test]
    fn warning_http_under_threshold() {
        let config = Config {
            thresholds: ThresholdsConfig {
                n_plus_one_http_warning_max: 3,
                ..ThresholdsConfig::default()
            },
            ..Config::default()
        };
        let findings = vec![
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
        ];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "n_plus_one_http_warning_max")
            .unwrap();
        assert!(rule.passed);
    }

    #[test]
    fn warning_http_over_threshold() {
        let config = Config {
            thresholds: ThresholdsConfig {
                n_plus_one_http_warning_max: 3,
                ..ThresholdsConfig::default()
            },
            ..Config::default()
        };
        let findings = vec![
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
        ];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        assert!(!gate.passed);
        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "n_plus_one_http_warning_max")
            .unwrap();
        assert!(!rule.passed);
        assert!((rule.actual - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn io_waste_ratio_fails_gate() {
        let config = Config::default(); // io_waste_ratio_max = 0.30
        let summary = make_test_green_summary(10, 5, 0.5);
        let gate = evaluate(&[], &summary, &config.thresholds, None);

        assert!(!gate.passed);
        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "io_waste_ratio_max")
            .unwrap();
        assert!(!rule.passed);
        assert!((rule.actual - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn custom_thresholds() {
        let config = Config {
            thresholds: ThresholdsConfig {
                n_plus_one_sql_critical_max: 5,
                io_waste_ratio_max: 0.90,
                ..ThresholdsConfig::default()
            },
            ..Config::default()
        };
        let findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Critical),
            make_finding(FindingType::NPlusOneSql, Severity::Critical),
        ];
        let summary = make_test_green_summary(10, 8, 0.8);
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        assert!(gate.passed, "2 critical SQL <= 5, 0.8 <= 0.90");
    }

    /// An [`IngestStats`] whose SQL usable ratio is
    /// `retained / (retained + gaps)`, the only kind present.
    fn ingest_stats(retained: u64, attribute_gaps: u64) -> IngestStats {
        let stats = crate::ingest::otlp::SpanConversionStats {
            received: retained + attribute_gaps,
            filtered_missing_db_statement: attribute_gaps,
            retained_sql: retained,
            ..Default::default()
        };
        IngestStats::from(stats)
    }

    fn thresholds_with_min_ratio(ratio: f64) -> ThresholdsConfig {
        ThresholdsConfig {
            min_usable_span_ratio: Some(ratio),
            ..ThresholdsConfig::default()
        }
    }

    #[test]
    fn usable_span_rule_absent_without_threshold() {
        // Default config: the rule is opt-in, stats alone must not add it.
        let config = Config::default();
        let stats = ingest_stats(1, 9);
        let gate = evaluate(
            &[],
            &empty_green_summary(),
            &config.thresholds,
            Some(&stats),
        );
        assert!(gate.passed);
        assert!(!gate.rules.iter().any(|r| r.rule == "min_usable_span_ratio"));
    }

    #[test]
    fn usable_span_rule_absent_without_stats() {
        // Threshold set but no OTLP tally (native/Jaeger/Zipkin input):
        // the rule is skipped, not passed, so the gate stays green.
        let thresholds = thresholds_with_min_ratio(0.9);
        let gate = evaluate(&[], &empty_green_summary(), &thresholds, None);
        assert!(gate.passed);
        assert!(!gate.rules.iter().any(|r| r.rule == "min_usable_span_ratio"));
    }

    #[test]
    fn unusable_instrumentation_fails_gate() {
        // The false-green scenario: 9 of 10 I/O-shaped spans lack their
        // attribute, ratio 0.1 < 0.9 threshold, the gate must fail even
        // though there are zero findings.
        let thresholds = thresholds_with_min_ratio(0.9);
        let stats = ingest_stats(1, 9);
        let gate = evaluate(&[], &empty_green_summary(), &thresholds, Some(&stats));
        assert!(!gate.passed);
        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "min_usable_span_ratio")
            .expect("rule evaluated when threshold and stats are present");
        assert!(!rule.passed);
        assert!((rule.actual - 0.1).abs() < f64::EPSILON);
        assert!((rule.threshold - 0.9).abs() < f64::EPSILON);
        assert!(rule_label(&rule.rule).is_some(), "new rule needs a label");
    }

    #[test]
    fn healthy_instrumentation_passes_usable_span_rule() {
        let thresholds = thresholds_with_min_ratio(0.9);
        let stats = ingest_stats(19, 1);
        let gate = evaluate(&[], &empty_green_summary(), &thresholds, Some(&stats));
        assert!(gate.passed);
        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "min_usable_span_ratio")
            .expect("rule evaluated");
        assert!(rule.passed);
        assert!((rule.actual - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn usable_span_rule_absent_without_io_shaped_spans() {
        // A tally of internal-only spans has no ratio (0/0): skip the
        // rule rather than judging instrumentation that sent no I/O.
        let thresholds = thresholds_with_min_ratio(0.9);
        let stats = ingest_stats(0, 0);
        let gate = evaluate(&[], &empty_green_summary(), &thresholds, Some(&stats));
        assert!(gate.passed);
        assert!(!gate.rules.iter().any(|r| r.rule == "min_usable_span_ratio"));
    }

    #[test]
    fn critical_http_counts_as_warning_plus() {
        let config = Config {
            thresholds: ThresholdsConfig {
                n_plus_one_http_warning_max: 0,
                ..ThresholdsConfig::default()
            },
            ..Config::default()
        };
        let findings = vec![make_finding(FindingType::NPlusOneHttp, Severity::Critical)];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config.thresholds, None);

        let rule = gate
            .rules
            .iter()
            .find(|r| r.rule == "n_plus_one_http_warning_max")
            .unwrap();
        assert!(
            !rule.passed,
            "critical HTTP should count toward warning+ threshold"
        );
    }
}
