//! `perf-sentinel disclose` subcommand.
//!
//! Loads an org-config TOML, aggregates archived per-window `Report`
//! NDJSON files inside the requested period, applies the official-intent
//! validator when needed, computes the deterministic content hash, and
//! writes the resulting `perf-sentinel-report.json`.

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, Utc};
use sentinel_core::report::periodic::aggregator::{
    AggregateInputs, AntiPatternAccumulator, ServiceAccumulator, UNATTRIBUTED_SERVICE,
    aggregate_from_paths,
};
use sentinel_core::report::periodic::org_config::{self, OrgConfig};
use sentinel_core::report::periodic::schema::{
    AntiPatternDetail, Application, ApplicationG1, ApplicationG2, CalibrationInputs,
    Confidentiality, DisabledPattern, ExcludedApp, ExcludedEnv, Integrity, IntegrityLevel,
    Methodology, Notes, OrgIdentifiers, Organisation, Period, PeriodType, PeriodicReport,
    ReportIntent, ReportMetadata, SCHEMA_VERSION, ScopeManifest, core_patterns_required,
};
use sentinel_core::report::periodic::{
    MIN_PERIOD_COVERAGE_FOR_OFFICIAL, binary_hash, compute_content_hash, validate_official,
};
use sentinel_core::text_safety::sanitize_for_terminal;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ReportIntentCli {
    Internal,
    Official,
    Audited,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ConfidentialityCli {
    Internal,
    Public,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum PeriodTypeCli {
    #[value(name = "calendar-quarter")]
    CalendarQuarter,
    #[value(name = "calendar-month")]
    CalendarMonth,
    #[value(name = "calendar-year")]
    CalendarYear,
    Custom,
}

impl From<ReportIntentCli> for ReportIntent {
    fn from(value: ReportIntentCli) -> Self {
        match value {
            ReportIntentCli::Internal => Self::Internal,
            ReportIntentCli::Official => Self::Official,
            ReportIntentCli::Audited => Self::Audited,
        }
    }
}

impl From<ConfidentialityCli> for Confidentiality {
    fn from(value: ConfidentialityCli) -> Self {
        match value {
            ConfidentialityCli::Internal => Self::Internal,
            ConfidentialityCli::Public => Self::Public,
        }
    }
}

impl From<PeriodTypeCli> for PeriodType {
    fn from(value: PeriodTypeCli) -> Self {
        match value {
            PeriodTypeCli::CalendarQuarter => Self::CalendarQuarter,
            PeriodTypeCli::CalendarMonth => Self::CalendarMonth,
            PeriodTypeCli::CalendarYear => Self::CalendarYear,
            PeriodTypeCli::Custom => Self::Custom,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_disclose(
    intent: ReportIntentCli,
    confidentiality: ConfidentialityCli,
    period_type: PeriodTypeCli,
    from: NaiveDate,
    to: NaiveDate,
    input: &[PathBuf],
    output: &Path,
    org_config_path: &Path,
    strict_attribution: bool,
    emit_attestation: Option<&Path>,
) -> i32 {
    if matches!(intent, ReportIntentCli::Audited) {
        eprintln!(
            "Error: audited intent is reserved for a future release, use 'internal' or 'official' instead"
        );
        return 2;
    }

    let org = match org_config::load_from_path(org_config_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Error: {}", sanitize_for_terminal(&err.to_string()));
            return 1;
        }
    };

    let days_covered = match (to - from).num_days() {
        n if n < 0 => {
            eprintln!("Error: to_date precedes from_date");
            return 2;
        }
        n => u32::try_from(n).map_or(u32::MAX, |d| d.saturating_add(1)),
    };

    let period = Period {
        from_date: from,
        to_date: to,
        period_type: period_type.into(),
        days_covered,
    };

    let aggregate = match aggregate_from_paths(input, &period, strict_attribution) {
        Ok(a) => a,
        Err(err) => {
            eprintln!("Error: {}", sanitize_for_terminal(&err.to_string()));
            return 1;
        }
    };

    let intent_schema: ReportIntent = intent.into();
    let confidentiality_schema: Confidentiality = confidentiality.into();
    let generated_by = if std::env::var("CI").is_ok_and(|v| !v.is_empty()) {
        "ci".to_string()
    } else {
        "cli-batch".to_string()
    };

    let windows = aggregate.windows_aggregated;
    let mut report = build_report(
        &org,
        period,
        intent_schema,
        confidentiality_schema,
        generated_by,
        aggregate,
    );

    report.integrity.binary_hash = binary_hash().ok();
    report.report_metadata.integrity_level = IntegrityLevel::HashOnly;

    if matches!(intent_schema, ReportIntent::Official)
        && let Err(errors) = validate_official(&report)
    {
        eprintln!("Error: report validation failed");
        for e in &errors {
            eprintln!("  - {}", sanitize_for_terminal(&e.to_string()));
        }
        return 2;
    }

    match compute_content_hash(&report) {
        Ok(hash) => {
            report.integrity.content_hash = hash;
        }
        Err(err) => {
            eprintln!("Error: failed to hash report: {err}");
            return 1;
        }
    }

    if let Err(err) = write_pretty_json(&report, output) {
        eprintln!("Error: failed to write {}: {err}", output.display());
        return 1;
    }

    if let Some(att_path) = emit_attestation {
        let subject_name = output
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("perf-sentinel-report.json");
        if let Err(err) = write_attestation(&report, output, att_path, subject_name) {
            eprintln!(
                "Error: failed to write attestation {}: {err}",
                att_path.display()
            );
            return 1;
        }
        eprintln!("Wrote attestation {}", att_path.display());
    }

    eprintln!(
        "Wrote {} ({} windows aggregated, {} services)",
        output.display(),
        windows,
        report.applications.len()
    );
    0
}

fn write_attestation(
    report: &PeriodicReport,
    report_path: &Path,
    attestation_path: &Path,
    subject_name: &str,
) -> std::io::Result<()> {
    use sentinel_core::report::periodic::attestation::build_in_toto_statement_named;
    use sentinel_core::report::periodic::compute_file_sha256_hex;

    // Refuse to truncate a symlink, same posture as write_pretty_json.
    if let Ok(meta) = std::fs::symlink_metadata(attestation_path)
        && meta.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "attestation output {} is a symlink, refusing to overwrite",
                attestation_path.display()
            ),
        ));
    }
    let digest = compute_file_sha256_hex(report_path)?;
    let statement = build_in_toto_statement_named(report, &digest, subject_name);
    // Compact single-line JSON matches the `.intoto.jsonl` convention
    // (one self-contained JSON value per line) used by cosign tooling,
    // with a trailing newline so concatenating multiple statements
    // stays valid JSONL.
    let mut json = serde_json::to_string(&statement)
        .map_err(|e| std::io::Error::other(format!("serialise attestation: {e}")))?;
    json.push('\n');
    std::fs::write(attestation_path, json)
}

