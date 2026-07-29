//! Quality gate evaluation: checks findings and `GreenOps` metrics against thresholds.

use crate::config::ThresholdsConfig;
use crate::detect::{Finding, FindingType, Severity};
use crate::report::{GreenSummary, QualityGate, QualityRule};

/// Evaluate quality gate rules against findings and green summary.
#[must_use]
pub fn evaluate(
    findings: &[Finding],
    green_summary: &GreenSummary,
    thresholds: &ThresholdsConfig,
) -> QualityGate {
    let mut rules = Vec::with_capacity(4);

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
        let gate = evaluate(&[], &summary, &config.thresholds);

        assert!(gate.passed);
        assert_eq!(gate.rules.len(), 4);
        assert!(gate.rules.iter().all(|r| r.passed));
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
        let gate = evaluate(&findings, &summary, &config.thresholds);

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
        let gate = evaluate(&findings, &summary, &config.thresholds);

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
        let gate = evaluate(&findings, &summary, &config.thresholds);

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
        let gate = evaluate(&findings, &summary, &config.thresholds);

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
        let gate = evaluate(&findings, &summary, &config.thresholds);

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
        let gate = evaluate(&[], &summary, &config.thresholds);

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
        let gate = evaluate(&findings, &summary, &config.thresholds);

        assert!(gate.passed, "2 critical SQL <= 5, 0.8 <= 0.90");
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
        let gate = evaluate(&findings, &summary, &config.thresholds);

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
