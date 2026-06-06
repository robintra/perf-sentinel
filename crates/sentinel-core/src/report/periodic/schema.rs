//! Wire schema (v1.2) for the periodic disclosure report.
//! See `docs/design/08-PERIODIC-DISCLOSURE.md` for ordering and
//! determinism invariants that any change here must preserve.
//!
//! v1.1 adds the `canonical_waste` / `operational_waste` tiers to
//! `Aggregate`: avoidable energy and carbon at the binary-pinned canonical
//! N+1 threshold and at the operator's configured threshold, side by side.
//! The pre-existing flat avoidable fields are retained as aliases of the
//! canonical tier. Additive via `serde(default)`, so v1.0 readers and
//! reports remain compatible.
//!
//! v1.2 adds `Aggregate.temporal_coverage` (how many declared calendar days
//! carry archived windows, a continuity signal), `ScopeManifest.coverage_basis`
//! (which scope fields are operator-asserted vs machine-derived), and the
//! reserved `Integrity.cross_period_log` hook for a future inter-period
//! transparency log. Every addition uses `serde(default)` plus a
//! `skip_serializing_if` so a pre-v1.2 report re-hashed on a v1.2 binary keeps
//! its `content_hash`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "perf-sentinel-report/v1.2";

/// Scope fields the operator declares by hand in the org config. These are
/// unaudited inputs: the binary cannot verify the size of the portfolio they
/// describe. Surfaced in [`CoverageBasis`] so a report consumer sees, in-band,
/// which figures rest on operator assertion.
pub const SCOPE_OPERATOR_DECLARED_FIELDS: &[&str] = &[
    "total_applications_declared",
    "total_requests_in_period",
    "applications_excluded",
    "environments_measured",
    "environments_excluded",
];

/// Scope fields the aggregator derives from the archived windows. These cannot
/// be set by the operator and are therefore not subject to the self-disclosure
/// caveat that applies to [`SCOPE_OPERATOR_DECLARED_FIELDS`].
pub const SCOPE_MACHINE_DERIVED_FIELDS: &[&str] = &[
    "applications_measured",
    "requests_measured",
    "coverage_percentage",
];

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
    /// In-band provenance marker: which scope fields are operator-asserted
    /// (unaudited) versus machine-derived from the archives. Constant for a
    /// given schema version, surfaced so a JSON consumer need not consult
    /// `docs/SCHEMA.md`. Absent on pre-v1.2 reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_basis: Option<CoverageBasis>,
}

/// Names the [`ScopeManifest`] fields that rest on operator assertion versus
/// those the aggregator computes. See [`SCOPE_OPERATOR_DECLARED_FIELDS`] and
/// [`SCOPE_MACHINE_DERIVED_FIELDS`]. The lists are fixed per schema version,
/// so this object carries no per-report variability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageBasis {
    pub operator_declared: Vec<String>,
    pub machine_derived: Vec<String>,
}

impl CoverageBasis {
    /// The canonical provenance split for the current schema version.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            operator_declared: SCOPE_OPERATOR_DECLARED_FIELDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            machine_derived: SCOPE_MACHINE_DERIVED_FIELDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
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
    /// Vintage of the `SPECpower` / CCF coefficient table that the
    /// running binary embedded at build time. Independent from the
    /// operator-declared `specpower_table_version` above. Consumers can
    /// compare both strings to detect drift between the operator's
    /// disclosure and the embedded data. Always populated by the
    /// `disclose` command from
    /// [`crate::score::cloud_energy::embedded_specpower_vintage`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_specpower_vintage: Option<String>,
    pub scaphandre_used: bool,
    /// Energy source models observed in the archived windows for the
    /// period. Sourced from each window's `GreenSummary.energy_model`
    /// (e.g. `"scaphandre_rapl"`, `"kepler_ebpf"`, `"redfish_bmc"`,
    /// `"cloud_specpower"`, `"io_proxy_v3"`)
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

/// Avoidable energy and carbon for one N+1 threshold, summed over the period.
///
/// `n_plus_one_threshold` is the threshold that produced these figures: the
/// binary-pinned [`crate::detect::DISCLOSURE_N_PLUS_ONE_THRESHOLD`] for the
/// canonical tier, the operator's configured value for the operational tier.
/// `waste_ratio` is `avoidable_io_ops / total_io_ops` over the period and
/// `efficiency_score` is `100 - waste_ratio * 100`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WasteTier {
    pub n_plus_one_threshold: u32,
    pub energy_kwh: f64,
    pub carbon_kgco2eq: f64,
    pub waste_ratio: f64,
    pub efficiency_score: f64,
}

