//! Detection stage: identifies performance anti-patterns in traces.

pub mod chatty;
pub mod correlate_cross;
pub mod fanout;
pub mod n_plus_one;
pub mod pool_saturation;
pub mod redundant;
pub mod sanitizer_aware;
pub mod serialized;
pub mod slow;
pub mod suggestions;

use std::collections::HashMap;

use crate::correlate::Trace;
use crate::event::EventType;
use serde::{Deserialize, Serialize};

/// Precomputed per-trace indices shared by the fanout and serialized
/// detectors. Both detectors need `children_by_parent` +
/// `span_index`; building them once per trace and passing the struct
/// halves the hot-path `HashMap` cost on traces that trigger both
/// detectors.
///
/// `pub` visibility is required because [`fanout::detect_fanout`] and
/// [`serialized::detect_serialized`] are public entry points that take
/// a `&TraceIndices<'_>`. The internal `build` constructor stays
/// `pub(super)` so external callers cannot bypass the `detect()`
/// orchestrator to produce an inconsistent indices / trace pair.
pub struct TraceIndices<'a> {
    pub children_by_parent: HashMap<&'a str, Vec<usize>>,
    pub span_index: HashMap<&'a str, usize>,
}

impl<'a> TraceIndices<'a> {
    /// Build both indices in a single pass over the trace's spans.
    #[must_use]
    pub fn build(trace: &'a Trace) -> Self {
        let mut children_by_parent: HashMap<&str, Vec<usize>> =
            HashMap::with_capacity(trace.spans.len() / 4 + 1);
        let mut span_index: HashMap<&str, usize> = HashMap::with_capacity(trace.spans.len());
        for (idx, span) in trace.spans.iter().enumerate() {
            span_index.insert(span.event.span_id.as_str(), idx);
            if let Some(ref parent_id) = span.event.parent_span_id {
                children_by_parent
                    .entry(parent_id.as_str())
                    .or_default()
                    .push(idx);
            }
        }
        Self {
            children_by_parent,
            span_index,
        }
    }
}

/// A detected performance anti-pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The anti-pattern category (N+1, redundant, slow, fanout, etc.).
    #[serde(rename = "type")]
    pub finding_type: FindingType,
    /// Severity level: critical, warning or info.
    pub severity: Severity,
    /// Trace identifier where the anti-pattern was detected.
    pub trace_id: String,
    /// Name of the service emitting the spans involved in the finding.
    pub service: String,
    /// Normalized inbound endpoint (route template) hosting the pattern.
    pub source_endpoint: String,
    /// Details of the matched pattern: template, occurrences, window, params.
    pub pattern: Pattern,
    /// Human-readable remediation hint for this finding.
    pub suggestion: String,
    /// Earliest timestamp among spans in the detected group.
    pub first_timestamp: String,
    /// Latest timestamp among spans in the detected group.
    pub last_timestamp: String,
    /// `GreenOps` impact estimate. Absent when green scoring is disabled.
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
    /// How this finding's type was classified.
    ///
    /// `None` (default, omitted from JSON) means direct classification
    /// via the standard pipeline rules (`distinct_params >= threshold`
    /// for N+1, repeated identical `(template, params)` for redundant).
    /// `Some(SanitizerHeuristic)` means the type was inferred via the
    /// 0.5.7 sanitizer-aware heuristic, because the OpenTelemetry agent
    /// collapsed every parameter to `?` and the standard distinct-params
    /// signal was unusable. Operators can filter on this field to spot
    /// where the heuristic is firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification_method: Option<ClassificationMethod>,
    /// Source code location from `OTel` `code.*` span attributes.
    /// `None` when the instrumentation agent does not emit these attributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_location: Option<crate::event::CodeLocation>,
    /// OpenTelemetry instrumentation scope chain captured from the
    /// originating span and its ancestors (leaf to root, deduplicated).
    /// Used by [`suggestions::enrich`] as a primary framework signal:
    /// the scope name (e.g. `io.opentelemetry.spring-data-3.0`) is
    /// emitted by the agent regardless of how the user names their
    /// repository class, so it survives user-code naming quirks.
    /// Empty when the upstream format does not carry scope info
    /// (Jaeger, Zipkin) or when the trace is synthetic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instrumentation_scopes: Vec<String>,
    /// Framework-specific actionable fix, populated by
    /// [`suggestions::enrich`] after the per-trace detectors run. `None`
    /// when no framework can be inferred from `code_location` or when the
    /// `(finding_type, framework)` pair has no mapping in the v1 table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<suggestions::SuggestedFix>,
    /// Canonical signature for ack matching, e.g.
    /// `n_plus_one_sql:order-svc:POST_/api/orders:a3f8b2c1`. Always
    /// present in JSON output so users can copy-paste it into
    /// `.perf-sentinel-acknowledgments.toml`. Filled by
    /// [`crate::acknowledgments::enrich_with_signatures`] at end of
    /// `pipeline::analyze` and after deserializing baselines.
    #[serde(default)]
    pub signature: String,
}

