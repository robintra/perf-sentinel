//! Report stage: outputs analysis results.
//!
//! # Deserialization invariant (baseline round-trip)
//!
//! The full [`Report`] tree derives `Deserialize` so `perf-sentinel
//! report --before <baseline.json>` can feed a stored baseline back in.
//! Every saved baseline from a past release must keep parsing after a
//! minor version bump, so the following rule is load-bearing:
//!
//! **New fields added to `Report`, `Analysis`, `GreenSummary`,
//! `QualityGate`, `Finding`, `Pattern`, `TopOffender`, `CarbonReport`,
//! `CarbonEstimate`, `RegionBreakdown` or any nested type must be
//! either `Option<T>` or carry `#[serde(default)]` with a sensible
//! `Default` impl.** A required field added to any of these types
//! breaks every stored baseline and every downstream consumer that
//! deserializes via the same JSON.
//!
//! Removed fields should stay in the struct for at least one minor
//! version with `#[serde(default)]` so incoming JSON from the previous
//! version does not fail on unknown-field attempts to re-read them.
//!
//! We deliberately do NOT add `#[serde(deny_unknown_fields)]`. The
//! trade-off is that a typo like `findigs:` silently deserializes as
//! the default (empty vec), so production pipelines should validate
//! baseline shapes upstream when they care.

pub mod html;
pub mod interpret;
pub mod json;
pub mod metrics;
pub mod periodic;
pub mod sarif;
pub mod warnings;

pub use self::warnings::Warning;

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::detect::correlate_cross::CrossTraceCorrelation;
use crate::report::interpret::InterpretationLevel;
use crate::score::carbon::{CarbonReport, RegionBreakdown, ScoringConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A complete analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub analysis: Analysis,
    pub findings: Vec<Finding>,
    pub green_summary: GreenSummary,
    pub quality_gate: QualityGate,
    /// Raw I/O operation count per `(service, endpoint)`. Populated by
    /// the pipeline regardless of `[green] enabled`, so the `diff`
    /// subcommand works even with green scoring off. Sorted by `service`
    /// then `endpoint` for deterministic JSON output. Empty when no
    /// traces were analyzed.
    ///
    /// Lives on `Report` rather than on `GreenSummary` because it is a
    /// raw telemetry counter, not a green metric, and is filled in
    /// regardless of the green configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_endpoint_io_ops: Vec<PerEndpointIoOps>,
    /// Cross-trace temporal correlations produced by the daemon's
    /// correlator. Always empty in the batch pipeline (the correlator
    /// runs over a rolling window that batch mode does not maintain).
    /// The HTML dashboard's Correlations tab lights up when this field
    /// is non-empty, i.e. when a daemon-produced Report is fed into
    /// `perf-sentinel report --input <daemon.json>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub correlations: Vec<CrossTraceCorrelation>,
    /// Snapshot- or analysis-level warnings surfaced to consumers. The
    /// daemon's `/api/export/report` cold-start path populates this with
    /// `"daemon has not yet processed any events"` so consumers can
    /// distinguish "daemon is empty" from "daemon emitted zero findings"
    /// without resorting to a 5xx HTTP status. Empty in CLI batch
    /// output. Additive on pre-0.5.16 baselines via `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Structured snapshot warnings (0.5.19+). Coexists with the legacy
    /// `warnings: Vec<String>` field. Each entry carries a stable
    /// `kind` (suitable for alerting / aggregation) and a
    /// human-readable `message`. Renderers prefer this field when
    /// non-empty, fall back to `warnings` otherwise. Additive on
    /// pre-0.5.19 baselines via `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warning_details: Vec<Warning>,
    /// Findings filtered out by the user's acknowledgments file
    /// (`.perf-sentinel-acknowledgments.toml`), paired with the matching
    /// ack metadata. Cleared from the wire payload by default; the CLI
    /// only retains it when `--show-acknowledged` is set so audit output
    /// stays opt-in. Additive on pre-0.5.17 baselines via `serde(default)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acknowledged_findings: Vec<AcknowledgedFinding>,
    /// `CARGO_PKG_VERSION` of the binary that wrote this report. Empty
    /// on reports written by binaries that predate this field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub binary_version: String,
}

