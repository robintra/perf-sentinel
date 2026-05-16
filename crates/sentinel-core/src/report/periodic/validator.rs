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

/// Minimum runtime-calibration ratio for an `official` intent report.
/// Reports below this threshold are likely produced during a daemon
/// migration or with partial Scaphandre coverage; publishing them as
/// `official` would silently understate or distort the period total.
pub const MIN_PERIOD_COVERAGE_FOR_OFFICIAL: f64 = 0.75;

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
    validate_enabled_patterns(meth, errors);
    validate_core_patterns(meth, errors);
    validate_disabled_patterns(meth, errors);
    validate_conformance(meth, errors);
    validate_calibration_inputs(meth, errors);
}

fn validate_enabled_patterns(meth: &Methodology, errors: &mut Vec<ValidationError>) {
    for name in &meth.enabled_patterns {
        if !KNOWN_PATTERNS.contains(&name.as_str()) {
            errors.push(ValidationError::Methodology {
                field: "enabled_patterns",
                reason: format!("unknown pattern {name:?}"),
            });
        }
    }
}

fn validate_core_patterns(meth: &Methodology, errors: &mut Vec<ValidationError>) {
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
    // The declared core set must match `core_patterns_required()` exactly.
    // Extra entries or reordering would otherwise pass the per-name checks
    // above without invalidating the canonical-set claim.
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
}

fn validate_disabled_patterns(meth: &Methodology, errors: &mut Vec<ValidationError>) {
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
}

fn validate_conformance(meth: &Methodology, errors: &mut Vec<ValidationError>) {
    // `partial` is informational only, not a publishable conformance level.
    if matches!(meth.conformance, Conformance::Partial) {
        errors.push(ValidationError::Methodology {
            field: "conformance",
            reason: "official disclosure requires conformance \"core-required\" or \"extended\""
                .to_string(),
        });
    }
}

