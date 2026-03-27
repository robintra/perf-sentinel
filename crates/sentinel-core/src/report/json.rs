//! JSON report sink: serializes the report as structured JSON to stdout.

use crate::report::{Report, ReportSink};

/// Outputs reports as JSON to stdout.
pub struct JsonReportSink;

impl ReportSink for JsonReportSink {
    type Error = JsonReportError;

    fn emit(&self, report: &Report) -> Result<(), Self::Error> {
        let stdout = std::io::stdout();
        let lock = stdout.lock();
        serde_json::to_writer_pretty(lock, report).map_err(|e| JsonReportError(e.to_string()))?;
        println!();
        Ok(())
    }
}

/// Errors that can occur during JSON report output.
#[derive(Debug, thiserror::Error)]
#[error("JSON report error: {0}")]
pub struct JsonReportError(String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Analysis, GreenSummary, QualityGate, Report};

    #[test]
    fn emit_empty_report() {
        let sink = JsonReportSink;
        let report = Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: 0,
                traces_analyzed: 0,
            },
            findings: vec![],
            green_summary: GreenSummary {
                total_io_ops: 0,
                avoidable_io_ops: 0,
                io_waste_ratio: 0.0,
                top_offenders: vec![],
                estimated_co2_grams: None,
                avoidable_co2_grams: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
        };
        assert!(sink.emit(&report).is_ok());
    }

    #[test]
    fn error_display() {
        let err = JsonReportError("test".to_string());
        assert_eq!(format!("{err}"), "JSON report error: test");
    }

    #[test]
    fn emit_report_with_findings() {
        use crate::detect::{Finding, FindingType, Pattern, Severity};

        let report = Report {
            analysis: Analysis {
                duration_ms: 42,
                events_processed: 10,
                traces_analyzed: 1,
            },
            findings: vec![Finding {
                finding_type: FindingType::NPlusOneSql,
                severity: Severity::Warning,
                trace_id: "trace-1".to_string(),
                service: "game".to_string(),
                source_endpoint: "POST /api/game/42/start".to_string(),
                pattern: Pattern {
                    template: "SELECT * FROM player WHERE game_id = ?".to_string(),
                    occurrences: 6,
                    window_ms: 250,
                    distinct_params: 6,
                },
                suggestion: "Use WHERE ... IN (?) to batch 6 queries into one".to_string(),
                first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
                last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
                green_impact: None,
            }],
            green_summary: GreenSummary {
                total_io_ops: 10,
                avoidable_io_ops: 5,
                io_waste_ratio: 0.5,
                top_offenders: vec![],
                estimated_co2_grams: None,
                avoidable_co2_grams: None,
            },
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("n_plus_one_sql"));
        assert!(json.contains("trace-1"));
        assert!(json.contains("game_id"));
        assert!(json.contains("\"occurrences\": 6"));
        assert!(json.contains("\"io_waste_ratio\": 0.5"));
        assert!(json.contains("\"first_timestamp\""));
        assert!(json.contains("\"last_timestamp\""));
    }
}