/// A finding paired with the acknowledgment that suppressed it.
///
/// Surfaced under [`Report::acknowledged_findings`] when the operator
/// asks for `--show-acknowledged`. The CLI clears this vector from the
/// emitted payload otherwise so the default audit trail is opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcknowledgedFinding {
    pub finding: Finding,
    pub acknowledgment: crate::acknowledgments::Acknowledgment,
}

/// Analysis metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub events_processed: usize,
    pub traces_analyzed: usize,
}

/// `GreenOps` summary of I/O waste.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreenSummary {
    pub total_io_ops: usize,
    pub avoidable_io_ops: usize,
    pub io_waste_ratio: f64,
    /// Classification band for `io_waste_ratio`
    /// (`healthy` / `moderate` / `high` / `critical`).
    ///
    /// Computed by [`InterpretationLevel::for_waste_ratio`]. The enum
    /// values are stable across versions, the thresholds behind them
    /// are versioned with the binary. See the [`interpret`] module for
    /// the stability contract.
    pub io_waste_ratio_band: InterpretationLevel,
    pub top_offenders: Vec<TopOffender>,
    /// Structured CO₂ report. Includes 2× multiplicative uncertainty
    /// bracket, SCI v1.0 methodology tags, and operational + embodied terms.
    /// `None` when green scoring is disabled or when no events were analyzed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2: Option<CarbonReport>,
    /// Per-region operational CO₂ breakdown sorted by `co2_gco2` descending.
    /// Empty when green scoring is disabled or no events were analyzed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionBreakdown>,
    /// Network transport CO₂ (gCO₂eq). Only present when
    /// `[green] include_network_transport = true` and at least one
    /// cross-region HTTP call had response size data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_gco2: Option<f64>,
    /// Active Electricity Maps scoring configuration (API version,
    /// emission factor type, temporal granularity). Surfaced for
    /// Scope 2 audit trails so reporters can verify which carbon
    /// model produced the numbers without reading the operator's
    /// TOML config. `None` when Electricity Maps is not configured.
    /// Additive on pre-0.5.12 baselines via `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring_config: Option<ScoringConfig>,
    /// Total energy consumed by the workload during the scoring window
    /// in kWh, runtime-calibrated. Sum of per-service energy when
    /// service-level measurement is available, falls back to the
    /// operational proxy (`total_io_ops × ENERGY_PER_IO_OP_KWH`) when
    /// not. `0.0` on pre-carbon-attribution baselines via `serde(default)`.
    #[serde(default)]
    pub energy_kwh: f64,
    /// Energy model used to compute `energy_kwh`. One of
    /// `"scaphandre_rapl"`, `"cloud_specpower"`, `"io_proxy_v3"`,
    /// `"io_proxy_v2"`, `"io_proxy_v1"`, with optional `+cal` suffix
    /// when per-service calibration factors are active. Reflects the
    /// highest-fidelity model observed in the window (not weighted by
    /// energy consumption). Empty string on pre-carbon-attribution
    /// baselines.
    #[serde(default)]
    pub energy_model: String,
    /// Operational carbon per service in kgCO2eq. Excludes the embodied
    /// term (which stays in `co2.total` only) and the transport term.
    /// Built at scoring time using the runtime-resolved
    /// `service → region` mapping and the per-region grid intensity
    /// (Electricity Maps real-time when available). Sum is
    /// approximately `co2.operational_gco2 / 1000.0` up to
    /// floating-point rounding. Empty on pre-carbon-attribution baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_carbon_kgco2eq: BTreeMap<String, f64>,
    /// Operational energy per service in kWh. Built at scoring time
    /// using the runtime-resolved energy entries (Scaphandre per-process
    /// RAPL when available, cloud `SPECpower` interpolation otherwise,
    /// proxy fallback). Sum is approximately `energy_kwh` up to
    /// floating-point rounding. Empty on pre-carbon-attribution
    /// baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_energy_kwh: BTreeMap<String, f64>,
    /// Per-service region attribution snapshot at scoring time. Surfaces
    /// the `service → region` mapping that produced the per-service
    /// carbon, using `"unknown"` for services that could not be resolved
    /// to a region. Empty on pre-carbon-attribution baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_region: BTreeMap<String, String>,
    /// Per-service energy model tag. Same value set as `energy_model`
    /// (window-level), per-service this time so auditors can verify which
    /// services benefited from Scaphandre or cloud `SPECpower` during this
    /// window. Presence of `"scaphandre_rapl"` or `"cloud_specpower"`
    /// indicates that at least one span of the service hit a measured
    /// energy source, not that 100% of the service's spans were measured.
    /// Read together with `per_service_measured_ratio` for the share of
    /// spans that benefited from the measured model. Services without any
    /// measured span inherit the window-level proxy tag; the `+cal` suffix
    /// on that inherited tag reflects window-wide calibration state, not
    /// whether a calibration factor applied to this specific service.
    /// Empty on pre-per-service-model baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_energy_model: BTreeMap<String, String>,
    /// Fraction of spans whose energy was resolved by Scaphandre or
    /// cloud `SPECpower` (versus proxy fallback) per service, in `[0.0,
    /// 1.0]`. `1.0` means every span had measured energy, `0.0` means
    /// the service fell back to proxy entirely. Pair with
    /// `per_service_energy_model` to assess fidelity. The aggregator
    /// surfaces a simple arithmetic mean of these per-window ratios
    /// under `aggregate.per_service_measured_ratio`, not a span-weighted
    /// average. Empty on pre-per-service-ratio baselines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub per_service_measured_ratio: BTreeMap<String, f64>,
}