/// Types of performance anti-patterns.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// get the safest fallback (lowest confidence), a forgotten stamp
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

/// How a [`Finding`]'s type was determined.
///
/// Orthogonal to [`Confidence`]: confidence describes the runtime context
/// (CI vs production daemon), `ClassificationMethod` describes which
/// detection rule produced the type. Stored in
/// [`Finding::classification_method`] as `Option`; `None` means the
/// standard direct rule fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationMethod {
    /// Standard pipeline classification (e.g. `distinct_params >=
    /// threshold` for N+1, repeated identical `(template, params)` for
    /// redundant). Equivalent to the absence of the field; emitted
    /// explicitly only when a caller wants to be unambiguous.
    Direct,
    /// Reclassified via the 0.5.7 sanitizer-aware heuristic, because the
    /// OpenTelemetry agent's SQL sanitizer collapsed every parameter to
    /// `?`, making the standard distinct-params check unreliable.
    SanitizerHeuristic,
}

/// Pattern details for a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pattern {
    /// Normalized query or URL template shared by the matched spans.
    pub template: String,
    /// Number of spans that matched this template within the window.
    pub occurrences: usize,
    /// Time span, in milliseconds, covering all matched occurrences.
    pub window_ms: u64,
    /// Count of distinct parameter sets observed across occurrences.
    pub distinct_params: usize,
}

/// `GreenOps` impact for a single finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    pub sanitizer_aware_classification: sanitizer_aware::SanitizerAwareMode,
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
            sanitizer_aware_classification: config.sanitizer_aware_classification,
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
    pub code_location: Option<crate::event::CodeLocation>,
    pub instrumentation_scopes: Vec<String>,
    pub classification_method: Option<ClassificationMethod>,
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
        classification_method: args.classification_method,
        code_location: args.code_location,
        instrumentation_scopes: args.instrumentation_scopes,
        suggested_fix: None,
        signature: String::new(),
    }
}

/// Stamp `confidence` on every finding in the slice.
///
/// Detectors emit `Confidence::default()` (= [`Confidence::CiBatch`])
/// per the contract on [`Finding::confidence`]. Pipeline callers
/// override the value with the runtime context (`CiBatch` for batch
/// `analyze`, `DaemonStaging` or `DaemonProduction` for the daemon)
/// using this helper so neither the batch nor the daemon path has to
/// duplicate the loop.
pub fn apply_confidence(findings: &mut [Finding], confidence: Confidence) {
    for finding in findings.iter_mut() {
        finding.confidence = confidence;
    }
}

/// Run per-trace + cross-trace detection on a set of traces.
///
/// Returns the unsorted, unconfidence-stamped `Vec<Finding>`. Callers
/// stamp confidence via [`apply_confidence`] then sort via
/// [`sort_findings`] before emission.
///
/// Cross-trace detection is gated on `traces.len() >= 2` because the
/// percentile-based `detect_slow_cross_trace` requires multiple
/// observations to compute a meaningful p50/p95/p99.
#[must_use]
pub fn run_full_detection(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = detect(traces, config);
    if traces.len() >= 2 {
        let mut cross_trace = slow::detect_slow_cross_trace(
            traces,
            config.slow_threshold_ms,
            config.slow_min_occurrences,
        );
        findings.append(&mut cross_trace);
    }
    findings
}

