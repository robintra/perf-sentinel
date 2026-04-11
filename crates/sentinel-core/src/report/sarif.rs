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
    /// SARIF v2.1.0 property bag. uses it to expose the
    /// tool-specific `confidence` field for perf-lint interop.
    /// Empty-by-default so pre-5b consumers are unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<SarifProperties>,
    /// SARIF v2.1.0 `rank` field (0-100). populates this from
    /// the finding's [`Confidence`] so SARIF consumers that don't read
    /// the custom `properties` bag still get a useful ordering signal.
    /// Mapping: `ci_batch = 30`, `daemon_staging = 60`,
    /// `daemon_production = 90`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
}

/// Custom properties attached to a [`SarifResult`].
///
/// perf-sentinel specific fields that don't fit the SARIF v2.1.0 schema
/// natively. perf-lint reads this bag to boost / reduce severity in the IDE
/// based on where the finding came from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifProperties {
    /// Source context of the finding: `"ci_batch"`, `"daemon_staging"`, or
    /// `"daemon_production"`. See [`Confidence`] for semantics.
    pub confidence: &'static str,
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
        FindingType::ChattyService => "Chatty service pattern detected",
        FindingType::PoolSaturation => "Connection pool saturation risk detected",
        FindingType::SerializedCalls => "Serialized-but-parallelizable calls detected",
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
        FindingType::ChattyService,
        FindingType::PoolSaturation,
        FindingType::SerializedCalls,
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
        // expose the confidence for perf-lint interop.
        properties: Some(SarifProperties {
            confidence: finding.confidence.as_str(),
        }),
        rank: Some(finding.confidence.sarif_rank()),
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
    use crate::detect::Confidence;

    fn make_report(findings: Vec<Finding>) -> Report {
        Report {
            analysis: crate::report::Analysis {
                duration_ms: 1,
                events_processed: 6,
                traces_analyzed: 1,
            },
            findings,
            green_summary: crate::report::GreenSummary::disabled(0),
            quality_gate: crate::report::QualityGate {
                passed: true,
                rules: vec![],
            },
        }
    }

    fn make_finding(ft: FindingType, sev: Severity) -> Finding {
        let mut f = crate::test_helpers::make_finding(ft, sev);
        f.pattern.template = "SELECT * FROM order_item WHERE order_id = ?".to_string();
        f.pattern.window_ms = 250;
        f.suggestion = "Use WHERE ... IN (?)".to_string();
        f
    }

    #[test]
    fn sarif_version_is_2_1_0() {
        let report = make_report(vec![]);
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.version, "2.1.0");
        assert!(sarif.schema.contains("sarif-schema-2.1.0"));
    }

    #[test]
    fn sarif_has_all_rule_definitions() {
        let report = make_report(vec![]);
        let sarif = report_to_sarif(&report);
        let rules = &sarif.runs[0].tool.driver.rules;
        assert_eq!(rules.len(), 10);
        assert_eq!(rules[0].id, "n_plus_one_sql");
        assert_eq!(rules[6].id, "excessive_fanout");
        assert_eq!(rules[7].id, "chatty_service");
        assert_eq!(rules[8].id, "pool_saturation");
        assert_eq!(rules[9].id, "serialized_calls");
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
        assert_eq!(result.logical_locations[0].name, "order-svc");
        assert_eq!(result.logical_locations[0].kind, "module");
        assert_eq!(
            result.logical_locations[1].name,
            "POST /api/orders/42/submit"
        );
        assert_eq!(result.logical_locations[1].kind, "function");
    }

    #[test]
    fn sarif_results_from_report() {
        let report = make_report(vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Critical),
        ]);
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.runs[0].results.len(), 2);
        assert_eq!(sarif.runs[0].results[0].level, "warning");
        assert_eq!(sarif.runs[0].results[1].level, "error");
    }

    #[test]
    fn sarif_empty_findings_produces_valid_json() {
        let report = make_report(vec![]);
        let sarif = report_to_sarif(&report);
        let json = serde_json::to_string(&sarif).unwrap();
        assert!(json.contains("\"version\":\"2.1.0\""));
        assert!(json.contains("\"results\":[]"));
    }

    #[test]
    fn sarif_special_chars_in_service_and_endpoint() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.service = "svc-with-\"quotes\"".to_string();
        finding.source_endpoint = "POST /api/items?a=1&b=<2>".to_string();
        let result = finding_to_result(&finding);
        // Should serialize without error
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#"svc-with-\"quotes\""#));
        assert!(json.contains("POST /api/items?a=1&b=<2>"));
    }

    #[test]
    fn sarif_finding_without_green_impact() {
        let mut finding = make_finding(FindingType::SlowSql, Severity::Warning);
        finding.green_impact = None;
        let result = finding_to_result(&finding);
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("slow_sql"));
    }

    // --- confidence exposure via properties + rank ---

    #[test]
    fn sarif_result_contains_ci_batch_confidence_by_default() {
        // Default finding has CiBatch confidence (detector emits it).
        let finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let result = finding_to_result(&finding);
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            json.contains(r#""confidence":"ci_batch""#),
            "SARIF should expose confidence in properties bag, got: {json}"
        );
        assert!(
            json.contains(r#""rank":30"#),
            "SARIF should populate rank=30 for CiBatch, got: {json}"
        );
    }

    #[test]
    fn sarif_result_daemon_staging_rank_60() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.confidence = Confidence::DaemonStaging;
        let result = finding_to_result(&finding);
        assert_eq!(result.rank, Some(60));
        let props = result.properties.as_ref().unwrap();
        assert_eq!(props.confidence, "daemon_staging");
    }

    #[test]
    fn sarif_result_daemon_production_rank_90() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Critical);
        finding.confidence = Confidence::DaemonProduction;
        let result = finding_to_result(&finding);
        assert_eq!(result.rank, Some(90));
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#""confidence":"daemon_production""#));
        assert!(json.contains(r#""rank":90"#));
    }

    #[test]
    fn sarif_log_round_trip_with_mixed_confidence() {
        // Full report → SARIF serialization should expose all three
        // confidence values across a mixed batch of findings.
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        f1.confidence = Confidence::CiBatch;
        let mut f2 = make_finding(FindingType::RedundantSql, Severity::Info);
        f2.confidence = Confidence::DaemonStaging;
        let mut f3 = make_finding(FindingType::ExcessiveFanout, Severity::Critical);
        f3.confidence = Confidence::DaemonProduction;
        let report = make_report(vec![f1, f2, f3]);
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.runs[0].results.len(), 3);
        assert_eq!(sarif.runs[0].results[0].rank, Some(30));
        assert_eq!(sarif.runs[0].results[1].rank, Some(60));
        assert_eq!(sarif.runs[0].results[2].rank, Some(90));
    }
}
