//! Detection stage: identifies performance anti-patterns in traces.

pub mod chatty;
pub mod fanout;
pub mod n_plus_one;
pub mod pool_saturation;
pub mod redundant;
pub mod serialized;
pub mod slow;

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::event::EventType;
use serde::{Deserialize, Serialize};

/// Group spans by `parent_span_id`, returning a map from parent ID to
/// child span indices. Spans without a parent are skipped.
///
/// Used by fanout and serialized call detection.
pub(super) fn group_children_by_parent(trace: &Trace) -> HashMap<&str, Vec<usize>> {
    let mut map: HashMap<&str, Vec<usize>> = HashMap::with_capacity(trace.spans.len() / 4 + 1);
    for (idx, span) in trace.spans.iter().enumerate() {
        if let Some(ref parent_id) = span.event.parent_span_id {
            map.entry(parent_id.as_str()).or_default().push(idx);
        }
    }
    map
}

/// Build a span index mapping `span_id -> index` for O(1) parent lookup.
///
/// Used by fanout and serialized call detection.
pub(super) fn build_span_index(trace: &Trace) -> HashMap<&str, usize> {
    trace
        .spans
        .iter()
        .enumerate()
        .map(|(i, s)| (s.event.span_id.as_str(), i))
        .collect()
}

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
    /// Source context of this finding: CI batch run, staging daemon, or
    /// production daemon. Used by downstream consumers (perf-lint) to
    /// boost or reduce severity based on how the finding was produced.
    ///
    /// **Contract:** detectors always emit [`Confidence::default()`]
    /// (= `CiBatch`); the real value is stamped by the pipeline caller
    /// (`pipeline::analyze_with_traces` for batch, `daemon::process_traces`
    /// for the daemon) after detection returns. This keeps the detector
    /// layer oblivious to runtime context.
    #[serde(default)]
    pub confidence: Confidence,
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
    ChattyService,
    PoolSaturation,
    SerializedCalls,
}

/// Severity levels for findings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

/// Source context for a [`Finding`]: where and how it was produced.
///
/// perf-lint consumes this field via its runtime-findings import path and
/// uses it to adjust severity in the IDE. A `daemon_production` finding
/// (observed on real production traffic) is a much stronger signal than a
/// `ci_batch` finding (observed on a controlled integration test run with
/// limited traffic shapes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Batch `analyze` run on traces collected in CI (integration tests).
    /// Lowest confidence: limited traffic shapes, controlled environment.
    ///
    /// Marked `#[default]` so detectors that emit `Confidence::default()`
    /// get the safest fallback (lowest confidence) — a forgotten stamp
    /// never inflates perf-lint's severity.
    #[default]
    CiBatch,
    /// Daemon `watch` run on staging traffic. Medium confidence: real
    /// patterns but not production scale.
    DaemonStaging,
    /// Daemon `watch` run on production traffic. Highest confidence:
    /// real patterns at real scale.
    DaemonProduction,
}