/// Run all per-trace detectors on a set of traces.
///
/// Does not include cross-trace analysis; see [`slow::detect_slow_cross_trace`]
/// or use [`run_full_detection`] for the combined pass.
#[must_use]
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        // Build the span-relationship indices once per trace and hand
        // them to the detectors that need them (fanout and serialized
        // today). Halves the per-trace HashMap cost vs the previous
        // per-detector-build pattern.
        let indices = TraceIndices::build(trace);
        // Each detector returns a Vec<Finding>. Using append() instead of
        // extend() avoids iterator overhead: append moves the backing
        // allocation in O(1) when the source Vec owns its buffer.
        // n_plus_one runs before redundant, and the resulting findings
        // are passed to redundant so it can skip templates already
        // classified as N+1 (including those reclassified by the
        // sanitizer-aware heuristic).
        let mut n_plus_one_findings = n_plus_one::detect_n_plus_one(
            trace,
            config.n_plus_one_threshold,
            config.window_ms,
            config.sanitizer_aware_classification,
        );
        let mut redundant_findings = redundant::detect_redundant(trace, &n_plus_one_findings);
        findings.append(&mut n_plus_one_findings);
        findings.append(&mut redundant_findings);
        findings.append(&mut slow::detect_slow(
            trace,
            config.slow_threshold_ms,
            config.slow_min_occurrences,
        ));
        findings.append(&mut fanout::detect_fanout(
            trace,
            &indices,
            config.max_fanout,
        ));
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
            &indices,
            config.serialized_min_sequential,
        ));
    }
    suggestions::enrich(&mut findings);
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
            sanitizer_aware_classification: sanitizer_aware::SanitizerAwareMode::default(),
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
        // Detectors emit `Confidence::default()`, the pipeline/daemon
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

    // --- Serde roundtrip for Finding (Deserialize added for query CLI) ---

    #[test]
    fn finding_serde_roundtrip() {
        let finding =
            crate::test_helpers::make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let json = serde_json::to_string(&finding).unwrap();
        let back: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(finding.finding_type, back.finding_type);
        assert_eq!(finding.severity, back.severity);
        assert_eq!(finding.trace_id, back.trace_id);
        assert_eq!(finding.service, back.service);
        assert_eq!(finding.pattern.template, back.pattern.template);
        assert_eq!(finding.confidence, back.confidence);
    }

    #[test]
    fn finding_with_code_location_serde_roundtrip() {
        let mut finding =
            crate::test_helpers::make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.code_location = Some(crate::event::CodeLocation {
            function: Some("processItems".to_string()),
            filepath: Some("src/Order.java".to_string()),
            lineno: Some(42),
            namespace: Some("com.example".to_string()),
        });
        let json = serde_json::to_string(&finding).unwrap();
        let back: Finding = serde_json::from_str(&json).unwrap();
        let loc = back.code_location.unwrap();
        assert_eq!(loc.function.as_deref(), Some("processItems"));
        assert_eq!(loc.lineno, Some(42));
    }

    #[test]
    fn finding_type_deserializes_from_snake_case() {
        let ft: FindingType = serde_json::from_str(r#""n_plus_one_sql""#).unwrap();
        assert_eq!(ft, FindingType::NPlusOneSql);
        let ft: FindingType = serde_json::from_str(r#""chatty_service""#).unwrap();
        assert_eq!(ft, FindingType::ChattyService);
    }

    #[test]
    fn severity_deserializes_from_snake_case() {
        let s: Severity = serde_json::from_str(r#""critical""#).unwrap();
        assert_eq!(s, Severity::Critical);
        let s: Severity = serde_json::from_str(r#""warning""#).unwrap();
        assert_eq!(s, Severity::Warning);
    }
}