fn validate_calibration_inputs(meth: &Methodology, errors: &mut Vec<ValidationError>) {
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
    let declared_vintage = meth.calibration_inputs.specpower_table_version.trim();
    if declared_vintage.is_empty() {
        errors.push(ValidationError::Methodology {
            field: "calibration_inputs.specpower_table_version",
            reason: "must not be empty".to_string(),
        });
    } else if let Some(binary_vintage) = meth.calibration_inputs.binary_specpower_vintage.as_deref()
    {
        // For Official intent, the operator-declared vintage must equal
        // the first whitespace-delimited token of the binary vintage.
        // The binary may carry an annotated suffix (e.g.
        // "2026-04-24 (CCF aligned)") but the operator declares the bare
        // date string. Substring match would accept "2026" or "CCF" and
        // miss the drift the audit is meant to catch.
        let binary_date_prefix = binary_vintage.split_whitespace().next().unwrap_or("");
        if declared_vintage != binary_date_prefix {
            errors.push(ValidationError::Methodology {
                field: "calibration_inputs.specpower_table_version",
                reason: format!(
                    "declared vintage {declared_vintage:?} does not match the running binary {binary_vintage:?}; update the org config to align, or downgrade intent to 'internal' for this report window"
                ),
            });
        }
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
    if !agg.period_coverage.is_finite() || !(0.0..=1.0).contains(&agg.period_coverage) {
        errors.push(ValidationError::Aggregate {
            field: "period_coverage",
            reason: format!("must be in [0, 1], got {}", agg.period_coverage),
        });
    } else if agg.period_coverage < MIN_PERIOD_COVERAGE_FOR_OFFICIAL {
        errors.push(ValidationError::Aggregate {
            field: "period_coverage",
            reason: format!(
                "{:.1}% is below the {:.0}% threshold required for official intent. \
                 Likely cause: daemon migration mid-period or partial Scaphandre \
                 coverage. Either regenerate the report over a shorter period that \
                 excludes non-calibrated windows, or set intent=internal.",
                agg.period_coverage * 100.0,
                MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0,
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
        Application, ApplicationG2, Confidentiality, DisabledPattern, ExcludedApp, PeriodicReport,
        ReportIntent,
    };
    use crate::report::periodic::test_fixtures;

    fn good_report(intent: ReportIntent, confidentiality: Confidentiality) -> PeriodicReport {
        let applications = match confidentiality {
            Confidentiality::Internal => {
                vec![Application::G1(test_fixtures::sample_g1_application())]
            }
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
        };
        let mut r = test_fixtures::sample_report(intent, confidentiality, applications);
        // The validator-specific report keeps a wider set of enabled
        // patterns and a populated coverage_percentage so each rejection
        // path has a non-empty starting point to mutate.
        r.methodology.enabled_patterns = vec![
            "n_plus_one_sql".to_string(),
            "n_plus_one_http".to_string(),
            "redundant_sql".to_string(),
            "redundant_http".to_string(),
            "slow_sql".to_string(),
        ];
        r.scope_manifest.coverage_percentage = Some(100.0);
        r
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
    fn declared_specpower_vintage_drift_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.methodology.calibration_inputs.specpower_table_version = "2023-05-01".to_string();
        r.methodology.calibration_inputs.binary_specpower_vintage =
            Some("2026-04-24 (CCF aligned)".to_string());
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors.iter().any(
                |e| matches!(e, ValidationError::Methodology { field, reason }
                    if *field == "calibration_inputs.specpower_table_version"
                        && reason.contains("does not match the running binary"))
            ),
            "got {errors:?}"
        );
    }

    #[test]
    fn declared_specpower_vintage_date_prefix_match_accepted() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.methodology.calibration_inputs.specpower_table_version = "2026-04-24".to_string();
        r.methodology.calibration_inputs.binary_specpower_vintage =
            Some("2026-04-24 (CCF aligned)".to_string());
        // Operator declares the bare date prefix; the binary annotates it.
        validate_official(&r).expect("date prefix vintage must validate");
    }

    #[test]
    fn declared_specpower_vintage_trivial_substring_rejected() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        // Operator declares a permissive substring that the previous
        // contains-based rule would have accepted ("2026" sits inside
        // "2026-04-24"). Prefix exact match must reject it as drift.
        r.methodology.calibration_inputs.specpower_table_version = "2026".to_string();
        r.methodology.calibration_inputs.binary_specpower_vintage =
            Some("2026-04-24 (CCF aligned)".to_string());
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Methodology { field, .. }
                    if *field == "calibration_inputs.specpower_table_version")),
            "trivial substring should be rejected, got {errors:?}"
        );
    }

    #[test]
    fn declared_specpower_vintage_without_binary_field_accepted() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        // When the binary vintage field is absent (older binary or
        // intentionally skipped), the drift rule cannot fire and the
        // operator's declaration is accepted as-is.
        r.methodology.calibration_inputs.specpower_table_version = "anything-2099".to_string();
        r.methodology.calibration_inputs.binary_specpower_vintage = None;
        validate_official(&r).expect("absent binary vintage must not gate the validator");
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
    fn period_coverage_below_threshold_rejected_for_official() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.aggregate.period_coverage = 0.6;
        let errors = validate_official(&r).unwrap_err();
        let threshold_text = format!("{:.0}%", MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0);
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Aggregate { field, reason }
                    if *field == "period_coverage" && reason.contains(&threshold_text))),
            "got {errors:?}"
        );
    }

    #[test]
    fn period_coverage_at_threshold_accepted_for_official() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.aggregate.period_coverage = MIN_PERIOD_COVERAGE_FOR_OFFICIAL;
        validate_official(&r).expect("0.75 should clear the official gate");
    }

    #[test]
    fn period_coverage_out_of_range_rejected_for_official() {
        let mut r = good_report(ReportIntent::Official, Confidentiality::Internal);
        r.aggregate.period_coverage = 1.5;
        let errors = validate_official(&r).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::Aggregate { field, reason }
                    if *field == "period_coverage" && reason.contains("[0, 1]"))),
            "got {errors:?}"
        );
    }

    #[test]
    fn period_coverage_below_threshold_skipped_for_internal() {
        let mut r = good_report(ReportIntent::Internal, Confidentiality::Internal);
        r.aggregate.period_coverage = 0.2;
        validate_official(&r).expect("internal intent must not gate on coverage");
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