impl WasteTier {
    /// True when the tier is the all-zero default (a v1.0 / pre-canonical
    /// report). Drives `skip_serializing_if` so absent tiers stay absent on
    /// the wire and the `content_hash` is stable across schema versions.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Temporal continuity of the period (v1.2). Measures how much of the declared
/// calendar window actually carried archived measurements.
///
/// Caveat: daemon archiving is traffic-gated (a window with no traffic writes
/// nothing), so this is "days with observed traffic", a lower bound on
/// activity, NOT a daemon-uptime guarantee. Legitimately quiet days (nights,
/// weekends, low-traffic services) lower it. It is published for transparency
/// and surfaced as a warning when low, never a hard `official` gate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TemporalCoverage {
    /// `observed_days / days_in_period`, clamped to `[0, 1]`.
    pub temporal_coverage: f64,
    /// Distinct UTC calendar days in the period that carry >= 1 window.
    pub observed_days: u32,
    /// The period's declared `days_covered`, mirrored for self-containment.
    pub days_in_period: u32,
    /// Longest run of consecutive in-period days with zero windows.
    pub largest_gap_days: u32,
}

impl TemporalCoverage {
    /// True when all-zero: a pre-v1.2 report, or a period with no windows.
    /// Drives `skip_serializing_if` so the field stays absent on the wire and
    /// the `content_hash` of pre-v1.2 reports is unchanged. Unlike
    /// `period_coverage`, the honest "unknown" here is zero/absent, not a
    /// permissive 1.0.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Aggregate {
    pub total_requests: u64,
    pub total_energy_kwh: f64,
    pub total_carbon_kgco2eq: f64,
    /// Period efficiency. Since v1.1 this aliases `canonical_waste.efficiency_score`
    /// (the non-manipulable tier); pre-v1.1 it carried the operator-threshold value.
    pub aggregate_efficiency_score: f64,
    /// Period waste ratio. Since v1.1 this aliases `canonical_waste.waste_ratio`.
    pub aggregate_waste_ratio: f64,
    pub anti_patterns_detected_count: u64,
    /// Avoidable carbon. Since v1.1 this aliases `canonical_waste.carbon_kgco2eq`.
    pub estimated_optimization_potential_kgco2eq: f64,
    /// Avoidable energy/carbon at the binary-pinned canonical N+1 threshold.
    /// Non-manipulable by operator config: this is the headline disclosure
    /// figure. `Default` (all zeros, threshold 0) for v1.0 reports.
    /// `skip_serializing_if` omits the default so a v1.0 report re-hashed on
    /// a v1.1 binary keeps the same `content_hash`.
    #[serde(default, skip_serializing_if = "WasteTier::is_default")]
    pub canonical_waste: WasteTier,
    /// Avoidable energy/carbon at the operator's configured N+1 threshold,
    /// recorded with that threshold so the gap to `canonical_waste` is
    /// visible. `Default` for v1.0 reports.
    #[serde(default, skip_serializing_if = "WasteTier::is_default")]
    pub operational_waste: WasteTier,
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
    /// Temporal continuity of the period (v1.2). See [`TemporalCoverage`].
    /// Default (all-zero) for pre-v1.2 reports, omitted from the wire so their
    /// `content_hash` stays stable.
    #[serde(default, skip_serializing_if = "TemporalCoverage::is_default")]
    pub temporal_coverage: TemporalCoverage,
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
    /// Reserved (v1.2): locator for an external append-only / Rekor-style
    /// transparency log that chains successive periodic reports, enabling
    /// INTER-period continuity verification (detecting an operator who silently
    /// stopped publishing for several periods). Always absent in v1.2; will be
    /// populated only under a future `intent=audited`. Part of the disclosed
    /// content, so it is NOT a post-sign field. See [`CrossPeriodLogRef`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_period_log: Option<CrossPeriodLogRef>,
}