fn build_report(
    org: &OrgConfig,
    period: Period,
    intent: ReportIntent,
    confidentiality: Confidentiality,
    generated_by: String,
    aggregate: AggregateInputs,
) -> PeriodicReport {
    let methodology = Methodology {
        sci_specification: org.methodology.sci_specification.clone(),
        perf_sentinel_version: env!("CARGO_PKG_VERSION").to_string(),
        enabled_patterns: org.methodology.enabled_patterns.clone(),
        disabled_patterns: org
            .methodology
            .disabled_patterns
            .iter()
            .map(|d| DisabledPattern {
                name: d.name.clone(),
                reason: d.reason.clone(),
            })
            .collect(),
        core_patterns_required: core_patterns_required(),
        conformance: org.methodology.conformance,
        calibration_inputs: CalibrationInputs {
            cloud_regions: org.methodology.calibration.cloud_regions.clone(),
            carbon_intensity_source: org.methodology.calibration.carbon_intensity_source.clone(),
            specpower_table_version: org.methodology.calibration.specpower_table_version.clone(),
            binary_specpower_vintage: Some(
                sentinel_core::score::cloud_energy::embedded_specpower_vintage().to_string(),
            ),
            scaphandre_used: org.methodology.calibration.scaphandre_used,
            energy_source_models: aggregate.energy_source_models.clone(),
            calibration_applied: aggregate.calibration_applied,
        },
    };

    let measured_services_count = aggregate
        .per_service
        .keys()
        .filter(|k| k.as_str() != UNATTRIBUTED_SERVICE)
        .count();
    let scope_manifest = ScopeManifest {
        total_applications_declared: org.scope_manifest.total_applications_declared,
        applications_measured: u32::try_from(measured_services_count).unwrap_or(u32::MAX),
        applications_excluded: org
            .scope_manifest
            .applications_excluded
            .iter()
            .map(|a| ExcludedApp {
                service_name: a.service_name.clone(),
                reason: a.reason.clone(),
            })
            .collect(),
        environments_measured: org.scope_manifest.environments_measured.clone(),
        environments_excluded: org
            .scope_manifest
            .environments_excluded
            .iter()
            .map(|e| ExcludedEnv {
                name: e.name.clone(),
                reason: e.reason.clone(),
            })
            .collect(),
        total_requests_in_period: org.scope_manifest.total_requests_in_period,
        requests_measured: aggregate.aggregate.total_requests,
        coverage_percentage: org.scope_manifest.total_requests_in_period.map(|total| {
            if total == 0 {
                0.0
            } else {
                100.0 * (aggregate.aggregate.total_requests as f64) / (total as f64)
            }
        }),
    };

    let applications = build_applications(
        &aggregate.per_service,
        &aggregate.first_seen,
        &aggregate.last_seen,
        confidentiality,
    );

    let base_disclaimers = if org.notes.disclaimers.is_empty() {
        default_disclaimers()
    } else {
        org.notes.disclaimers.clone()
    };
    let disclaimers = augment_disclaimers_for_coverage(
        base_disclaimers,
        intent,
        aggregate.aggregate.period_coverage,
    );
    let disclaimers =
        augment_disclaimers_for_binary_versions(disclaimers, &aggregate.aggregate.binary_versions);
    let disclaimers =
        augment_disclaimers_for_calibration(disclaimers, aggregate.calibration_applied);

    PeriodicReport {
        schema_version: SCHEMA_VERSION.to_string(),
        report_metadata: ReportMetadata {
            intent,
            confidentiality_level: confidentiality,
            integrity_level: IntegrityLevel::None,
            generated_at: Utc::now(),
            generated_by,
            perf_sentinel_version: env!("CARGO_PKG_VERSION").to_string(),
            report_uuid: Uuid::new_v4(),
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        organisation: Organisation {
            name: org.organisation.name.clone(),
            country: org.organisation.country.clone(),
            identifiers: OrgIdentifiers {
                siren: org.organisation.identifiers.siren.clone(),
                vat: org.organisation.identifiers.vat.clone(),
                lei: org.organisation.identifiers.lei.clone(),
                opencorporates_url: org.organisation.identifiers.opencorporates_url.clone(),
                domain: org.organisation.identifiers.domain.clone(),
            },
            sector: org.organisation.sector.clone(),
        },
        period,
        scope_manifest,
        methodology,
        aggregate: aggregate.aggregate,
        applications,
        integrity: Integrity {
            content_hash: String::new(),
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: None,
            binary_attestation: None,
        },
        notes: Notes {
            disclaimers,
            reference_urls: org.notes.reference_urls.clone(),
        },
    }
}

/// Append a disclaimer when the period had at least one window with
/// operator-supplied calibration coefficients applied.
fn augment_disclaimers_for_calibration(
    mut disclaimers: Vec<String>,
    calibration_applied: bool,
) -> Vec<String> {
    if calibration_applied {
        disclaimers.push(
            "Calibration applied: per-service energy coefficients from the operator \
             calibration file were used for at least one scoring window in this period. \
             Inspect methodology.calibration_inputs.calibration_applied for the binary fact."
                .to_string(),
        );
    }
    disclaimers
}

/// Append a disclaimer when the period spans more than one
/// perf-sentinel binary version. Single-version periods emit nothing.
fn augment_disclaimers_for_binary_versions(
    mut disclaimers: Vec<String>,
    binary_versions: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    if binary_versions.len() > 1 {
        let list = binary_versions
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        disclaimers.push(format!(
            "This period spans multiple perf-sentinel binary versions ({list}). \
             Verify version compatibility if comparing this report against \
             historical baselines."
        ));
    }
    disclaimers
}

/// Append the runtime-calibration coverage disclaimer when an internal
/// report falls below the official-grade threshold. Official reports are
/// rejected by `validate_official` upstream, so they never reach this
/// branch.
fn augment_disclaimers_for_coverage(
    mut disclaimers: Vec<String>,
    intent: ReportIntent,
    period_coverage: f64,
) -> Vec<String> {
    if matches!(intent, ReportIntent::Internal)
        && period_coverage < MIN_PERIOD_COVERAGE_FOR_OFFICIAL
    {
        disclaimers.push(format!(
            "Runtime-calibration coverage for this period is {:.1}%, below the \
             {:.0}% threshold. Aggregate energy and per-service attribution rely \
             on proxy fallback for the remaining windows. Not suitable for \
             official disclosure.",
            period_coverage * 100.0,
            MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0,
        ));
    }
    disclaimers
}

fn build_applications(
    per_service: &BTreeMap<String, ServiceAccumulator>,
    first_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    last_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    confidentiality: Confidentiality,
) -> Vec<Application> {
    let mut out = Vec::with_capacity(per_service.len());
    for (service, accum) in per_service {
        // The `_unattributed` bucket contributes to aggregate totals
        // but is not a "measured application" in the wire output.
        // Keeping it would desync applications_measured from applications.len().
        if service == UNATTRIBUTED_SERVICE {
            continue;
        }
        let avoidable: u64 = accum
            .anti_patterns
            .values()
            .map(|ap| ap.avoidable_io_ops)
            .sum();
        let any_anti_pattern: u64 = accum.anti_patterns.values().map(|ap| ap.occurrences).sum();
        let efficiency_score = if accum.total_io_ops == 0 {
            // Zero I/O recorded but findings present: cannot publish 100%.
            if any_anti_pattern == 0 { 100.0 } else { 0.0 }
        } else {
            // Efficiency = 100 - 100 * avoidable / total_io_ops (clamped).
            (100.0 - 100.0 * (avoidable as f64) / (accum.total_io_ops as f64)).clamp(0.0, 100.0)
        };
        let endpoints_observed = u32::try_from(accum.endpoints_seen.len()).unwrap_or(u32::MAX);
        match confidentiality {
            Confidentiality::Internal => out.push(Application::G1(ApplicationG1 {
                service_name: service.clone(),
                display_name: None,
                service_version: None,
                endpoints_observed,
                total_requests: accum.total_requests,
                energy_kwh: accum.energy_kwh,
                carbon_kgco2eq: accum.carbon_kgco2eq,
                efficiency_score,
                anti_patterns: build_anti_pattern_details(
                    service,
                    &accum.anti_patterns,
                    first_seen,
                    last_seen,
                    service_carbon_ratio(accum),
                ),
            })),
            Confidentiality::Public => {
                let count: u64 = accum.anti_patterns.values().map(|ap| ap.occurrences).sum();
                out.push(Application::G2(ApplicationG2 {
                    service_name: service.clone(),
                    display_name: None,
                    service_version: None,
                    endpoints_observed,
                    total_requests: accum.total_requests,
                    energy_kwh: accum.energy_kwh,
                    carbon_kgco2eq: accum.carbon_kgco2eq,
                    efficiency_score,
                    anti_patterns_detected_count: count,
                }));
            }
        }
    }
    out
}

fn service_carbon_ratio(accum: &ServiceAccumulator) -> f64 {
    if accum.energy_kwh > 0.0 {
        accum.carbon_kgco2eq / accum.energy_kwh
    } else {
        0.0
    }
}

fn build_anti_pattern_details(
    service: &str,
    anti_patterns: &BTreeMap<String, AntiPatternAccumulator>,
    first_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    last_seen: &BTreeMap<(String, String), chrono::DateTime<Utc>>,
    service_carbon_kwh_ratio: f64,
) -> Vec<AntiPatternDetail> {
    // Proxy coefficient lifted from the carbon module so the per-pattern
    // waste line up with the aggregate proxy energy. Region-blind, see
    // design doc 08.
    const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1;
    let now = Utc::now();
    let mut out = Vec::with_capacity(anti_patterns.len());
    for (pattern, accum) in anti_patterns {
        let key = (service.to_string(), pattern.clone());
        let first = first_seen.get(&key).copied().unwrap_or(now);
        let last = last_seen.get(&key).copied().unwrap_or(now);
        let waste_kwh = (accum.avoidable_io_ops as f64) * ENERGY_PER_IO_OP_KWH;
        let waste_kgco2eq = waste_kwh * service_carbon_kwh_ratio;
        out.push(AntiPatternDetail {
            kind: pattern.clone(),
            occurrences: accum.occurrences,
            estimated_waste_kwh: waste_kwh,
            estimated_waste_kgco2eq: waste_kgco2eq,
            first_seen: first,
            last_seen: last,
        });
    }
    out
}

fn default_disclaimers() -> Vec<String> {
    vec![
        "Directional estimate, not regulatory-grade.".to_string(),
        "Approximate uncertainty bracket: ~2x multiplicative.".to_string(),
        "Optimization potential excludes embodied hardware emissions (SCI M term).".to_string(),
        "Per-service carbon includes operational emissions only; embodied carbon (SCI M term) is reported in the aggregate total but not attributed per service.".to_string(),
        "Energy and carbon attribution per service is runtime-calibrated when the window's energy_model is non-empty; archives written before this feature shipped fall back to proportional I/O share.".to_string(),
        "Not suitable for CSRD or GHG Protocol Scope 3 reporting.".to_string(),
        "Methodology: ISO/IEC 21031:2024 (SCI).".to_string(),
    ]
}

fn write_pretty_json(report: &PeriodicReport, output: &Path) -> std::io::Result<()> {
    // Refuse to truncate a symlink. Residual TOCTOU between the check
    // and the open is accepted given the CLI is operator-driven.
    if let Ok(meta) = std::fs::symlink_metadata(output)
        && meta.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "output {} is a symlink; refusing to overwrite",
                output.display()
            ),
        ));
    }
    let file = std::fs::File::create(output)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, report)?;
    use std::io::Write as _;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_disclaimer_added_for_internal_below_threshold() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(base, ReportIntent::Internal, 0.5);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("50.0%"));
        let threshold_text = format!("{:.0}%", MIN_PERIOD_COVERAGE_FOR_OFFICIAL * 100.0);
        assert!(out[1].contains(&threshold_text));
        assert!(out[1].contains("Not suitable for official disclosure"));
    }

    #[test]
    fn coverage_disclaimer_omitted_for_internal_at_full_coverage() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(base.clone(), ReportIntent::Internal, 1.0);
        assert_eq!(out, base);
    }

    #[test]
    fn coverage_disclaimer_omitted_for_internal_exactly_at_threshold() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(
            base.clone(),
            ReportIntent::Internal,
            MIN_PERIOD_COVERAGE_FOR_OFFICIAL,
        );
        assert_eq!(out, base);
    }

    #[test]
    fn coverage_disclaimer_omitted_for_official_intent() {
        // Official below threshold is refused by the validator upstream,
        // but if we ever build the report (e.g. validator bypassed), this
        // branch must not add the internal-only disclaimer.
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_coverage(base.clone(), ReportIntent::Official, 0.5);
        assert_eq!(out, base);
    }

    #[test]
    fn binary_versions_disclaimer_omitted_for_single_version() {
        let base = vec!["existing".to_string()];
        let mut versions = std::collections::BTreeSet::new();
        versions.insert("0.6.2".to_string());
        let out = augment_disclaimers_for_binary_versions(base.clone(), &versions);
        assert_eq!(out, base);
    }

    #[test]
    fn binary_versions_disclaimer_omitted_for_empty_set() {
        let base = vec!["existing".to_string()];
        let versions = std::collections::BTreeSet::new();
        let out = augment_disclaimers_for_binary_versions(base.clone(), &versions);
        assert_eq!(out, base);
    }

    #[test]
    fn calibration_disclaimer_omitted_when_not_applied() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_calibration(base.clone(), false);
        assert_eq!(out, base);
    }

    #[test]
    fn calibration_disclaimer_added_when_applied() {
        let base = vec!["existing".to_string()];
        let out = augment_disclaimers_for_calibration(base, true);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("Calibration applied"));
        assert!(out[1].contains("calibration_inputs.calibration_applied"));
    }

    #[test]
    fn binary_versions_disclaimer_added_for_multiple_versions() {
        let base = vec!["existing".to_string()];
        let mut versions = std::collections::BTreeSet::new();
        versions.insert("0.6.2".to_string());
        versions.insert("0.6.3".to_string());
        let out = augment_disclaimers_for_binary_versions(base, &versions);
        assert_eq!(out.len(), 2);
        assert!(out[1].contains("0.6.2"));
        assert!(out[1].contains("0.6.3"));
        assert!(out[1].contains("multiple perf-sentinel binary versions"));
    }

    #[test]
    fn emit_attestation_produces_statement_with_matching_digest() {
        use sentinel_core::report::periodic::attestation::{
            IN_TOTO_STATEMENT_TYPE, InTotoStatement, PERF_SENTINEL_PREDICATE_TYPE,
        };
        use sentinel_core::report::periodic::compute_file_sha256_hex;

        let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("docs/schemas/examples/example-official-public-G2.json");
        let report: PeriodicReport =
            serde_json::from_str(&std::fs::read_to_string(&example).unwrap()).unwrap();

        let tmp = std::env::temp_dir().join(format!(
            "perf-sentinel-attestation-test-{}.json",
            std::process::id()
        ));
        let att = tmp.with_extension("intoto.jsonl");
        std::fs::write(&tmp, std::fs::read(&example).unwrap()).unwrap();

        write_attestation(&report, &tmp, &att, "subject.json").expect("write attestation");

        let statement_json = std::fs::read_to_string(&att).unwrap();
        let statement: InTotoStatement = serde_json::from_str(&statement_json).unwrap();
        assert_eq!(statement.statement_type, IN_TOTO_STATEMENT_TYPE);
        assert_eq!(statement.predicate_type, PERF_SENTINEL_PREDICATE_TYPE);
        assert_eq!(statement.subject[0].name, "subject.json");
        let expected_digest = compute_file_sha256_hex(&tmp).unwrap();
        assert_eq!(
            statement.subject[0].digest.get("sha256").unwrap(),
            &expected_digest
        );

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&att);
    }
}
