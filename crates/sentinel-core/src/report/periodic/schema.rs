//! Wire schema (v1.0) for the periodic disclosure report.
//! See `docs/design/08-PERIODIC-DISCLOSURE.md` for ordering and
//! determinism invariants that any change here must preserve.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    SignedWithAttestation,
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
    /// Binary that wrote the disclosure file. Mirrors the per-window
    /// `Report.binary_version` so disclosure files and source archives
    /// share the same indexing convention. Empty on files produced
    /// before this field shipped.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub binary_version: String,
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
    /// Energy source models observed in the archived windows for the
    /// period. Sourced from each window's `GreenSummary.energy_model`
    /// (e.g. `"scaphandre_rapl"`, `"cloud_specpower"`, `"io_proxy_v3"`)
    /// with the optional `+cal` suffix stripped. Empty when every
    /// archived window predates per-service carbon attribution.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub energy_source_models: BTreeSet<String>,
    /// `true` if at least one window in the period emitted an
    /// `energy_model` tag with the `+cal` suffix, meaning operator-supplied
    /// per-service calibration coefficients adjusted the proxy energy.
    /// The `+cal` suffix is stripped from `energy_source_models`, so this
    /// flag is the only place that surfaces the fact.
    #[serde(default)]
    pub calibration_applied: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Aggregate {
    pub total_requests: u64,
    pub total_energy_kwh: f64,
    pub total_carbon_kgco2eq: f64,
    pub aggregate_efficiency_score: f64,
    pub aggregate_waste_ratio: f64,
    pub anti_patterns_detected_count: u64,
    pub estimated_optimization_potential_kgco2eq: f64,
    /// Fraction of the period's scoring windows that used runtime-calibrated
    /// energy attribution. `1.0` when every window provided runtime energy,
    /// `0.0` when every window fell back to the proxy. Defined as
    /// `runtime_windows / (runtime_windows + fallback_windows)`, pinned to
    /// `1.0` when no windows were aggregated. The aggregator always emits
    /// the computed value, so `1.0` only surfaces through `serde(default)`
    /// when re-reading a periodic report that was produced before this
    /// field shipped. This default is permissive (it grants the maximum
    /// quality score) and is meant for type safety, not for assessing the
    /// quality of legacy reports.
    #[serde(default = "default_period_coverage")]
    pub period_coverage: f64,
    /// Set of distinct perf-sentinel binary versions observed across
    /// the period's windows. Empty if every window predates the field.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub binary_versions: BTreeSet<String>,
    /// Number of scoring windows in the period that used
    /// runtime-calibrated attribution.
    #[serde(default)]
    pub runtime_windows_count: u64,
    /// Number of scoring windows in the period that fell back to the
    /// proxy attribution path.
    #[serde(default)]
    pub fallback_windows_count: u64,
    /// Per-service set of distinct energy models observed over the
    /// period. The `+cal` suffix is stripped before insertion; see
    /// `calibration_inputs.calibration_applied` for the period-wide
    /// calibration flag. Empty for periods without per-service attribution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_energy_models: BTreeMap<String, BTreeSet<String>>,
    /// Per-service mean of the per-window `per_service_measured_ratio`
    /// across the period. Simple arithmetic mean (each window counts
    /// equally), not span-weighted: a 10-span window and a 10000-span
    /// window contribute the same weight. Read with
    /// `per_service_energy_models` to distinguish "service had a
    /// measured tag but only on 5% of its spans" from "service was
    /// fully measured". Empty for periods without per-service attribution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_measured_ratio: BTreeMap<String, f64>,
}

#[inline]
const fn default_period_coverage() -> f64 {
    1.0
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
    /// Sigstore cosign in-toto attestation metadata. The signature
    /// itself lives in a sidecar bundle file. This object carries
    /// the locator and identity facts a verifier needs. Serialised
    /// as `null` when absent so the canonical `content_hash` form
    /// stays stable for files predating this typed schema.
    #[serde(default)]
    pub signature: Option<SignatureMetadata>,
    /// SLSA build provenance attestation for the perf-sentinel binary
    /// that produced this report. Skipped from the output when absent
    /// so files predating the field retain their canonical form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_attestation: Option<BinaryAttestationMetadata>,
}

/// Sigstore cosign + in-toto signature locator. Format strings are
/// documented in `docs/design/10-SIGSTORE-ATTESTATION.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureMetadata {
    /// Format identifier, currently `"sigstore-cosign-intoto-v1"`.
    pub format: String,
    /// URL where the attestation bundle (statement + signature +
    /// Rekor inclusion proof) is published.
    pub bundle_url: String,
    /// Signer identity recorded by the OIDC issuer.
    pub signer_identity: String,
    /// OIDC issuer that authenticated the signer.
    pub signer_issuer: String,
    /// Rekor transparency log URL.
    pub rekor_url: String,
    /// Numeric log index in the Rekor instance.
    pub rekor_log_index: u64,
    /// Inclusion-proof timestamp, RFC 3339.
    pub signed_at: String,
}

