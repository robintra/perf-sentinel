//! Wire schema (v1.0) for the periodic disclosure report.
//! See `docs/design/08-PERIODIC-DISCLOSURE.md` for ordering and
//! determinism invariants that any change here must preserve.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "perf-sentinel-report/v1.0";

/// Patterns that an `intent = "official"` disclosure must keep enabled.
/// Aligned with `FindingType::is_avoidable_io()`: the four patterns whose
/// remediation directly reduces I/O and therefore energy/carbon.
pub const CORE_PATTERNS_REQUIRED: &[&str] = &[
    "n_plus_one_sql",
    "n_plus_one_http",
    "redundant_sql",
    "redundant_http",
];

#[must_use]
pub fn core_patterns_required() -> Vec<String> {
    CORE_PATTERNS_REQUIRED
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodicReport {
    pub schema_version: String,
    pub report_metadata: ReportMetadata,
    pub organisation: Organisation,
    pub period: Period,
    pub scope_manifest: ScopeManifest,
    pub methodology: Methodology,
    pub aggregate: Aggregate,
    pub applications: Vec<Application>,
    pub integrity: Integrity,
    pub notes: Notes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportIntent {
    Internal,
    Official,
    Audited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidentiality {
    Internal,
    Public,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrityLevel {
    None,
    HashOnly,
    Signed,
    Audited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeriodType {
    CalendarQuarter,
    CalendarMonth,
    CalendarYear,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Conformance {
    CoreRequired,
    Extended,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMetadata {
    pub intent: ReportIntent,
    pub confidentiality_level: Confidentiality,
    pub integrity_level: IntegrityLevel,
    pub generated_at: DateTime<Utc>,
    pub generated_by: String,
    pub perf_sentinel_version: String,
    pub report_uuid: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organisation {
    pub name: String,
    pub country: String,
    #[serde(default)]
    pub identifiers: OrgIdentifiers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sector: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrgIdentifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub siren: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lei: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencorporates_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Period {
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub period_type: PeriodType,
    pub days_covered: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeManifest {
    pub total_applications_declared: u32,
    pub applications_measured: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications_excluded: Vec<ExcludedApp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments_measured: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments_excluded: Vec<ExcludedEnv>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_requests_in_period: Option<u64>,
    pub requests_measured: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_percentage: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedApp {
    pub service_name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedEnv {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Methodology {
    pub sci_specification: String,
    pub perf_sentinel_version: String,
    pub enabled_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_patterns: Vec<DisabledPattern>,
    pub core_patterns_required: Vec<String>,
    pub conformance: Conformance,
    pub calibration_inputs: CalibrationInputs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledPattern {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationInputs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cloud_regions: Vec<String>,
    pub carbon_intensity_source: String,
    pub specpower_table_version: String,
    pub scaphandre_used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aggregate {
    pub total_requests: u64,
    pub total_energy_kwh: f64,
    pub total_carbon_kgco2eq: f64,
    pub aggregate_efficiency_score: f64,
    pub aggregate_waste_ratio: f64,
    pub anti_patterns_detected_count: u64,
    pub estimated_optimization_potential_kgco2eq: f64,
}

/// G1 carries per-anti-pattern detail, G2 carries only a count.
/// Homogeneous per disclosure, see design doc 08.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Application {
    G1(ApplicationG1),
    G2(ApplicationG2),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationG1 {
    pub service_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    pub endpoints_observed: u32,
    pub total_requests: u64,
    pub energy_kwh: f64,
    pub carbon_kgco2eq: f64,
    pub efficiency_score: f64,
    pub anti_patterns: Vec<AntiPatternDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationG2 {
    pub service_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    pub endpoints_observed: u32,
    pub total_requests: u64,
    pub energy_kwh: f64,
    pub carbon_kgco2eq: f64,
    pub efficiency_score: f64,
    pub anti_patterns_detected_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntiPatternDetail {
    #[serde(rename = "type")]
    pub kind: String,
    pub occurrences: u64,
    pub estimated_waste_kwh: f64,
    pub estimated_waste_kgco2eq: f64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Integrity {
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_verification_url: Option<String>,
    #[serde(default)]
    pub trace_integrity_chain: serde_json::Value,
    #[serde(default)]
    pub signature: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disclaimers: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reference_urls: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metadata() -> ReportMetadata {
        ReportMetadata {
            intent: ReportIntent::Internal,
            confidentiality_level: Confidentiality::Internal,
            integrity_level: IntegrityLevel::HashOnly,
            generated_at: DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            generated_by: "cli-batch".to_string(),
            perf_sentinel_version: "0.6.2".to_string(),
            report_uuid: Uuid::nil(),
        }
    }

    fn sample_organisation() -> Organisation {
        Organisation {
            name: "Acme Corp".to_string(),
            country: "FR".to_string(),
            identifiers: OrgIdentifiers {
                siren: Some("123456789".to_string()),
                ..Default::default()
            },
            sector: Some("62.01".to_string()),
        }
    }

    fn sample_period() -> Period {
        Period {
            from_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            to_date: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            period_type: PeriodType::CalendarQuarter,
            days_covered: 90,
        }
    }

    fn sample_scope() -> ScopeManifest {
        ScopeManifest {
            total_applications_declared: 5,
            applications_measured: 4,
            applications_excluded: vec![ExcludedApp {
                service_name: "legacy-batch".to_string(),
                reason: "instrumentation pending".to_string(),
            }],
            environments_measured: vec!["prod".to_string()],
            environments_excluded: vec![],
            total_requests_in_period: Some(1_000_000),
            requests_measured: 980_000,
            coverage_percentage: Some(98.0),
        }
    }

    fn sample_methodology() -> Methodology {
        Methodology {
            sci_specification: "ISO/IEC 21031:2024".to_string(),
            perf_sentinel_version: "0.6.2".to_string(),
            enabled_patterns: vec!["n_plus_one_sql".to_string(), "slow_sql".to_string()],
            disabled_patterns: vec![],
            core_patterns_required: core_patterns_required(),
            conformance: Conformance::CoreRequired,
            calibration_inputs: CalibrationInputs {
                cloud_regions: vec!["eu-west-3".to_string()],
                carbon_intensity_source: "electricity_maps".to_string(),
                specpower_table_version: "2024-2026".to_string(),
                scaphandre_used: false,
            },
        }
    }

    fn sample_aggregate() -> Aggregate {
        Aggregate {
            total_requests: 980_000,
            total_energy_kwh: 12.5,
            total_carbon_kgco2eq: 1.4,
            aggregate_efficiency_score: 82.0,
            aggregate_waste_ratio: 0.18,
            anti_patterns_detected_count: 47,
            estimated_optimization_potential_kgco2eq: 0.25,
        }
    }

    fn sample_integrity() -> Integrity {
        Integrity {
            content_hash: "sha256:".to_string()
                + "0000000000000000000000000000000000000000000000000000000000000000",
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: serde_json::Value::Null,
        }
    }

    fn sample_notes() -> Notes {
        let mut urls = BTreeMap::new();
        urls.insert(
            "project".to_string(),
            "https://github.com/robintra/perf-sentinel".to_string(),
        );
        Notes {
            disclaimers: vec!["Directional estimate, not regulatory-grade".to_string()],
            reference_urls: urls,
        }
    }

    fn sample_g1_app() -> ApplicationG1 {
        ApplicationG1 {
            service_name: "checkout".to_string(),
            display_name: Some("Checkout".to_string()),
            service_version: Some("v1.4.2".to_string()),
            endpoints_observed: 12,
            total_requests: 240_000,
            energy_kwh: 4.1,
            carbon_kgco2eq: 0.46,
            efficiency_score: 78.0,
            anti_patterns: vec![AntiPatternDetail {
                kind: "n_plus_one_sql".to_string(),
                occurrences: 12,
                estimated_waste_kwh: 0.05,
                estimated_waste_kgco2eq: 0.006,
                first_seen: DateTime::parse_from_rfc3339("2026-01-04T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                last_seen: DateTime::parse_from_rfc3339("2026-03-29T18:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            }],
        }
    }

    fn sample_g2_app() -> ApplicationG2 {
        ApplicationG2 {
            service_name: "checkout".to_string(),
            display_name: Some("Checkout".to_string()),
            service_version: None,
            endpoints_observed: 12,
            total_requests: 240_000,
            energy_kwh: 4.1,
            carbon_kgco2eq: 0.46,
            efficiency_score: 78.0,
            anti_patterns_detected_count: 12,
        }
    }

    fn sample_report(applications: Vec<Application>) -> PeriodicReport {
        PeriodicReport {
            schema_version: SCHEMA_VERSION.to_string(),
            report_metadata: sample_metadata(),
            organisation: sample_organisation(),
            period: sample_period(),
            scope_manifest: sample_scope(),
            methodology: sample_methodology(),
            aggregate: sample_aggregate(),
            applications,
            integrity: sample_integrity(),
            notes: sample_notes(),
        }
    }

    #[test]
    fn roundtrip_v1_minimal() {
        let r = sample_report(vec![]);
        let json = serde_json::to_string(&r).unwrap();
        let back: PeriodicReport = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2);
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert!(back.applications.is_empty());
    }

    #[test]
    fn roundtrip_v1_full_g1() {
        let r = sample_report(vec![Application::G1(sample_g1_app())]);
        let json = serde_json::to_string(&r).unwrap();
        let back: PeriodicReport = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.applications[0], Application::G1(_)));
        let Application::G1(ref app) = back.applications[0] else {
            unreachable!()
        };
        assert_eq!(app.anti_patterns.len(), 1);
        assert_eq!(app.anti_patterns[0].kind, "n_plus_one_sql");
    }

    #[test]
    fn roundtrip_v1_full_g2() {
        let r = sample_report(vec![Application::G2(sample_g2_app())]);
        let json = serde_json::to_string(&r).unwrap();
        let back: PeriodicReport = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.applications[0], Application::G2(_)));
        let Application::G2(ref app) = back.applications[0] else {
            unreachable!()
        };
        assert_eq!(app.anti_patterns_detected_count, 12);
    }

    #[test]
    fn application_g1_disambiguates_from_g2() {
        // The untagged enum discriminates by required-field presence.
        let g1 = serde_json::json!({
            "service_name": "svc",
            "endpoints_observed": 1,
            "total_requests": 10,
            "energy_kwh": 0.1,
            "carbon_kgco2eq": 0.01,
            "efficiency_score": 90.0,
            "anti_patterns": []
        });
        let g2 = serde_json::json!({
            "service_name": "svc",
            "endpoints_observed": 1,
            "total_requests": 10,
            "energy_kwh": 0.1,
            "carbon_kgco2eq": 0.01,
            "efficiency_score": 90.0,
            "anti_patterns_detected_count": 0
        });
        assert!(matches!(
            serde_json::from_value::<Application>(g1).unwrap(),
            Application::G1(_)
        ));
        assert!(matches!(
            serde_json::from_value::<Application>(g2).unwrap(),
            Application::G2(_)
        ));
    }

    #[test]
    fn core_patterns_required_matches_constant() {
        let v = core_patterns_required();
        assert_eq!(v.len(), CORE_PATTERNS_REQUIRED.len());
        assert!(v.contains(&"n_plus_one_sql".to_string()));
        assert!(v.contains(&"n_plus_one_http".to_string()));
        assert!(v.contains(&"redundant_sql".to_string()));
        assert!(v.contains(&"redundant_http".to_string()));
    }

    #[test]
    fn enum_serialization_uses_kebab_or_snake() {
        let v = serde_json::to_string(&PeriodType::CalendarQuarter).unwrap();
        assert_eq!(v, "\"calendar-quarter\"");
        let v = serde_json::to_string(&IntegrityLevel::HashOnly).unwrap();
        assert_eq!(v, "\"hash-only\"");
        let v = serde_json::to_string(&ReportIntent::Official).unwrap();
        assert_eq!(v, "\"official\"");
    }

    #[test]
    fn unknown_top_level_fields_tolerated() {
        // Forward-compat: a future version may add fields that this build
        // does not know about. Deserialisation must not fail.
        let mut v = serde_json::to_value(sample_report(vec![])).unwrap();
        v.as_object_mut()
            .unwrap()
            .insert("future_field".to_string(), serde_json::json!("ignore me"));
        let _: PeriodicReport = serde_json::from_value(v).unwrap();
    }
}
