//! Detection stage: identifies performance anti-patterns in traces.

pub mod fanout;
pub mod n_plus_one;
pub mod redundant;
pub mod slow;

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
    /// Earliest timestamp among spans in the detected group.
    pub first_timestamp: String,
    /// Latest timestamp among spans in the detected group.
    pub last_timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green_impact: Option<GreenImpact>,
}

/// Types of performance anti-patterns.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingType {
    NPlusOneSql,
    NPlusOneHttp,
    RedundantSql,
    RedundantHttp,
    SlowSql,
    SlowHttp,
    ExcessiveFanout,
}

/// Severity levels for findings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// Pattern details for a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pattern {
    pub template: String,
    pub occurrences: usize,
    pub window_ms: u64,
    pub distinct_params: usize,
}

/// `GreenOps` impact for a single finding.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GreenImpact {
    /// Extra I/O operations caused by this anti-pattern (occurrences - 1).
    pub estimated_extra_io_ops: usize,
    /// I/O Intensity Score of the endpoint where this finding occurs.
    pub io_intensity_score: f64,
}

impl FindingType {
    #[must_use]
    pub const fn from_event_type_n_plus_one(event_type: &EventType) -> Self {
        match event_type {
            EventType::Sql => Self::NPlusOneSql,
            EventType::HttpOut => Self::NPlusOneHttp,
        }
    }

    #[must_use]
    pub const fn from_event_type_redundant(event_type: &EventType) -> Self {
        match event_type {
            EventType::Sql => Self::RedundantSql,
            EventType::HttpOut => Self::RedundantHttp,
        }
    }

    #[must_use]
    pub const fn from_event_type_slow(event_type: &EventType) -> Self {
        match event_type {
            EventType::Sql => Self::SlowSql,
            EventType::HttpOut => Self::SlowHttp,
        }
    }

    /// Returns the `snake_case` string representation of this finding type.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NPlusOneSql => "n_plus_one_sql",
            Self::NPlusOneHttp => "n_plus_one_http",
            Self::RedundantSql => "redundant_sql",
            Self::RedundantHttp => "redundant_http",
            Self::SlowSql => "slow_sql",
            Self::SlowHttp => "slow_http",
            Self::ExcessiveFanout => "excessive_fanout",
        }
    }

    /// Whether this finding type represents avoidable I/O operations.
    ///
    /// N+1 and redundant patterns are avoidable (can be batched or cached).
    /// Slow and fanout findings are not avoidable: slow operations need
    /// optimization (indexing, caching), and fanout detection cannot distinguish
    /// necessary parallel work from batchable sequential work, so it
    /// conservatively excludes fanout from waste scoring.
    #[must_use]
    pub const fn is_avoidable_io(&self) -> bool {
        matches!(
            self,
            Self::NPlusOneSql | Self::NPlusOneHttp | Self::RedundantSql | Self::RedundantHttp
        )
    }
}

impl Severity {
    /// Returns the `snake_case` string representation of this severity.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// Configuration for the detection stage.
#[derive(Debug, Clone)]
pub struct DetectConfig {
    pub n_plus_one_threshold: u32,
    pub window_ms: u64,
    pub slow_threshold_ms: u64,
    pub slow_min_occurrences: u32,
    pub max_fanout: u32,
}

impl From<&crate::config::Config> for DetectConfig {
    fn from(config: &crate::config::Config) -> Self {
        Self {
            n_plus_one_threshold: config.n_plus_one_threshold,
            window_ms: config.window_duration_ms,
            slow_threshold_ms: config.slow_query_threshold_ms,
            slow_min_occurrences: config.slow_query_min_occurrences,
            max_fanout: config.max_fanout,
        }
    }
}

/// Run all per-trace detectors on a set of traces.
///
/// Does not include cross-trace analysis; see [`slow::detect_slow_cross_trace`].
#[must_use]
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        findings.extend(n_plus_one::detect_n_plus_one(
            trace,
            config.n_plus_one_threshold,
            config.window_ms,
        ));
        findings.extend(redundant::detect_redundant(trace));
        findings.extend(slow::detect_slow(
            trace,
            config.slow_threshold_ms,
            config.slow_min_occurrences,
        ));
        findings.extend(fanout::detect_fanout(trace, config.max_fanout));
    }
    findings
}

