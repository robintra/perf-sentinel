//! Quality gate evaluation: checks findings and `GreenOps` metrics against thresholds.

use crate::config::Config;
use crate::detect::{Finding, FindingType, Severity};
use crate::report::{GreenSummary, QualityGate, QualityRule};

/// Evaluate quality gate rules against findings and green summary.
#[must_use]
pub fn evaluate(
    findings: &[Finding],
    green_summary: &GreenSummary,
    config: &Config,
) -> QualityGate {
    let mut rules = Vec::with_capacity(3);

    // Rule 1: n_plus_one_sql_critical_max
    let critical_sql_count = findings
        .iter()
        .filter(|f| f.finding_type == FindingType::NPlusOneSql && f.severity == Severity::Critical)
        .count();
    let threshold_sql = config.n_plus_one_sql_critical_max;
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
    let threshold_http = config.n_plus_one_http_warning_max;
    rules.push(QualityRule {
        rule: "n_plus_one_http_warning_max".to_string(),
        threshold: f64::from(threshold_http),
        actual: warning_plus_http_count as f64,
        passed: warning_plus_http_count <= threshold_http as usize,
    });

    // Rule 3: io_waste_ratio_max
    rules.push(QualityRule {
        rule: "io_waste_ratio_max".to_string(),
        threshold: config.io_waste_ratio_max,
        actual: green_summary.io_waste_ratio,
        passed: green_summary.io_waste_ratio <= config.io_waste_ratio_max,
    });

    let passed = rules.iter().all(|r| r.passed);
    QualityGate { passed, rules }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Confidence, GreenImpact, Pattern};

    fn make_finding(finding_type: FindingType, severity: Severity) -> Finding {
        Finding {
            finding_type,
            severity,
            trace_id: "trace-1".to_string(),
            service: "order-svc".to_string(),
            source_endpoint: "POST /api/orders/42/submit".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM t WHERE id = ?".to_string(),
                occurrences: 6,
                window_ms: 200,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: Some(GreenImpact {
                estimated_extra_io_ops: 5,
                io_intensity_score: 6.0,
            }),
            confidence: Confidence::default(),
        }
    }

    fn empty_green_summary() -> GreenSummary {
        GreenSummary {
            total_io_ops: 0,
            avoidable_io_ops: 0,
            io_waste_ratio: 0.0,
            top_offenders: vec![],
            co2: None,
            regions: vec![],
        }
    }

    #[test]
    fn all_rules_pass_with_no_findings() {
        let config = Config::default();
        let summary = empty_green_summary();
        let gate = evaluate(&[], &summary, &config);

        assert!(gate.passed);
        assert_eq!(gate.rules.len(), 3);
        assert!(gate.rules.iter().all(|r| r.passed));
    }

    #[test]
    fn critical_sql_fails_gate() {
        let config = Config::default(); // n_plus_one_sql_critical_max = 0
        let findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config);

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
        let gate = evaluate(&findings, &summary, &config);

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
            n_plus_one_http_warning_max: 3,
            ..Config::default()
        };
        let findings = vec![
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
        ];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config);

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
            n_plus_one_http_warning_max: 3,
            ..Config::default()
        };
        let findings = vec![
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
        ];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config);

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
        let summary = GreenSummary {
            total_io_ops: 10,
            avoidable_io_ops: 5,
            io_waste_ratio: 0.5,
            top_offenders: vec![],
            co2: None,
            regions: vec![],
        };
        let gate = evaluate(&[], &summary, &config);

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
            n_plus_one_sql_critical_max: 5,
            io_waste_ratio_max: 0.90,
            ..Config::default()
        };
        let findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Critical),
            make_finding(FindingType::NPlusOneSql, Severity::Critical),
        ];
        let summary = GreenSummary {
            total_io_ops: 10,
            avoidable_io_ops: 8,
            io_waste_ratio: 0.8,
            top_offenders: vec![],
            co2: None,
            regions: vec![],
        };
        let gate = evaluate(&findings, &summary, &config);

        assert!(gate.passed, "2 critical SQL <= 5, 0.8 <= 0.90");
    }

    #[test]
    fn critical_http_counts_as_warning_plus() {
        let config = Config {
            n_plus_one_http_warning_max: 0,
            ..Config::default()
        };
        let findings = vec![make_finding(FindingType::NPlusOneHttp, Severity::Critical)];
        let summary = empty_green_summary();
        let gate = evaluate(&findings, &summary, &config);

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
