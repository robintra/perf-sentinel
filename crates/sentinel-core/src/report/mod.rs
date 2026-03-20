//! Report stage: outputs analysis results.

pub mod json;

use crate::detect::Finding;
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
    pub top_offenders: Vec<String>,
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
