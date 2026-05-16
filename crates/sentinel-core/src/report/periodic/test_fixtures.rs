//! Shared test fixtures for `report::periodic` tests. Factored out to
//! avoid duplicated builders flagged by static analysis.

#![cfg(test)]

use chrono::{DateTime, NaiveDate, Utc};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use super::schema::{
    AntiPatternDetail, Application, ApplicationG1, CalibrationInputs, Confidentiality, Conformance,
    Integrity, IntegrityLevel, Methodology, Notes, OrgIdentifiers, Organisation, Period,
    PeriodType, PeriodicReport, ReportIntent, ReportMetadata, SCHEMA_VERSION, ScopeManifest,
    core_patterns_required,
};

pub(super) fn parse_rfc3339(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("test fixture timestamp")
        .with_timezone(&Utc)
}

pub(super) fn sample_metadata(
    intent: ReportIntent,
    confidentiality: Confidentiality,
) -> ReportMetadata {
    ReportMetadata {
        intent,
        confidentiality_level: confidentiality,
        integrity_level: IntegrityLevel::HashOnly,
        generated_at: parse_rfc3339("2026-04-01T00:00:00Z"),
        generated_by: "cli-batch".to_string(),
        perf_sentinel_version: "0.6.2".to_string(),
        report_uuid: Uuid::nil(),
        binary_version: String::new(),
    }
}

pub(super) fn sample_g1_application() -> ApplicationG1 {
    ApplicationG1 {
        service_name: "svc".to_string(),
        display_name: None,
        service_version: None,
        endpoints_observed: 1,
        total_requests: 1000,
        energy_kwh: 0.5,
        carbon_kgco2eq: 0.05,
        efficiency_score: 90.0,
        anti_patterns: vec![AntiPatternDetail {
            kind: "n_plus_one_sql".to_string(),
            occurrences: 3,
            estimated_waste_kwh: 0.02,
            estimated_waste_kgco2eq: 0.003,
            first_seen: parse_rfc3339("2026-01-05T00:00:00Z"),
            last_seen: parse_rfc3339("2026-03-20T00:00:00Z"),
        }],
    }
}

pub(super) fn sample_organisation() -> Organisation {
    Organisation {
        name: "Acme".to_string(),
        country: "FR".to_string(),
        identifiers: OrgIdentifiers::default(),
        sector: None,
    }
}

pub(super) fn sample_q1_period() -> Period {
    Period {
        from_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        to_date: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
        period_type: PeriodType::CalendarQuarter,
        days_covered: 90,
    }
}

pub(super) fn sample_calibration_inputs() -> CalibrationInputs {
    CalibrationInputs {
        cloud_regions: vec!["eu-west-3".to_string()],
        carbon_intensity_source: "electricity_maps".to_string(),
        specpower_table_version: "2026-04-24".to_string(),
        binary_specpower_vintage: Some(
            crate::score::cloud_energy::embedded_specpower_vintage().to_string(),
        ),
        scaphandre_used: false,
        calibration_applied: false,
        energy_source_models: BTreeSet::new(),
    }
}

pub(super) fn sample_methodology() -> Methodology {
    Methodology {
        sci_specification: "ISO/IEC 21031:2024".to_string(),
        perf_sentinel_version: "0.6.2".to_string(),
        enabled_patterns: vec![
            "n_plus_one_sql".to_string(),
            "n_plus_one_http".to_string(),
            "redundant_sql".to_string(),
            "redundant_http".to_string(),
        ],
        disabled_patterns: vec![],
        core_patterns_required: core_patterns_required(),
        conformance: Conformance::CoreRequired,
        calibration_inputs: sample_calibration_inputs(),
    }
}

pub(super) fn sample_scope_manifest() -> ScopeManifest {
    ScopeManifest {
        total_applications_declared: 1,
        applications_measured: 1,
        applications_excluded: vec![],
        environments_measured: vec!["prod".to_string()],
        environments_excluded: vec![],
        total_requests_in_period: None,
        requests_measured: 1000,
        coverage_percentage: None,
    }
}

pub(super) fn sample_integrity() -> Integrity {
    Integrity {
        content_hash: String::new(),
        binary_hash: None,
        binary_verification_url: None,
        trace_integrity_chain: serde_json::Value::Null,
        signature: None,
        binary_attestation: None,
    }
}

/// Build a publishable `PeriodicReport` with the canonical sample data
/// used by hash/validator tests. The aggregate is filled with realistic
/// non-zero values so `validate_official` accepts it.
pub(super) fn sample_report(
    intent: ReportIntent,
    confidentiality: Confidentiality,
    applications: Vec<Application>,
) -> PeriodicReport {
    use super::schema::Aggregate;
    PeriodicReport {
        schema_version: SCHEMA_VERSION.to_string(),
        report_metadata: sample_metadata(intent, confidentiality),
        organisation: sample_organisation(),
        period: sample_q1_period(),
        scope_manifest: sample_scope_manifest(),
        methodology: sample_methodology(),
        aggregate: Aggregate {
            total_requests: 1000,
            total_energy_kwh: 0.5,
            total_carbon_kgco2eq: 0.05,
            aggregate_efficiency_score: 90.0,
            aggregate_waste_ratio: 0.1,
            anti_patterns_detected_count: 3,
            estimated_optimization_potential_kgco2eq: 0.01,
            period_coverage: 1.0,
            binary_versions: BTreeSet::new(),
            runtime_windows_count: 0,
            fallback_windows_count: 0,
            per_service_energy_models: BTreeMap::new(),
            per_service_measured_ratio: BTreeMap::new(),
        },
        applications,
        integrity: sample_integrity(),
        notes: Notes::default(),
    }
}