/// Sort findings deterministically for stable output.
///
/// Orders by finding type, severity, trace ID, source endpoint, and template.
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.finding_type
            .cmp(&b.finding_type)
            .then_with(|| a.severity.cmp(&b.severity))
            .then_with(|| a.trace_id.cmp(&b.trace_id))
            .then_with(|| a.source_endpoint.cmp(&b.source_endpoint))
            .then_with(|| a.pattern.template.cmp(&b.pattern.template))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> DetectConfig {
        DetectConfig {
            n_plus_one_threshold: 5,
            window_ms: 500,
            slow_threshold_ms: 500,
            slow_min_occurrences: 3,
            max_fanout: 20,
        }
    }

    #[test]
    fn empty_traces_produce_no_findings() {
        let findings = detect(&[], &default_config());
        assert!(findings.is_empty());
    }

    #[test]
    fn finding_type_serializes_to_snake_case() {
        let json = serde_json::to_string(&FindingType::NPlusOneSql).unwrap();
        assert_eq!(json, r#""n_plus_one_sql""#);

        let json = serde_json::to_string(&FindingType::RedundantHttp).unwrap();
        assert_eq!(json, r#""redundant_http""#);

        let json = serde_json::to_string(&FindingType::SlowSql).unwrap();
        assert_eq!(json, r#""slow_sql""#);

        let json = serde_json::to_string(&FindingType::SlowHttp).unwrap();
        assert_eq!(json, r#""slow_http""#);

        let json = serde_json::to_string(&FindingType::ExcessiveFanout).unwrap();
        assert_eq!(json, r#""excessive_fanout""#);
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
                &format!("SELECT * FROM order_item WHERE order_id = {i}"),
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
        let findings = detect(&[trace], &default_config());

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
                    "SELECT * FROM order_item WHERE order_id = 42",
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
        let findings = detect(&[trace_a, trace_b], &default_config());

        // Both traces have redundant queries
        assert!(
            findings.iter().any(|f| f.trace_id == "trace-A"),
            "trace-A should have findings"
        );
        assert!(
            findings.iter().any(|f| f.trace_id == "trace-B"),
            "trace-B should have findings"
        );
    }

    #[test]
    fn finding_type_as_str() {
        assert_eq!(FindingType::NPlusOneSql.as_str(), "n_plus_one_sql");
        assert_eq!(FindingType::SlowHttp.as_str(), "slow_http");
    }

    #[test]
    fn severity_as_str() {
        assert_eq!(Severity::Critical.as_str(), "critical");
        assert_eq!(Severity::Warning.as_str(), "warning");
        assert_eq!(Severity::Info.as_str(), "info");
    }

    #[test]
    fn finding_type_from_event_type_n_plus_one() {
        use crate::event::EventType;
        assert_eq!(
            FindingType::from_event_type_n_plus_one(&EventType::Sql),
            FindingType::NPlusOneSql
        );
        assert_eq!(
            FindingType::from_event_type_n_plus_one(&EventType::HttpOut),
            FindingType::NPlusOneHttp
        );
    }

    #[test]
    fn finding_type_from_event_type_redundant() {
        use crate::event::EventType;
        assert_eq!(
            FindingType::from_event_type_redundant(&EventType::Sql),
            FindingType::RedundantSql
        );
        assert_eq!(
            FindingType::from_event_type_redundant(&EventType::HttpOut),
            FindingType::RedundantHttp
        );
    }

    #[test]
    fn finding_type_from_event_type_slow() {
        use crate::event::EventType;
        assert_eq!(
            FindingType::from_event_type_slow(&EventType::Sql),
            FindingType::SlowSql
        );
        assert_eq!(
            FindingType::from_event_type_slow(&EventType::HttpOut),
            FindingType::SlowHttp
        );
    }

    #[test]
    fn detect_all_three_types_on_one_trace() {
        use crate::test_helpers::{make_sql_event, make_sql_event_with_duration, make_trace};
        let mut events = Vec::new();
        // 5 different params -> N+1
        for i in 1..=5 {
            events.push(make_sql_event(
                "trace-1",
                &format!("span-n{i}"),
                &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
            ));
        }
        // 3 identical queries -> redundant
        for i in 1..=3 {
            events.push(make_sql_event(
                "trace-1",
                &format!("span-r{i}"),
                "SELECT * FROM config WHERE key = 'timeout'",
                &format!("2025-07-10T14:32:02.{:03}Z", i * 30),
            ));
        }
        // 3 slow queries -> slow
        for i in 1..=3 {
            events.push(make_sql_event_with_duration(
                "trace-1",
                &format!("span-s{i}"),
                &format!("SELECT * FROM big_table WHERE id = {}", i + 100),
                &format!("2025-07-10T14:32:03.{:03}Z", i * 30),
                600_000,
            ));
        }
        let trace = make_trace(events);
        let findings = detect(&[trace], &default_config());

        let has_n1 = findings
            .iter()
            .any(|f| f.finding_type == FindingType::NPlusOneSql);
        let has_redundant = findings
            .iter()
            .any(|f| f.finding_type == FindingType::RedundantSql);
        let has_slow = findings
            .iter()
            .any(|f| f.finding_type == FindingType::SlowSql);

        assert!(has_n1, "should detect N+1");
        assert!(has_redundant, "should detect redundant");
        assert!(has_slow, "should detect slow");
    }
}
