//! Report stage: outputs analysis results.

pub mod json;

use crate::detect::Detection;
use crate::score::GreenScore;

/// A complete analysis report.
#[derive(Debug, Clone)]
pub struct Report {
    pub detections: Vec<Detection>,
    pub scores: Vec<GreenScore>,
}

/// Trait for report output sinks.
pub trait ReportSink {
    type Error: std::error::Error;

    fn emit(&self, report: &Report) -> Result<(), Self::Error>;
}
