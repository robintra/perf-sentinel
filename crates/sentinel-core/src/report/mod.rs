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
pub mod sarif;

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
}

/// Raw I/O operation count for a single `(service, endpoint)` pair.
///
/// Stable JSON shape from v0.4.2 onward. Field names will not be renamed
/// or removed in a minor release. The `(service, endpoint)` pair is the
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
                span.event.service.as_str(),
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
}