impl Confidence {
    /// Returns the `snake_case` string representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::CiBatch => "ci_batch",
            Self::DaemonStaging => "daemon_staging",
            Self::DaemonProduction => "daemon_production",
        }
    }

    /// Map confidence to a SARIF `rank` value (0-100).
    ///
    /// Rank is SARIF v2.1.0's standard "how much should this matter"
    /// signal: 0 = low priority, 100 = highest. Populating it means
    /// SARIF consumers that ignore the custom `properties` bag still
    /// get a usable ordering.
    #[must_use]
    pub const fn sarif_rank(&self) -> u32 {
        match self {
            Self::CiBatch => 30,
            Self::DaemonStaging => 60,
            Self::DaemonProduction => 90,
        }
    }
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
    /// Classification band for `io_intensity_score`
    /// (`healthy` / `moderate` / `high` / `critical`).
    ///
    /// Computed by [`crate::report::interpret::InterpretationLevel::for_iis`].
    /// The enum values are stable across versions; the thresholds behind
    /// them are versioned with the binary. See
    /// [`crate::report::interpret`] for the stability contract.
    pub io_intensity_band: crate::report::interpret::InterpretationLevel,
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
            Self::ChattyService => "chatty_service",
            Self::PoolSaturation => "pool_saturation",
            Self::SerializedCalls => "serialized_calls",
        }
    }

    /// Returns a short human-readable label for CLI and TUI display.
    #[must_use]
    pub const fn display_label(&self) -> &'static str {
        match self {
            Self::NPlusOneSql => "N+1 SQL",
            Self::NPlusOneHttp => "N+1 HTTP",
            Self::RedundantSql => "Redundant SQL",
            Self::RedundantHttp => "Redundant HTTP",
            Self::SlowSql => "Slow SQL",
            Self::SlowHttp => "Slow HTTP",
            Self::ExcessiveFanout => "Excessive fanout",
            Self::ChattyService => "Chatty service",
            Self::PoolSaturation => "Pool saturation",
            Self::SerializedCalls => "Serialized calls",
        }
    }

    /// Whether this finding type represents avoidable I/O operations.
    ///
    /// N+1 and redundant patterns are avoidable (can be batched or cached).
    /// Slow and fanout findings are not avoidable: slow operations need
    /// optimization (indexing, caching), and fanout detection cannot distinguish
    /// necessary parallel work from batchable sequential work, so it
    /// conservatively excludes fanout from waste scoring.
    ///
    /// Chatty service, pool saturation, and serialized calls are also excluded:
    /// chatty is an architectural concern (service decomposition granularity,
    /// not a per-query batching opportunity), pool saturation is a resource
    /// tuning issue, and serialized calls are a latency optimization that does
    /// not reduce I/O count.
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
    pub chatty_service_min_calls: u32,
    pub pool_saturation_concurrent_threshold: u32,
    pub serialized_min_sequential: u32,
}

impl From<&crate::config::Config> for DetectConfig {
    fn from(config: &crate::config::Config) -> Self {
        Self {
            n_plus_one_threshold: config.n_plus_one_threshold,
            window_ms: config.window_duration_ms,
            slow_threshold_ms: config.slow_query_threshold_ms,
            slow_min_occurrences: config.slow_query_min_occurrences,
            max_fanout: config.max_fanout,
            chatty_service_min_calls: config.chatty_service_min_calls,
            pool_saturation_concurrent_threshold: config.pool_saturation_concurrent_threshold,
            serialized_min_sequential: config.serialized_min_sequential,
        }
    }
}

/// Arguments for [`build_per_trace_finding`], grouped to stay under
/// clippy's argument-count limit.
pub(crate) struct PerTraceFindingArgs<'a> {
    pub finding_type: FindingType,
    pub severity: Severity,
    pub trace_id: &'a str,
    pub first_span: &'a crate::normalize::NormalizedEvent,
    pub template: &'a str,
    pub occurrences: usize,
    pub window_ms: u64,
    pub distinct_params: usize,
    pub suggestion: String,
    pub first_timestamp: &'a str,
    pub last_timestamp: &'a str,
}

/// Build a [`Finding`] from the common fields shared by per-trace
/// detectors (N+1, redundant, slow). Avoids duplicating the struct
/// literal across detection modules.
pub(crate) fn build_per_trace_finding(args: PerTraceFindingArgs<'_>) -> Finding {
    Finding {
        finding_type: args.finding_type,
        severity: args.severity,
        trace_id: args.trace_id.to_string(),
        service: args.first_span.event.service.clone(),
        source_endpoint: args.first_span.event.source.endpoint.clone(),
        pattern: Pattern {
            template: args.template.to_string(),
            occurrences: args.occurrences,
            window_ms: args.window_ms,
            distinct_params: args.distinct_params,
        },
        suggestion: args.suggestion,
        first_timestamp: args.first_timestamp.to_string(),
        last_timestamp: args.last_timestamp.to_string(),
        green_impact: None,
        confidence: Confidence::default(),
    }
}

/// Run all per-trace detectors on a set of traces.
///
/// Does not include cross-trace analysis; see [`slow::detect_slow_cross_trace`].
#[must_use]
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        // Each detector returns a Vec<Finding>. Using append() instead of
        // extend() avoids iterator overhead: append moves the backing
        // allocation in O(1) when the source Vec owns its buffer.
        findings.append(&mut n_plus_one::detect_n_plus_one(
            trace,
            config.n_plus_one_threshold,
            config.window_ms,
        ));
        findings.append(&mut redundant::detect_redundant(trace));
        findings.append(&mut slow::detect_slow(
            trace,
            config.slow_threshold_ms,
            config.slow_min_occurrences,
        ));
        findings.append(&mut fanout::detect_fanout(trace, config.max_fanout));
        findings.append(&mut chatty::detect_chatty(
            trace,
            config.chatty_service_min_calls,
        ));
        findings.append(&mut pool_saturation::detect_pool_saturation(
            trace,
            config.pool_saturation_concurrent_threshold,
        ));
        findings.append(&mut serialized::detect_serialized(
            trace,
            config.serialized_min_sequential,
        ));
    }
    findings
}

