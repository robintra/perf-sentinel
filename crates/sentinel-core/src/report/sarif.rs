//! SARIF v2.1.0 report export.
//!
//! Generates a [SARIF](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html)
//! report from perf-sentinel findings. Uses logical locations (service + endpoint)
//! since perf-sentinel analyzes traces, not source code.

use serde::Serialize;

use crate::detect::{Finding, FindingType, Severity};
use crate::report::Report;

// ── SARIF v2.1.0 structs ───────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLog {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub version: String,
    pub runs: Vec<SarifRun>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRun {
    pub tool: SarifTool,
    pub results: Vec<SarifResult>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifTool {
    pub driver: SarifDriver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifDriver {
    pub name: String,
    pub version: String,
    pub information_uri: String,
    pub rules: Vec<SarifRule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRule {
    pub id: String,
    pub short_description: SarifMessage,
    pub default_configuration: SarifDefaultConfiguration,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifDefaultConfiguration {
    pub level: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifResult {
    pub rule_id: String,
    pub level: String,
    pub message: SarifMessage,
    pub logical_locations: Vec<SarifLogicalLocation>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifMessage {
    pub text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLogicalLocation {
    pub name: String,
    pub kind: String,
}

// ── Conversion ──────────────────────────────────────────────────────

fn severity_to_sarif_level(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

fn finding_type_description(ft: &FindingType) -> &'static str {
    match ft {
        FindingType::NPlusOneSql => "N+1 SQL query pattern detected",
        FindingType::NPlusOneHttp => "N+1 HTTP call pattern detected",
        FindingType::RedundantSql => "Redundant SQL query detected",
        FindingType::RedundantHttp => "Redundant HTTP call detected",
        FindingType::SlowSql => "Slow SQL query pattern detected",
        FindingType::SlowHttp => "Slow HTTP call pattern detected",
        FindingType::ExcessiveFanout => "Excessive span fanout detected",
    }
}

fn build_rules() -> Vec<SarifRule> {
    let variants = [
        FindingType::NPlusOneSql,
        FindingType::NPlusOneHttp,
        FindingType::RedundantSql,
        FindingType::RedundantHttp,
        FindingType::SlowSql,
        FindingType::SlowHttp,
        FindingType::ExcessiveFanout,
    ];
    variants
        .iter()
        .map(|ft| SarifRule {
            id: ft.as_str().to_string(),
            short_description: SarifMessage {
                text: finding_type_description(ft).to_string(),
            },
            default_configuration: SarifDefaultConfiguration {
                level: "warning".to_string(),
            },
        })
        .collect()
}

fn finding_to_result(finding: &Finding) -> SarifResult {
    let message = format!(
        "{} in {} on {}: {} ({} occurrences, {}ms window). {}",
        finding.finding_type.as_str(),
        finding.service,
        finding.source_endpoint,
        finding.pattern.template,
        finding.pattern.occurrences,
        finding.pattern.window_ms,
        finding.suggestion
    );

    SarifResult {
        rule_id: finding.finding_type.as_str().to_string(),
        level: severity_to_sarif_level(&finding.severity).to_string(),
        message: SarifMessage { text: message },
        logical_locations: vec![
            SarifLogicalLocation {
                name: finding.service.clone(),
                kind: "module".to_string(),
            },
            SarifLogicalLocation {
                name: finding.source_endpoint.clone(),
                kind: "function".to_string(),
            },
        ],
    }
}

const SARIF_SCHEMA: &str = "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json";

/// Convert a perf-sentinel Report to a SARIF log.
#[must_use]
pub fn report_to_sarif(report: &Report) -> SarifLog {
    SarifLog {
        schema: SARIF_SCHEMA.to_string(),
        version: "2.1.0".to_string(),
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "perf-sentinel".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    information_uri: "https://github.com/robintra/perf-sentinel".to_string(),
                    rules: build_rules(),
                },
            },
            results: report.findings.iter().map(finding_to_result).collect(),
        }],
    }
}

/// Emit a Report as SARIF JSON to stdout.
///
/// # Errors
///
/// Returns an error if JSON serialization or stdout write fails.
pub fn emit_sarif(report: &Report) -> Result<(), SarifError> {
    use std::io::Write;
    let sarif = report_to_sarif(report);
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, &sarif)?;
    let _ = writeln!(lock);
    Ok(())
}

/// Errors from SARIF emission.
#[derive(Debug, thiserror::Error)]
pub enum SarifError {
    /// JSON serialization failed.
    #[error("SARIF serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{GreenImpact, Pattern};

    fn make_finding(ft: FindingType, sev: Severity) -> Finding {
        Finding {
            finding_type: ft,
            severity: sev,
            trace_id: "trace-1".to_string(),
            service: "game".to_string(),
            source_endpoint: "POST /api/game/42/start".to_string(),
            pattern: Pattern {
                template: "SELECT * FROM player WHERE game_id = ?".to_string(),
                occurrences: 6,
                window_ms: 250,
                distinct_params: 6,
            },
            suggestion: "Use WHERE ... IN (?)".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: Some(GreenImpact {
                estimated_extra_io_ops: 5,
                io_intensity_score: 6.0,
            }),
        }
    }

    #[test]
    fn sarif_version_is_2_1_0() {
        let report = Report {
            analysis: crate::report::Analysis {
                duration_ms: 1,
                events_processed: 6,
                traces_analyzed: 1,
            },
            findings: vec![],
            green_summary: crate::report::GreenSummary::disabled(0),
            quality_gate: crate::report::QualityGate {
                passed: true,
                rules: vec![],
            },
        };
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.version, "2.1.0");
        assert!(sarif.schema.contains("sarif-schema-2.1.0"));
    }

    #[test]
    fn sarif_has_all_rule_definitions() {
        let report = Report {
            analysis: crate::report::Analysis {
                duration_ms: 0,
                events_processed: 0,
                traces_analyzed: 0,
            },
            findings: vec![],
            green_summary: crate::report::GreenSummary::disabled(0),
            quality_gate: crate::report::QualityGate {
                passed: true,
                rules: vec![],
            },
        };
        let sarif = report_to_sarif(&report);
        let rules = &sarif.runs[0].tool.driver.rules;
        assert_eq!(rules.len(), 7);
        assert_eq!(rules[0].id, "n_plus_one_sql");
        assert_eq!(rules[6].id, "excessive_fanout");
    }

    #[test]
    fn sarif_maps_severity_to_level() {
        assert_eq!(severity_to_sarif_level(&Severity::Critical), "error");
        assert_eq!(severity_to_sarif_level(&Severity::Warning), "warning");
        assert_eq!(severity_to_sarif_level(&Severity::Info), "note");
    }

    #[test]
    fn sarif_result_has_logical_locations() {
        let finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let result = finding_to_result(&finding);
        assert_eq!(result.rule_id, "n_plus_one_sql");
        assert_eq!(result.level, "warning");
        assert_eq!(result.logical_locations.len(), 2);
        assert_eq!(result.logical_locations[0].name, "game");
        assert_eq!(result.logical_locations[0].kind, "module");
        assert_eq!(result.logical_locations[1].name, "POST /api/game/42/start");
        assert_eq!(result.logical_locations[1].kind, "function");
    }

    #[test]
    fn sarif_results_from_report() {
        let report = Report {
            analysis: crate::report::Analysis {
                duration_ms: 1,
                events_processed: 6,
                traces_analyzed: 1,
            },
            findings: vec![
                make_finding(FindingType::NPlusOneSql, Severity::Warning),
                make_finding(FindingType::NPlusOneHttp, Severity::Critical),
            ],
            green_summary: crate::report::GreenSummary::disabled(6),
            quality_gate: crate::report::QualityGate {
                passed: false,
                rules: vec![],
            },
        };
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.runs[0].results.len(), 2);
        assert_eq!(sarif.runs[0].results[0].level, "warning");
        assert_eq!(sarif.runs[0].results[1].level, "error");
    }
}
