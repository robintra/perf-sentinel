//! JSON report sink (stub).

use crate::report::{Report, ReportSink};
use std::fmt;

/// Outputs reports as JSON to stdout.
pub struct JsonReportSink;

impl ReportSink for JsonReportSink {
    type Error = JsonReportError;

    fn emit(&self, report: &Report) -> Result<(), Self::Error> {
        // Stub: print summary counts
        println!(
            "{{\"detections\": {}, \"scores\": {}}}",
            report.detections.len(),
            report.scores.len()
        );
        Ok(())
    }
}

/// Errors that can occur during JSON report output.
#[derive(Debug)]
pub struct JsonReportError;

impl fmt::Display for JsonReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON report error")
    }
}

impl std::error::Error for JsonReportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Report;

    #[test]
    fn emit_empty_report() {
        let sink = JsonReportSink;
        let report = Report {
            detections: vec![],
            scores: vec![],
        };
        assert!(sink.emit(&report).is_ok());
    }

    #[test]
    fn error_display() {
        let err = JsonReportError;
        assert_eq!(format!("{err}"), "JSON report error");
    }
}