/// Reserved (v1.2) locator for an inter-period transparency log. The intra-
/// report integrity guarantees (`content_hash`, cosign signature, SLSA
/// provenance) bind a single published report. They cannot detect an operator
/// who simply stops disclosing. A chained, append-only log of successive
/// `content_hash` values closes that gap for the future `audited` intent.
/// Defined now so the field is forward-compatible; no runtime support yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossPeriodLogRef {
    /// Format identifier for the inter-period log entry.
    pub format: String,
    /// URL of the append-only log holding this report's entry.
    pub log_url: String,
    /// `content_hash` of the immediately preceding period's report, chaining
    /// the disclosures so a skipped period is detectable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_report_hash: Option<String>,
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
    use core::assert_matches;

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
            coverage_basis: Some(CoverageBasis::standard()),
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
            calibration_inputs: super::super::test_fixtures::sample_calibration_inputs(),
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
            canonical_waste: WasteTier {
                n_plus_one_threshold: 2,
                energy_kwh: 2.1,
                carbon_kgco2eq: 0.25,
                waste_ratio: 0.18,
                efficiency_score: 82.0,
            },
            operational_waste: WasteTier {
                n_plus_one_threshold: 5,
                energy_kwh: 0.9,
                carbon_kgco2eq: 0.12,
                waste_ratio: 0.08,
                efficiency_score: 92.0,
            },
            period_coverage: 1.0,
            binary_versions: BTreeSet::new(),
            runtime_windows_count: 0,
            fallback_windows_count: 0,
            per_service_energy_models: BTreeMap::new(),
            per_service_measured_ratio: BTreeMap::new(),
            temporal_coverage: TemporalCoverage {
                temporal_coverage: 0.9,
                observed_days: 81,
                days_in_period: 90,
                largest_gap_days: 3,
            },
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
            cross_period_log: None,
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
    fn default_waste_tiers_omitted_from_serialization() {
        // A v1.0 / pre-canonical report has all-zero tiers; they must be
        // omitted so re-hashing it on a v1.1 binary keeps content_hash stable.
        let mut agg = sample_aggregate();
        assert!(
            serde_json::to_value(&agg)
                .unwrap()
                .get("canonical_waste")
                .is_some()
        );

        agg.canonical_waste = WasteTier::default();
        agg.operational_waste = WasteTier::default();
        let json = serde_json::to_value(&agg).unwrap();
        assert!(json.get("canonical_waste").is_none());
        assert!(json.get("operational_waste").is_none());
    }

    #[test]
    fn roundtrip_v1_full_g1() {
        let r = sample_report(vec![Application::G1(sample_g1_app())]);
        let json = serde_json::to_string(&r).unwrap();
        let back: PeriodicReport = serde_json::from_str(&json).unwrap();
        assert_matches!(back.applications[0], Application::G1(_));
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
        assert_matches!(back.applications[0], Application::G2(_));
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
        assert_matches!(
            serde_json::from_value::<Application>(g1).unwrap(),
            Application::G1(_)
        );
        assert_matches!(
            serde_json::from_value::<Application>(g2).unwrap(),
            Application::G2(_)
        );
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
            cross_period_log: None,
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
            cross_period_log: None,
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
            cross_period_log: None,
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
            cross_period_log: None,
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
        assert_matches!(back, IntegrityLevel::SignedWithAttestation);
    }

    #[test]
    fn default_temporal_coverage_omitted_from_serialization() {
        // A pre-v1.2 report has the all-zero default; it must be omitted so
        // re-hashing it on a v1.2 binary keeps content_hash stable.
        let mut agg = sample_aggregate();
        assert!(
            serde_json::to_value(&agg)
                .unwrap()
                .get("temporal_coverage")
                .is_some()
        );
        agg.temporal_coverage = TemporalCoverage::default();
        let json = serde_json::to_value(&agg).unwrap();
        assert!(json.get("temporal_coverage").is_none());
    }

    #[test]
    fn aggregate_temporal_coverage_defaults_when_missing() {
        // Reports produced before v1.2 omit the field; default is all-zero
        // (the honest "unknown"), NOT the permissive 1.0 of period_coverage.
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
        assert!(agg.temporal_coverage.is_default());
        assert_eq!(agg.temporal_coverage.observed_days, 0);
    }

    #[test]
    fn coverage_basis_roundtrips_and_is_omitted_when_none() {
        let mut scope = sample_scope();
        let json = serde_json::to_value(&scope).unwrap();
        let basis = json.get("coverage_basis").unwrap();
        assert!(basis.get("operator_declared").is_some());
        assert!(basis.get("machine_derived").is_some());

        scope.coverage_basis = None;
        let json = serde_json::to_value(&scope).unwrap();
        assert!(json.get("coverage_basis").is_none());
    }

    #[test]
    fn cross_period_log_reserved_and_absent_in_v1_2() {
        // The reserved hook is None today; it must not appear on the wire so
        // the content_hash of every current report is unaffected.
        let i = sample_integrity();
        let s = serde_json::to_string(&i).unwrap();
        assert!(!s.contains("cross_period_log"));
        let back: Integrity = serde_json::from_str(&s).unwrap();
        assert!(back.cross_period_log.is_none());
    }

    #[test]
    fn coverage_basis_field_lists_match_scope_manifest_keys() {
        // The provenance lists are hand-maintained strings. Lock them to the
        // real serialized field names so a future rename of a ScopeManifest
        // field cannot silently leave a stale provenance label, mirroring
        // `core_patterns_required_matches_constant`.
        let scope = ScopeManifest {
            total_applications_declared: 1,
            applications_measured: 1,
            applications_excluded: vec![ExcludedApp {
                service_name: "x".to_string(),
                reason: "y".to_string(),
            }],
            environments_measured: vec!["prod".to_string()],
            environments_excluded: vec![ExcludedEnv {
                name: "dev".to_string(),
                reason: "z".to_string(),
            }],
            total_requests_in_period: Some(1),
            requests_measured: 1,
            coverage_percentage: Some(1.0),
            coverage_basis: Some(CoverageBasis::standard()),
        };
        let value = serde_json::to_value(&scope).unwrap();
        let keys = value.as_object().unwrap();
        for name in SCOPE_OPERATOR_DECLARED_FIELDS
            .iter()
            .chain(SCOPE_MACHINE_DERIVED_FIELDS)
        {
            assert!(
                keys.contains_key(*name),
                "coverage_basis lists a non-existent ScopeManifest field {name:?}"
            );
            assert!(
                !(SCOPE_OPERATOR_DECLARED_FIELDS.contains(name)
                    && SCOPE_MACHINE_DERIVED_FIELDS.contains(name)),
                "{name:?} appears in both provenance lists"
            );
        }
    }
}
