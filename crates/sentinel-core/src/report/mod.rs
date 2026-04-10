//! Report stage: outputs analysis results.

pub mod json;
pub mod metrics;
pub mod sarif;

use crate::detect::Finding;
use crate::score::carbon::{CarbonReport, RegionBreakdown};
use serde::Serialize;

/// A complete analysis report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub analysis: Analysis,
    pub findings: Vec<Finding>,
    pub green_summary: GreenSummary,
    pub quality_gate: QualityGate,
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

impl GreenSummary {
    /// Create a `GreenSummary` with only `total_io_ops` set (green scoring disabled).
    #[must_use]
    pub fn disabled(total_io_ops: usize) -> Self {
        Self {
            total_io_ops,
            avoidable_io_ops: 0,
            io_waste_ratio: 0.0,
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