/// Raw I/O operation count for a single `(service, endpoint)` pair.
///
/// Stable JSON shape: field names will not be renamed or removed in a
/// minor release. The `(service, endpoint)` pair is the
/// primary key so the same endpoint path served by two different
/// services produces two distinct entries (microservices commonly share
/// generic paths like `/health`, `/metrics`, `/api/users`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerEndpointIoOps {
    pub service: String,
    pub endpoint: String,
    pub io_ops: usize,
}

/// Single-pass per-endpoint I/O op counter. Returns the counts sorted by
/// `(service, endpoint)` for deterministic output. O(N) over the total
/// span count.
///
/// Used by the pipeline to populate `Report.per_endpoint_io_ops` when
/// green scoring is **disabled**. When green scoring is enabled,
/// [`crate::score::score_green`] returns the same data as part of its
/// own single-pass span iteration, so this helper is not called and the
/// hot path stays a single O(N) walk.
#[must_use]
pub fn compute_per_endpoint_io_ops(traces: &[Trace]) -> Vec<PerEndpointIoOps> {
    // BTreeMap so the resulting Vec is naturally sorted by key without
    // a separate sort pass. Key is `(service, endpoint)` so two traces
    // for the same endpoint on different services stay distinct.
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for trace in traces {
        for span in &trace.spans {
            let key = (
                span.event.service.as_ref(),
                span.event.source.endpoint.as_str(),
            );
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .map(|((service, endpoint), io_ops)| PerEndpointIoOps {
            service: service.to_string(),
            endpoint: endpoint.to_string(),
            io_ops,
        })
        .collect()
}

impl GreenSummary {
    /// Create a `GreenSummary` with only `total_io_ops` set (green scoring disabled).
    #[must_use]
    pub fn disabled(total_io_ops: usize) -> Self {
        Self {
            total_io_ops,
            avoidable_io_ops: 0,
            io_waste_ratio: 0.0,
            io_waste_ratio_band: InterpretationLevel::Healthy,
            top_offenders: vec![],
            co2: None,
            regions: vec![],
            transport_gco2: None,
            scoring_config: None,
            energy_kwh: 0.0,
            energy_model: String::new(),
            per_service_carbon_kgco2eq: BTreeMap::new(),
            per_service_energy_kwh: BTreeMap::new(),
            per_service_region: BTreeMap::new(),
            per_service_energy_model: BTreeMap::new(),
            per_service_measured_ratio: BTreeMap::new(),
        }
    }
}

/// A top offender endpoint ranked by I/O Intensity Score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopOffender {
    pub endpoint: String,
    pub service: String,
    pub io_intensity_score: f64,
    /// Classification band for `io_intensity_score`. Stable enum values
    /// across versions, thresholds versioned with the binary. See the
    /// [`interpret`] module for the stability contract.
    pub io_intensity_band: InterpretationLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co2_grams: Option<f64>,
}

/// Quality gate result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGate {
    pub passed: bool,
    pub rules: Vec<QualityRule>,
}

