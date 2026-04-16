//! Report stage: outputs analysis results.

pub mod interpret;
pub mod json;
pub mod metrics;
pub mod sarif;

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::report::interpret::InterpretationLevel;
use crate::score::carbon::{CarbonReport, RegionBreakdown};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A complete analysis report.
#[derive(Debug, Clone, Serialize)]
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
}

/// Analysis metadata.
#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub events_processed: usize,
    pub traces_analyzed: usize,
}

/// `GreenOps` summary of I/O waste.
#[derive(Debug, Clone, Serialize)]
pub struct GreenSummary {
    pub total_io_ops: usize,
    pub avoidable_io_ops: usize,
    pub io_waste_ratio: f64,
    /// Classification band for `io_waste_ratio`
    /// (`healthy` / `moderate` / `high` / `critical`).
    ///
    /// Computed by [`InterpretationLevel::for_waste_ratio`]. The enum
    /// values are stable across versions; the thresholds behind them
    /// are versioned with the binary. See the [`interpret`] module for
    /// the stability contract.
    pub io_waste_ratio_band: InterpretationLevel,
    pub top_offenders: Vec<TopOffender>,
    /// Structured CO₂ report. Includes 2× multiplicative uncertainty
    /// bracket, SCI v1.0 methodology tags, and operational + embodied terms.
    /// `None` when green scoring is disabled or when no events were analyzed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub co2: Option<CarbonReport>,
    /// Per-region operational CO₂ breakdown sorted by `co2_gco2` descending.
    /// Empty when green scoring is disabled or no events were analyzed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionBreakdown>,
    /// Network transport CO₂ (gCO₂eq). Only present when
    /// `[green] include_network_transport = true` and at least one
    /// cross-region HTTP call had response size data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_gco2: Option<f64>,
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
        }
    }
}

/// A top offender endpoint ranked by I/O Intensity Score.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TopOffender {
    pub endpoint: String,
    pub service: String,
    pub io_intensity_score: f64,
    /// Classification band for `io_intensity_score`. Stable enum values
    /// across versions; thresholds versioned with the binary. See the
    /// [`interpret`] module for the stability contract.
    pub io_intensity_band: InterpretationLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub co2_grams: Option<f64>,
}

/// Quality gate result.
#[derive(Debug, Clone, Serialize)]
pub struct QualityGate {
    pub passed: bool,
    pub rules: Vec<QualityRule>,
}

/// A single quality gate rule check.
#[derive(Debug, Clone, Serialize)]
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
