//! Collect-all validator for an `intent = "official"`
//! [`PeriodicReport`]. `internal` is a no-op, `audited` short-circuits.
//! See `docs/design/08-PERIODIC-DISCLOSURE.md` for the rule list and
//! the rationale for collecting every error in one pass.

use super::errors::ValidationError;
use super::hasher::compute_content_hash;
use super::schema::{
    Aggregate, Application, ApplicationG1, ApplicationG2, Confidentiality, Conformance,
    Methodology, Organisation, Period, PeriodicReport, ReportIntent, ScopeManifest,
};

/// All pattern names known to perf-sentinel, kept in sync with
/// `FindingType::as_str()`. A schema test asserts the length matches the
/// number of variants in `crate::detect::FindingType`.
const KNOWN_PATTERNS: &[&str] = &[
    "n_plus_one_sql",
    "n_plus_one_http",
    "redundant_sql",
    "redundant_http",
    "slow_sql",
    "slow_http",
    "excessive_fanout",
    "chatty_service",
    "pool_saturation",
    "serialized_calls",
];

/// Values accepted for `methodology.calibration_inputs.carbon_intensity_source`.
const CARBON_SOURCE_VALUES: &[&str] = &["electricity_maps", "static_tables", "mixed"];

/// Validate a report for the given intent.
///
/// `internal` always returns `Ok(())`.
/// `audited` returns `Err(vec![AuditedNotImplemented])`.
/// `official` collects every rule violation it can find.
///
/// # Errors
///
/// Returns the full list of [`ValidationError`] entries when validation
/// fails. The list is never empty when this function returns `Err`.
pub fn validate_official(report: &PeriodicReport) -> Result<(), Vec<ValidationError>> {
    match report.report_metadata.intent {
        ReportIntent::Internal => return Ok(()),
        ReportIntent::Audited => return Err(vec![ValidationError::AuditedNotImplemented]),
        ReportIntent::Official => {}
    }

    let mut errors = Vec::new();
    validate_organisation(&report.organisation, &mut errors);
    validate_period(&report.period, &mut errors);
    validate_scope_manifest(&report.scope_manifest, &mut errors);
    validate_methodology(&report.methodology, &mut errors);
    validate_aggregate(&report.aggregate, &mut errors);
    validate_applications(
        &report.applications,
        report.report_metadata.confidentiality_level,
        &mut errors,
    );
    validate_scope_application_consistency(
        &report.scope_manifest,
        &report.applications,
        &mut errors,
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Verify that `integrity.content_hash` matches a freshly computed hash.
///
/// # Errors
///
/// Returns [`ValidationError::HashMismatch`] when the stored hash differs
/// from the recomputed value, or [`ValidationError::Aggregate`] (re-used
/// as a generic carrier) if the report cannot be serialised for hashing.
pub fn validate_content_hash(report: &PeriodicReport) -> Result<(), ValidationError> {
    let expected = compute_content_hash(report).map_err(|e| ValidationError::Aggregate {
        field: "content_hash",
        reason: e.to_string(),
    })?;
    if expected == report.integrity.content_hash {
        Ok(())
    } else {
        Err(ValidationError::HashMismatch {
            expected,
            actual: report.integrity.content_hash.clone(),
        })
    }
}

fn validate_organisation(org: &Organisation, errors: &mut Vec<ValidationError>) {
    if org.name.trim().is_empty() {
        errors.push(ValidationError::Organisation {
            field: "name",
            reason: "must not be empty".to_string(),
        });
    }
    if !is_iso_3166_alpha2(&org.country) {
        errors.push(ValidationError::Organisation {
            field: "country",
            reason: format!(
                "must be a 2-letter ISO 3166-1 alpha-2 code in upper case, got {:?}",
                org.country
            ),
        });
    }
}

fn is_iso_3166_alpha2(s: &str) -> bool {
    s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase())
}

fn validate_period(period: &Period, errors: &mut Vec<ValidationError>) {
    if period.to_date < period.from_date {
        errors.push(ValidationError::Period(format!(
            "to_date {} precedes from_date {}",
            period.to_date, period.from_date
        )));
    }
    if period.days_covered < 30 {
        errors.push(ValidationError::Period(format!(
            "days_covered must be >= 30 for an official disclosure, got {}",
            period.days_covered
        )));
    }
}

fn validate_scope_manifest(scope: &ScopeManifest, errors: &mut Vec<ValidationError>) {
    for (idx, excluded) in scope.applications_excluded.iter().enumerate() {
        if excluded.reason.trim().is_empty() {
            errors.push(ValidationError::ScopeManifest {
                field: "applications_excluded",
                reason: format!("entry {idx} has empty reason"),
            });
        }
    }
    if scope.applications_measured > scope.total_applications_declared {
        errors.push(ValidationError::ScopeManifest {
            field: "applications_measured",
            reason: format!(
                "{} measured exceeds {} declared",
                scope.applications_measured, scope.total_applications_declared
            ),
        });
    }
    if let Some(pct) = scope.coverage_percentage
        && (!(0.0..=100.0).contains(&pct) || !pct.is_finite())
    {
        errors.push(ValidationError::ScopeManifest {
            field: "coverage_percentage",
            reason: format!("must be a finite value in [0, 100], got {pct}"),
        });
    }
}

fn validate_methodology(meth: &Methodology, errors: &mut Vec<ValidationError>) {
    for name in &meth.enabled_patterns {
        if !KNOWN_PATTERNS.contains(&name.as_str()) {
            errors.push(ValidationError::Methodology {
                field: "enabled_patterns",
                reason: format!("unknown pattern {name:?}"),
            });
        }
    }
    for name in &meth.core_patterns_required {
        if !KNOWN_PATTERNS.contains(&name.as_str()) {
            errors.push(ValidationError::Methodology {
                field: "core_patterns_required",
                reason: format!("unknown pattern {name:?}"),
            });
        }
        if !meth.enabled_patterns.contains(name) {
            errors.push(ValidationError::Methodology {
                field: "core_patterns_required",
                reason: format!("core pattern {name:?} is not in enabled_patterns"),
            });
        }
    }
    for disabled in &meth.disabled_patterns {
        if !KNOWN_PATTERNS.contains(&disabled.name.as_str()) {
            errors.push(ValidationError::Methodology {
                field: "disabled_patterns",
                reason: format!("unknown pattern {:?}", disabled.name),
            });
        }
        if meth.core_patterns_required.contains(&disabled.name) {
            errors.push(ValidationError::Methodology {
                field: "disabled_patterns",
                reason: format!(
                    "pattern {:?} is core-required and cannot be disabled",
                    disabled.name
                ),
            });
        }
    }
    // Conformance must be at least `core-required` for a publishable
    // disclosure. `partial` is informational only.
    if matches!(meth.conformance, Conformance::Partial) {
        errors.push(ValidationError::Methodology {
            field: "conformance",
            reason: "official disclosure requires conformance \"core-required\" or \"extended\""
                .to_string(),
        });
    }
    // core_patterns_required must exactly match the canonical set
    // returned by `core_patterns_required()`. The validator already
    // rejects missing entries via the "must be in enabled_patterns"
    // path, but a divergent vector (extra entries, reordering) would
    // pass that check unnoticed.
    let canonical = super::schema::core_patterns_required();
    for declared in &meth.core_patterns_required {
        if !canonical.contains(declared) {
            errors.push(ValidationError::Methodology {
                field: "core_patterns_required",
                reason: format!("{declared:?} is not in the canonical core set"),
            });
        }
    }
    for canon in &canonical {
        if !meth.core_patterns_required.contains(canon) {
            errors.push(ValidationError::Methodology {
                field: "core_patterns_required",
                reason: format!("canonical core pattern {canon:?} is missing"),
            });
        }
    }
    let src = meth.calibration_inputs.carbon_intensity_source.trim();
    if src.is_empty() {
        errors.push(ValidationError::Methodology {
            field: "calibration_inputs.carbon_intensity_source",
            reason: "must not be empty".to_string(),
        });
    } else if !CARBON_SOURCE_VALUES.contains(&src) {
        errors.push(ValidationError::Methodology {
            field: "calibration_inputs.carbon_intensity_source",
            reason: format!(
                "must be one of {CARBON_SOURCE_VALUES:?}, got {:?}",
                meth.calibration_inputs.carbon_intensity_source
            ),
        });
    }
    if meth
        .calibration_inputs
        .specpower_table_version
        .trim()
        .is_empty()
    {
        errors.push(ValidationError::Methodology {
            field: "calibration_inputs.specpower_table_version",
            reason: "must not be empty".to_string(),
        });
    }
}

fn validate_aggregate(agg: &Aggregate, errors: &mut Vec<ValidationError>) {
    let checks: [(&'static str, f64); 5] = [
        ("total_energy_kwh", agg.total_energy_kwh),
        ("total_carbon_kgco2eq", agg.total_carbon_kgco2eq),
        ("aggregate_efficiency_score", agg.aggregate_efficiency_score),
        ("aggregate_waste_ratio", agg.aggregate_waste_ratio),
        (
            "estimated_optimization_potential_kgco2eq",
            agg.estimated_optimization_potential_kgco2eq,
        ),
    ];
    let mut tainted: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for (field, value) in checks {
        if !value.is_finite() {
            errors.push(ValidationError::Aggregate {
                field,
                reason: format!("must be a finite number, got {value}"),
            });
            tainted.insert(field);
        } else if value < 0.0 {
            errors.push(ValidationError::Aggregate {
                field,
                reason: format!("must be >= 0, got {value}"),
            });
            tainted.insert(field);
        }
    }
    // Bounded-range checks skipped when the field already failed the
    // finite + non-negative gate, otherwise a single bad value emits
    // two errors with conflicting bounds.
    if !tainted.contains("aggregate_waste_ratio")
        && !(0.0..=1.0).contains(&agg.aggregate_waste_ratio)
    {
        errors.push(ValidationError::Aggregate {
            field: "aggregate_waste_ratio",
            reason: format!("must be in [0, 1], got {}", agg.aggregate_waste_ratio),
        });
    }
    if !tainted.contains("aggregate_efficiency_score")
        && !(0.0..=100.0).contains(&agg.aggregate_efficiency_score)
    {
        errors.push(ValidationError::Aggregate {
            field: "aggregate_efficiency_score",
            reason: format!(
                "must be in [0, 100], got {}",
                agg.aggregate_efficiency_score
            ),
        });
    }
}

fn validate_scope_application_consistency(
    scope: &ScopeManifest,
    apps: &[Application],
    errors: &mut Vec<ValidationError>,
) {
    if !apps.is_empty() && scope.applications_measured == 0 {
        errors.push(ValidationError::ScopeManifest {
            field: "applications_measured",
            reason: format!(
                "must be > 0 when applications carries {} entries",
                apps.len()
            ),
        });
    }
}

fn validate_applications(
    apps: &[Application],
    confidentiality: Confidentiality,
    errors: &mut Vec<ValidationError>,
) {
    if apps.is_empty() {
        errors.push(ValidationError::Applications(
            "must list at least one measured application".to_string(),
        ));
        return;
    }

    let first_is_g1 = matches!(apps[0], Application::G1(_));
    let homogeneous = apps
        .iter()
        .all(|a| matches!(a, Application::G1(_)) == first_is_g1);
    if !homogeneous {
        errors.push(ValidationError::Applications(
            "all entries must share the same granularity (G1 or G2)".to_string(),
        ));
    }

    if confidentiality == Confidentiality::Public && first_is_g1 {
        errors.push(ValidationError::Applications(
            "confidentiality=public requires G2 granularity (no per-anti-pattern detail)"
                .to_string(),
        ));
    }

    for (idx, app) in apps.iter().enumerate() {
        match app {
            Application::G1(g1) => validate_app_g1(idx, g1, errors),
            Application::G2(g2) => validate_app_g2(idx, g2, errors),
        }
    }
}

fn validate_app_g1(idx: usize, app: &ApplicationG1, errors: &mut Vec<ValidationError>) {
    if app.service_name.trim().is_empty() {
        errors.push(ValidationError::Applications(format!(
            "applications[{idx}].service_name must not be empty"
        )));
    }
    for (i, ap) in app.anti_patterns.iter().enumerate() {
        if !KNOWN_PATTERNS.contains(&ap.kind.as_str()) {
            errors.push(ValidationError::Applications(format!(
                "applications[{idx}].anti_patterns[{i}].type: unknown pattern {:?}",
                ap.kind
            )));
        }
        if ap.last_seen < ap.first_seen {
            errors.push(ValidationError::Applications(format!(
                "applications[{idx}].anti_patterns[{i}]: last_seen precedes first_seen"
            )));
        }
    }
}

fn validate_app_g2(idx: usize, app: &ApplicationG2, errors: &mut Vec<ValidationError>) {
    if app.service_name.trim().is_empty() {
        errors.push(ValidationError::Applications(format!(
            "applications[{idx}].service_name must not be empty"
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::FindingType;
    use crate::report::periodic::schema::{
        Aggregate, AntiPatternDetail, Application, ApplicationG1, ApplicationG2, CalibrationInputs,
        Confidentiality, Conformance, DisabledPattern, ExcludedApp, Integrity, IntegrityLevel,
        Methodology, Notes, OrgIdentifiers, Organisation, Period, PeriodType, PeriodicReport,
        ReportIntent, ReportMetadata, SCHEMA_VERSION, ScopeManifest, core_patterns_required,
    };
    use chrono::{DateTime, NaiveDate, Utc};
    use serde_json::Value;
    use uuid::Uuid;

    #[allow(clippy::too_many_lines)]
    fn good_report(intent: ReportIntent, confidentiality: Confidentiality) -> PeriodicReport {
        PeriodicReport {
            schema_version: SCHEMA_VERSION.to_string(),
            report_metadata: ReportMetadata {
                intent,
                confidentiality_level: confidentiality,
                integrity_level: IntegrityLevel::HashOnly,
                generated_at: DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                generated_by: "cli-batch".to_string(),
                perf_sentinel_version: "0.6.2".to_string(),
                report_uuid: Uuid::nil(),
            },
            organisation: Organisation {
                name: "Acme".to_string(),
                country: "FR".to_string(),
                identifiers: OrgIdentifiers::default(),
                sector: None,
            },
            period: Period {
                from_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                to_date: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
                period_type: PeriodType::CalendarQuarter,
                days_covered: 90,
            },
            scope_manifest: ScopeManifest {
                total_applications_declared: 1,
                applications_measured: 1,
                applications_excluded: vec![],
                environments_measured: vec!["prod".to_string()],
                environments_excluded: vec![],
                total_requests_in_period: None,
                requests_measured: 1000,
                coverage_percentage: Some(100.0),
            },
            methodology: Methodology {
                sci_specification: "ISO/IEC 21031:2024".to_string(),
                perf_sentinel_version: "0.6.2".to_string(),
                enabled_patterns: vec![
                    "n_plus_one_sql".to_string(),
                    "n_plus_one_http".to_string(),
                    "redundant_sql".to_string(),
                    "redundant_http".to_string(),
                    "slow_sql".to_string(),
                ],
                disabled_patterns: vec![],
                core_patterns_required: core_patterns_required(),
                conformance: Conformance::CoreRequired,
                calibration_inputs: CalibrationInputs {
                    cloud_regions: vec!["eu-west-3".to_string()],
                    carbon_intensity_source: "electricity_maps".to_string(),
                    specpower_table_version: "2024-2026".to_string(),
                    scaphandre_used: false,
                },
            },
            aggregate: Aggregate {
                total_requests: 1000,
                total_energy_kwh: 0.5,
                total_carbon_kgco2eq: 0.05,
                aggregate_efficiency_score: 90.0,
                aggregate_waste_ratio: 0.1,
                anti_patterns_detected_count: 3,
                estimated_optimization_potential_kgco2eq: 0.01,
            },
            applications: match confidentiality {
                Confidentiality::Internal => vec![Application::G1(ApplicationG1 {
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
                        first_seen: DateTime::parse_from_rfc3339("2026-01-05T00:00:00Z")
                            .unwrap()
                            .with_timezone(&Utc),
                        last_seen: DateTime::parse_from_rfc3339("2026-03-20T00:00:00Z")
                            .unwrap()
                            .with_timezone(&Utc),
                    }],
                })],
                Confidentiality::Public => vec![Application::G2(ApplicationG2 {
                    service_name: "svc".to_string(),
                    display_name: None,
                    service_version: None,
                    endpoints_observed: 1,
                    total_requests: 1000,
                    energy_kwh: 0.5,
                    carbon_kgco2eq: 0.05,
                    efficiency_score: 90.0,
                    anti_patterns_detected_count: 3,
                })],
            },
            integrity: Integrity {
                content_hash: String::new(),
                binary_hash: None,
                binary_verification_url: None,
                trace_integrity_chain: Value::Null,
                signature: Value::Null,
            },
            notes: Notes::default(),
        }
    }

    #[test]
    fn internal_intent_always_ok() {
        let mut r = good_report(ReportIntent::Internal, Confidentiality::Internal);
        r.organisation.name = String::new();
        r.organisation.country = "fr".to_string();
        r.period.days_covered = 1;
        r.applications.clear();
        assert!(validate_official(&r).is_ok());
    }

    #[test]
    fn audited_intent_short_circuits() {
        let r = good_report(ReportIntent::Audited, Confidentiality::Internal);
        let errors = validate_official(&r).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], ValidationError::AuditedNotImplemented));
    }

    #[test]
    fn official_happy_path_internal_confidentiality() {
        let r = good_report(ReportIntent::Official, Confidentiality::Internal);
        validate_official(&r).expect("happy path should pass");
    }

    #[test]
    fn official_happy_path_public_confidentiality() {
        let r = good_report(ReportIntent::Official, Confidentiality::Public);
        validate_official(&r).expect("happy path should pass");
    }

    #[test]
    fn official_collects_all_errors() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.organisation.name = String::new();
        r.organisation.country = "fr".to_string();
        r.period.days_covered = 10;
        r.applications.clear();
        let errors = validate_official(&r).unwrap_err();
        // Expect at least: organisation.name, organisation.country, period, applications.
        assert!(errors.len() >= 4, "expected >= 4 errors, got {errors:?}");
    }

    #[test]
    fn unknown_pattern_in_enabled_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.methodology
            .enabled_patterns
            .push("n_plus_one_redis".to_string());
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors.iter().any(
                |e| matches!(e, ValidationError::Methodology { field, reason }
                    if *field == "enabled_patterns" && reason.contains("n_plus_one_redis"))
            ),
            "got {errors:?}"
        );
    }

    #[test]
    fn core_pattern_disabled_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.methodology.disabled_patterns.push(DisabledPattern {
            name: "n_plus_one_sql".to_string(),
            reason: "noisy".to_string(),
        });
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Methodology { field, .. }
                    if *field == "disabled_patterns")),
            "got {errors:?}"
        );
    }

    #[test]
    fn core_pattern_missing_from_enabled_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.methodology
            .enabled_patterns
            .retain(|p| p != "n_plus_one_sql");
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Methodology { field, .. }
                    if *field == "core_patterns_required")),
            "got {errors:?}"
        );
    }

    #[test]
    fn unknown_carbon_source_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.methodology.calibration_inputs.carbon_intensity_source = "guesswork".to_string();
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Methodology { field, .. }
                    if *field == "calibration_inputs.carbon_intensity_source")),
            "got {errors:?}"
        );
    }

    #[test]
    fn heterogeneous_applications_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.applications.push(Application::G2(ApplicationG2 {
            service_name: "other".to_string(),
            display_name: None,
            service_version: None,
            endpoints_observed: 1,
            total_requests: 1,
            energy_kwh: 0.0,
            carbon_kgco2eq: 0.0,
            efficiency_score: 100.0,
            anti_patterns_detected_count: 0,
        }));
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Applications(msg)
                    if msg.contains("same granularity"))),
            "got {errors:?}"
        );
    }

    #[test]
    fn public_with_g1_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Public);
        r.applications = vec![Application::G1(ApplicationG1 {
            service_name: "svc".to_string(),
            display_name: None,
            service_version: None,
            endpoints_observed: 1,
            total_requests: 1,
            energy_kwh: 0.0,
            carbon_kgco2eq: 0.0,
            efficiency_score: 100.0,
            anti_patterns: vec![],
        })];
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Applications(msg)
                if msg.contains("G2 granularity"))),
            "got {errors:?}"
        );
    }

    #[test]
    fn period_under_30_days_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.period.days_covered = 10;
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Period(_))),
            "got {errors:?}"
        );
    }

    #[test]
    fn waste_ratio_out_of_range_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.aggregate.aggregate_waste_ratio = 1.5;
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Aggregate { field, .. }
                    if *field == "aggregate_waste_ratio")),
            "got {errors:?}"
        );
    }

    #[test]
    fn applications_excluded_without_reason_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.scope_manifest.applications_excluded.push(ExcludedApp {
            service_name: "legacy".to_string(),
            reason: String::new(),
        });
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::ScopeManifest { field, .. }
                    if *field == "applications_excluded")),
            "got {errors:?}"
        );
    }

    #[test]
    fn validate_content_hash_round_trip() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        let hash = compute_content_hash(&r).unwrap();
        r.integrity.content_hash = hash;
        validate_content_hash(&r).expect("recomputed hash should match");
    }

    #[test]
    fn validate_content_hash_detects_tamper() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.integrity.content_hash = compute_content_hash(&r).unwrap();
        r.aggregate.total_energy_kwh += 1.0;
        let err = validate_content_hash(&r).unwrap_err();
        assert!(matches!(err, ValidationError::HashMismatch { .. }));
    }

    #[test]
    fn known_patterns_matches_finding_type_count() {
        // Match-exhaustiveness ensures any future FindingType variant
        // forces this test to be updated alongside KNOWN_PATTERNS.
        let count = match FindingType::NPlusOneSql {
            FindingType::NPlusOneSql
            | FindingType::NPlusOneHttp
            | FindingType::RedundantSql
            | FindingType::RedundantHttp
            | FindingType::SlowSql
            | FindingType::SlowHttp
            | FindingType::ExcessiveFanout
            | FindingType::ChattyService
            | FindingType::PoolSaturation
            | FindingType::SerializedCalls => 10,
        };
        assert_eq!(count, KNOWN_PATTERNS.len());
    }
}