/// A single quality gate rule check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityRule {
    pub rule: String,
    pub threshold: f64,
    pub actual: f64,
    pub passed: bool,
}

/// Trait for report output sinks.
pub trait ReportSink {
    type Error: std::error::Error;

    /// # Errors
    ///
    /// Returns an error if the report cannot be written to the output sink.
    fn emit(&self, report: &Report) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_summary_pre_0512_baseline_loads_without_scoring_config() {
        // Hand-crafted JSON shaped like a pre-0.5.12 baseline (no
        // scoring_config field). The Option must default to None,
        // ensuring `report --before <old.json>` still works after the
        // additive change.
        let json = r#"{
            "total_io_ops": 0,
            "avoidable_io_ops": 0,
            "io_waste_ratio": 0.0,
            "io_waste_ratio_band": "healthy",
            "top_offenders": []
        }"#;
        let summary: GreenSummary = serde_json::from_str(json).expect("backward-compat parse");
        assert!(summary.scoring_config.is_none());
    }

    #[test]
    fn green_summary_disabled_factory_has_no_scoring_config() {
        let summary = GreenSummary::disabled(0);
        assert!(summary.scoring_config.is_none());
    }

    #[test]
    fn green_summary_skips_scoring_config_when_none() {
        let summary = GreenSummary::disabled(42);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            !json.contains("scoring_config"),
            "scoring_config should be skipped when None, got: {json}"
        );
    }

    fn minimal_report_json_without_warning_details() -> String {
        // Shaped like a 0.5.18 Report (no warning_details key). Used to
        // verify that the new field defaults to empty when absent, so a
        // pre-0.5.19 baseline replayed via `report --before <old.json>`
        // still parses cleanly.
        r#"{
            "analysis": {"duration_ms": 0, "events_processed": 0, "traces_analyzed": 0},
            "findings": [],
            "green_summary": {
                "total_io_ops": 0,
                "avoidable_io_ops": 0,
                "io_waste_ratio": 0.0,
                "io_waste_ratio_band": "healthy",
                "top_offenders": []
            },
            "quality_gate": {"passed": true, "rules": []},
            "warnings": ["legacy warning text"]
        }"#
        .to_string()
    }

    #[test]
    fn report_warning_details_default_empty_when_absent() {
        let report: Report =
            serde_json::from_str(&minimal_report_json_without_warning_details()).expect("parse");
        assert!(report.warning_details.is_empty());
    }

    #[test]
    fn report_legacy_warnings_field_still_parses() {
        let report: Report =
            serde_json::from_str(&minimal_report_json_without_warning_details()).expect("parse");
        assert_eq!(report.warnings, vec!["legacy warning text".to_string()]);
        assert!(report.warning_details.is_empty());
    }

    #[test]
    fn report_warning_details_skipped_in_serialize_when_empty() {
        let report = crate::test_helpers::empty_report();
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(
            !json.contains("warning_details"),
            "warning_details should be skipped when empty, got: {json}"
        );
    }

    #[test]
    fn report_warning_details_serialized_when_present() {
        let mut report = crate::test_helpers::empty_report();
        report.warning_details = vec![
            Warning::new("cold_start", "msg one"),
            Warning::new("ingestion_drops", "msg two"),
        ];
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let array = parsed
            .get("warning_details")
            .and_then(|v| v.as_array())
            .expect("warning_details array");
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["kind"], "cold_start");
        assert_eq!(array[1]["kind"], "ingestion_drops");
    }

    #[test]
    fn green_summary_roundtrip_with_new_carbon_attribution_fields() {
        let mut per_service_carbon = BTreeMap::new();
        per_service_carbon.insert("checkout".to_string(), 0.42);
        per_service_carbon.insert("catalog".to_string(), 0.11);
        let mut per_service_energy = BTreeMap::new();
        per_service_energy.insert("checkout".to_string(), 0.0021);
        per_service_energy.insert("catalog".to_string(), 0.0005);
        let mut per_service_region = BTreeMap::new();
        per_service_region.insert("checkout".to_string(), "eu-west-3".to_string());
        per_service_region.insert("catalog".to_string(), "unknown".to_string());
        let mut per_service_energy_model = BTreeMap::new();
        per_service_energy_model.insert("checkout".to_string(), "scaphandre_rapl".to_string());
        per_service_energy_model.insert("catalog".to_string(), "io_proxy_v3+cal".to_string());
        let mut per_service_measured_ratio = BTreeMap::new();
        per_service_measured_ratio.insert("checkout".to_string(), 0.75);
        per_service_measured_ratio.insert("catalog".to_string(), 0.0);

        let summary = GreenSummary {
            energy_kwh: 0.0026,
            energy_model: "scaphandre_rapl+cal".to_string(),
            per_service_carbon_kgco2eq: per_service_carbon.clone(),
            per_service_energy_kwh: per_service_energy.clone(),
            per_service_region: per_service_region.clone(),
            per_service_energy_model: per_service_energy_model.clone(),
            per_service_measured_ratio: per_service_measured_ratio.clone(),
            ..GreenSummary::disabled(0)
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let parsed: GreenSummary = serde_json::from_str(&json).expect("deserialize");

        assert!((parsed.energy_kwh - 0.0026).abs() < 1e-12);
        assert_eq!(parsed.energy_model, "scaphandre_rapl+cal");
        assert_eq!(parsed.per_service_carbon_kgco2eq, per_service_carbon);
        assert_eq!(parsed.per_service_energy_kwh, per_service_energy);
        assert_eq!(parsed.per_service_region, per_service_region);
        assert_eq!(parsed.per_service_energy_model, per_service_energy_model);
        assert_eq!(
            parsed.per_service_measured_ratio,
            per_service_measured_ratio
        );
    }

    #[test]
    fn green_summary_legacy_baseline_deserializes_with_default_carbon_attribution() {
        // A pre-carbon-attribution archive line carries `GreenSummary`
        // without `energy_kwh`, `energy_model`, or the per_service_*
        // maps. Deserialization must fill them with the documented
        // defaults so the aggregator can detect the absence and fall
        // back to the proxy path.
        let legacy = serde_json::json!({
            "total_io_ops": 100,
            "avoidable_io_ops": 5,
            "io_waste_ratio": 0.05,
            "io_waste_ratio_band": "healthy",
            "top_offenders": []
        });
        let parsed: GreenSummary = serde_json::from_value(legacy).expect("deserialize legacy");
        assert!(parsed.energy_kwh.abs() < f64::EPSILON);
        assert!(parsed.energy_model.is_empty());
        assert!(parsed.per_service_carbon_kgco2eq.is_empty());
        assert!(parsed.per_service_energy_kwh.is_empty());
        assert!(parsed.per_service_region.is_empty());
        assert!(parsed.per_service_energy_model.is_empty());
        assert!(parsed.per_service_measured_ratio.is_empty());
    }
}