/// Sort findings deterministically for stable output.
///
/// Orders by finding type, severity, trace ID, source endpoint, and template.
pub(crate) fn sort_findings(findings: &mut [Finding]) {
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
            chatty_service_min_calls: 15,
            pool_saturation_concurrent_threshold: 10,
            serialized_min_sequential: 3,
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

        let json = serde_json::to_string(&FindingType::ChattyService).unwrap();
        assert_eq!(json, r#""chatty_service""#);

        let json = serde_json::to_string(&FindingType::PoolSaturation).unwrap();
        assert_eq!(json, r#""pool_saturation""#);

        let json = serde_json::to_string(&FindingType::SerializedCalls).unwrap();
        assert_eq!(json, r#""serialized_calls""#);
    }

    #[test]
    fn severity_serializes_to_snake_case() {
        let json = serde_json::to_string(&Severity::Critical).unwrap();
        assert_eq!(json, r#""critical""#);
    }

    // --- Confidence field tests ---

    #[test]
    fn confidence_default_is_ci_batch() {
        assert_eq!(Confidence::default(), Confidence::CiBatch);
    }

    #[test]
    fn confidence_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&Confidence::CiBatch).unwrap(),
            r#""ci_batch""#
        );
        assert_eq!(
            serde_json::to_string(&Confidence::DaemonStaging).unwrap(),
            r#""daemon_staging""#
        );
        assert_eq!(
            serde_json::to_string(&Confidence::DaemonProduction).unwrap(),
            r#""daemon_production""#
        );
    }

    #[test]
    fn confidence_deserializes_from_snake_case() {
        let c: Confidence = serde_json::from_str(r#""ci_batch""#).unwrap();
        assert_eq!(c, Confidence::CiBatch);
        let c: Confidence = serde_json::from_str(r#""daemon_staging""#).unwrap();
        assert_eq!(c, Confidence::DaemonStaging);
        let c: Confidence = serde_json::from_str(r#""daemon_production""#).unwrap();
        assert_eq!(c, Confidence::DaemonProduction);
    }

    #[test]
    fn confidence_as_str_matches_serialization() {
        assert_eq!(Confidence::CiBatch.as_str(), "ci_batch");
        assert_eq!(Confidence::DaemonStaging.as_str(), "daemon_staging");
        assert_eq!(Confidence::DaemonProduction.as_str(), "daemon_production");
    }

    #[test]
    fn confidence_sarif_rank_increases_with_confidence() {
        // Ordering must be strictly ascending so SARIF consumers that sort
        // by rank produce the expected "production > staging > CI" order.
        assert!(Confidence::CiBatch.sarif_rank() < Confidence::DaemonStaging.sarif_rank());
        assert!(Confidence::DaemonStaging.sarif_rank() < Confidence::DaemonProduction.sarif_rank());
        assert_eq!(Confidence::CiBatch.sarif_rank(), 30);
        assert_eq!(Confidence::DaemonStaging.sarif_rank(), 60);
        assert_eq!(Confidence::DaemonProduction.sarif_rank(), 90);
    }

    #[test]
    fn detector_findings_default_to_ci_batch_confidence() {
        // Detectors emit `Confidence::default()` — the pipeline/daemon
        // caller is responsible for stamping the real value. Verify the
        // default here so a regression that changes Confidence::default()
        // surfaces loudly.
        use crate::test_helpers::{make_sql_event, make_trace};
        let events: Vec<crate::event::SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect(&[trace], &default_config());
        assert!(!findings.is_empty());
        for f in &findings {
            assert_eq!(f.confidence, Confidence::CiBatch);
        }
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
        assert_eq!(FindingType::ChattyService.as_str(), "chatty_service");
        assert_eq!(FindingType::PoolSaturation.as_str(), "pool_saturation");
        assert_eq!(FindingType::SerializedCalls.as_str(), "serialized_calls");
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