/// SLSA build provenance attestation locator. The full attestation
/// bundle is published as a separate artifact alongside the binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryAttestationMetadata {
    /// Format identifier, currently `"slsa-provenance-v1"`.
    pub format: String,
    /// URL where the SLSA provenance attestation is published.
    pub attestation_url: String,
    /// Builder identifier as recorded in the SLSA provenance.
    pub builder_id: String,
    /// Git tag of the perf-sentinel release that produced the binary.
    pub git_tag: String,
    /// Git commit SHA of the source tree used for the build.
    pub git_commit: String,
    /// SLSA level claimed by the attestation (e.g. `"L2"`, `"L3"`).
    pub slsa_level: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
            binary_version: String::new(),
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
                calibration_applied: false,
                energy_source_models: BTreeSet::new(),
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
            period_coverage: 1.0,
            binary_versions: BTreeSet::new(),
            runtime_windows_count: 0,
            fallback_windows_count: 0,
            per_service_energy_models: BTreeMap::new(),
            per_service_measured_ratio: BTreeMap::new(),
        }
    }

    fn sample_integrity() -> Integrity {
        Integrity {
            content_hash: "sha256:".to_string()
                + "0000000000000000000000000000000000000000000000000000000000000000",
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: None,
            binary_attestation: None,
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

    #[test]
    fn aggregate_period_coverage_defaults_to_one_when_missing() {
        // Periodic reports produced before this field shipped omit it.
        // Default must be 1.0 (permissive type-safety value, not a quality
        // signal; see the field doc-comment).
        let legacy = serde_json::json!({
            "total_requests": 0,
            "total_energy_kwh": 0.0,
            "total_carbon_kgco2eq": 0.0,
            "aggregate_efficiency_score": 100.0,
            "aggregate_waste_ratio": 0.0,
            "anti_patterns_detected_count": 0,
            "estimated_optimization_potential_kgco2eq": 0.0
        });
        let agg: Aggregate = serde_json::from_value(legacy).unwrap();
        assert!((agg.period_coverage - 1.0).abs() < f64::EPSILON);
        assert_eq!(agg.runtime_windows_count, 0);
        assert_eq!(agg.fallback_windows_count, 0);
        assert!(agg.binary_versions.is_empty());
    }

    fn sample_signature() -> SignatureMetadata {
        SignatureMetadata {
            format: "sigstore-cosign-intoto-v1".to_string(),
            bundle_url: "https://example.fr/perf-sentinel-attestation.sig".to_string(),
            signer_identity: "https://github.com/robintra/perf-sentinel/.github/workflows/release.yml@refs/tags/v0.7.0".to_string(),
            signer_issuer: "https://token.actions.githubusercontent.com".to_string(),
            rekor_url: "https://rekor.sigstore.dev".to_string(),
            rekor_log_index: 123_456_789,
            signed_at: "2026-05-14T16:00:00Z".to_string(),
        }
    }

    fn sample_attestation() -> BinaryAttestationMetadata {
        BinaryAttestationMetadata {
            format: "slsa-provenance-v1".to_string(),
            attestation_url: "https://github.com/robintra/perf-sentinel/releases/download/v0.7.0/perf-sentinel-linux-amd64.intoto.jsonl".to_string(),
            builder_id: "https://github.com/actions/runner".to_string(),
            git_tag: "v0.7.0".to_string(),
            git_commit: "a47be9d".to_string(),
            slsa_level: "L2".to_string(),
        }
    }

    #[test]
    fn integrity_roundtrip_with_signature_and_attestation() {
        let i = Integrity {
            content_hash: "sha256:".to_string() + &"0".repeat(64),
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: Some(sample_signature()),
            binary_attestation: Some(sample_attestation()),
        };
        let s = serde_json::to_string(&i).unwrap();
        let back: Integrity = serde_json::from_str(&s).unwrap();
        assert_eq!(back.signature, Some(sample_signature()));
        assert_eq!(back.binary_attestation, Some(sample_attestation()));
    }

    #[test]
    fn integrity_roundtrip_with_signature_only() {
        let i = Integrity {
            content_hash: "sha256:".to_string() + &"0".repeat(64),
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: Some(sample_signature()),
            binary_attestation: None,
        };
        let s = serde_json::to_string(&i).unwrap();
        let back: Integrity = serde_json::from_str(&s).unwrap();
        assert_eq!(back.signature, Some(sample_signature()));
        assert!(back.binary_attestation.is_none());
        // skip_serializing_if drops binary_attestation when None to keep
        // the canonical content_hash form stable for files predating it.
        assert!(!s.contains("binary_attestation"));
    }

    #[test]
    fn integrity_roundtrip_with_attestation_only() {
        let i = Integrity {
            content_hash: "sha256:".to_string() + &"0".repeat(64),
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: None,
            binary_attestation: Some(sample_attestation()),
        };
        let s = serde_json::to_string(&i).unwrap();
        let back: Integrity = serde_json::from_str(&s).unwrap();
        assert!(back.signature.is_none());
        assert_eq!(back.binary_attestation, Some(sample_attestation()));
    }

    #[test]
    fn integrity_roundtrip_hash_only_emits_null_signature() {
        // signature has no skip_serializing_if so legacy hash-only files
        // continue to serialize "signature": null. Required so the
        // canonical content_hash form is stable across the type migration.
        let i = Integrity {
            content_hash: "sha256:".to_string() + &"0".repeat(64),
            binary_hash: None,
            binary_verification_url: None,
            trace_integrity_chain: serde_json::Value::Null,
            signature: None,
            binary_attestation: None,
        };
        let s = serde_json::to_string(&i).unwrap();
        assert!(s.contains("\"signature\":null"));
        assert!(!s.contains("binary_attestation"));
    }

    #[test]
    fn integrity_level_kebab_serialization_for_new_variant() {
        let v = serde_json::to_string(&IntegrityLevel::SignedWithAttestation).unwrap();
        assert_eq!(v, "\"signed-with-attestation\"");
        let back: IntegrityLevel = serde_json::from_str("\"signed-with-attestation\"").unwrap();
        assert!(matches!(back, IntegrityLevel::SignedWithAttestation));
    }
}
