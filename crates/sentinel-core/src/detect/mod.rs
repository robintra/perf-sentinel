//! Detection stage: identifies performance anti-patterns in traces.

pub mod n_plus_one;
pub mod redundant;

use crate::correlate::Trace;
use crate::event::EventType;
use serde::Serialize;

/// A detected performance anti-pattern.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    #[serde(rename = "type")]
    pub finding_type: FindingType,
    pub severity: Severity,
    pub trace_id: String,
    pub service: String,
    pub source_endpoint: String,
    pub pattern: Pattern,
    pub suggestion: String,
}

/// Types of performance anti-patterns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingType {
    NPlusOneSql,
    NPlusOneHttp,
    RedundantSql,
    RedundantHttp,
}

/// Severity levels for findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// Pattern details for a finding.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pattern {
    pub template: String,
    pub occurrences: usize,
    pub window_ms: u64,
    pub distinct_params: usize,
}

impl FindingType {
    #[must_use]
    pub fn from_event_type_n_plus_one(event_type: &EventType) -> Self {
        match event_type {
            EventType::Sql => FindingType::NPlusOneSql,
            EventType::HttpOut => FindingType::NPlusOneHttp,
        }
    }

    #[must_use]
    pub fn from_event_type_redundant(event_type: &EventType) -> Self {
        match event_type {
            EventType::Sql => FindingType::RedundantSql,
            EventType::HttpOut => FindingType::RedundantHttp,
        }
    }
}

/// Run all detectors on a set of traces.
#[must_use]
pub fn detect(traces: &[Trace], threshold: u32, window_ms: u64) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        findings.extend(n_plus_one::detect_n_plus_one(trace, threshold, window_ms));
        findings.extend(redundant::detect_redundant(trace));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_traces_produce_no_findings() {
        let findings = detect(&[], 5, 500);
        assert!(findings.is_empty());
    }

    #[test]
    fn finding_type_serializes_to_snake_case() {
        let json = serde_json::to_string(&FindingType::NPlusOneSql).unwrap();
        assert_eq!(json, r#""n_plus_one_sql""#);

        let json = serde_json::to_string(&FindingType::RedundantHttp).unwrap();
        assert_eq!(json, r#""redundant_http""#);
    }

    #[test]
    fn severity_serializes_to_snake_case() {
        let json = serde_json::to_string(&Severity::Critical).unwrap();
        assert_eq!(json, r#""critical""#);
    }

    #[test]
    fn detect_combines_n_plus_one_and_redundant() {
        use crate::test_helpers::{make_sql_event, make_trace};
        // 5 events with different params -> N+1
        // 3 events with same params -> redundant
        let mut events = Vec::new();
        for i in 1..=5 {
            events.push(make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                &format!("SELECT * FROM player WHERE game_id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            ));
        }
        for i in 6..=8 {
            events.push(make_sql_event(
                "trace-1",
                &format!("span-{i}"),
                "SELECT * FROM config WHERE key = 'timeout'",
                &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
            ));
        }

        let trace = make_trace(events);
        let findings = detect(&[trace], 5, 500);

        let has_n_plus_one = findings
            .iter()
            .any(|f| f.finding_type == FindingType::NPlusOneSql);
        let has_redundant = findings
            .iter()
            .any(|f| f.finding_type == FindingType::RedundantSql);
        assert!(has_n_plus_one, "should detect N+1");
        assert!(has_redundant, "should detect redundant");
    }

    #[test]
    fn detect_multiple_traces() {
        use crate::test_helpers::{make_sql_event, make_trace};
        // Two separate traces, each with redundant queries
        let events_t1: Vec<crate::event::SpanEvent> = (1..=3)
            .map(|i| {
                make_sql_event(
                    "trace-A",
                    &format!("span-a{i}"),
                    "SELECT * FROM player WHERE game_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let events_t2: Vec<crate::event::SpanEvent> = (1..=2)
            .map(|i| {
                make_sql_event(
                    "trace-B",
                    &format!("span-b{i}"),
                    "SELECT * FROM orders WHERE user_id = 7",
                    &format!("2025-07-10T14:32:02.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace_a = make_trace(events_t1);
        let trace_b = make_trace(events_t2);
        let findings = detect(&[trace_a, trace_b], 5, 500);

        // Both traces have redundant queries
        let trace_a_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.trace_id == "trace-A")
            .collect();
        let trace_b_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.trace_id == "trace-B")
            .collect();
        assert!(!trace_a_findings.is_empty(), "trace-A should have findings");
        assert!(!trace_b_findings.is_empty(), "trace-B should have findings");
    }
}
