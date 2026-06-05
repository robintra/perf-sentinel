//! N+1 detection for SQL queries and HTTP calls.
//!
//! Detects patterns where the same normalized template is called N times
//! with different parameters within a single trace, inside a configurable
//! time window.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;
use crate::normalize::NormalizedEvent;

use super::sanitizer_aware::{self, SanitizerAwareMode, SanitizerVerdict};
use super::{ClassificationMethod, Finding, FindingType, Severity};

/// Occurrence count at which an N+1 finding escalates from
/// `Severity::Warning` to `Severity::Critical`.
///
/// This is the canonical source of truth for the critical threshold.
/// `crate::report::interpret::IIS_CRITICAL` is mechanically anchored on
/// this constant via a drift-guard test, so that raising or lowering it
/// here automatically updates the CLI interpretation band.
pub(crate) const CRITICAL_OCCURRENCE_THRESHOLD: usize = 10;

/// Fixed N+1 threshold for the avoidable energy/carbon archived for periodic
/// disclosure. Not operator-configurable on purpose: sourcing it from config
/// would let a loose operational threshold shrink the disclosed waste. Value
/// `2` is the most sensitive defensible threshold, so the figure is an upper
/// bound the operator cannot reduce.
pub const DISCLOSURE_N_PLUS_ONE_THRESHOLD: u32 = 2;

/// Detect N+1 patterns in a single trace.
///
/// Groups spans by (`event_type`, template) and emits a finding when the
/// number of occurrences reaches `threshold` within `window_limit` ms.
/// The classification of each group is decided in one of two ways:
///
/// - **Direct** (`distinct_params >= threshold`): the standard rule. The
///   resulting finding has `classification_method = None`.
/// - **Sanitizer heuristic** (gated on `mode`, SQL only): the group has
///   fewer distinct param sets than the threshold but every span looks
///   like the `OTel` SQL sanitizer collapsed its literals to `?`. See
///   [`sanitizer_aware`] for the verdict logic. The resulting finding
///   has `classification_method = Some(SanitizerHeuristic)`.
///
/// The two paths are mutually exclusive by construction
/// ([`sanitizer_aware::looks_sanitized`] requires `params.is_empty()` on
/// every span, so a group reaching the direct rule's distinct-params
/// threshold cannot also be sanitized), so a single loop suffices.
#[must_use]
pub fn detect_n_plus_one(
    trace: &Trace,
    threshold: u32,
    window_limit: u64,
    mode: SanitizerAwareMode,
) -> Vec<Finding> {
    let threshold = threshold as usize;

    let mut groups: HashMap<(&EventType, &str), Vec<usize>> =
        HashMap::with_capacity(trace.spans.len().min(64));
    for (i, span) in trace.spans.iter().enumerate() {
        groups
            .entry((&span.event.event_type, &span.template))
            .or_default()
            .push(i);
    }

    let mut findings = Vec::new();
    for ((event_type, template), indices) in &groups {
        let Some((distinct_params, classification_method)) =
            classify_group(trace, event_type, indices, threshold, mode)
        else {
            continue;
        };
        if let Some(finding) = build_finding(
            trace,
            event_type,
            template,
            indices,
            window_limit,
            distinct_params,
            classification_method,
        ) {
            findings.push(finding);
        }
    }
    findings
}

/// Decide whether a `(event_type, template)` group qualifies as N+1 and
/// under which classification method.
///
/// Returns `Some((distinct_params, classification_method))` to emit a
/// finding, `None` to skip the group. Three emit paths:
/// - direct rule (`distinct_params >= threshold`): returns
///   `(distinct_params, None)`.
/// - SQL sanitizer heuristic (gated on `mode` + `looks_sanitized`):
///   returns `(1, Some(ClassificationMethod::SanitizerHeuristic))`.
/// - HTTP heuristic (gated on `mode` + timing variance): returns
///   `(distinct_params, Some(ClassificationMethod::SanitizerHeuristic))`.
fn classify_group(
    trace: &Trace,
    event_type: &EventType,
    indices: &[usize],
    threshold: usize,
    mode: SanitizerAwareMode,
) -> Option<(usize, Option<ClassificationMethod>)> {
    if indices.len() < threshold {
        return None;
    }
    let distinct_params: HashSet<&[String]> = indices
        .iter()
        .map(|&i| trace.spans[i].params.as_slice())
        .collect();
    if distinct_params.len() >= threshold {
        return Some((distinct_params.len(), None));
    }
    if mode == SanitizerAwareMode::Never {
        return None;
    }
    let high_occurrence = indices.len() >= threshold.saturating_mul(3);
    match event_type {
        EventType::Sql => {
            if !sanitizer_aware::looks_sanitized_indexed(&trace.spans, indices) {
                return None;
            }
            if mode == SanitizerAwareMode::Always {
                return Some((1, Some(ClassificationMethod::SanitizerHeuristic)));
            }
            let verdict = sanitizer_aware::classify_sanitized_sql_group_indexed(
                &trace.spans,
                indices,
                mode,
                || sequential_siblings_indexed(&trace.spans, indices),
                high_occurrence,
            );
            matches!(verdict, SanitizerVerdict::LikelyNPlusOne)
                .then_some((1, Some(ClassificationMethod::SanitizerHeuristic)))
        }
        EventType::HttpOut => {
            if mode == SanitizerAwareMode::Always {
                return Some((
                    distinct_params.len(),
                    Some(ClassificationMethod::SanitizerHeuristic),
                ));
            }
            let verdict = sanitizer_aware::classify_http_group_indexed(
                &trace.spans,
                indices,
                mode,
                || sequential_siblings_indexed(&trace.spans, indices),
                high_occurrence,
            );
            matches!(verdict, SanitizerVerdict::LikelyNPlusOne).then_some((
                distinct_params.len(),
                Some(ClassificationMethod::SanitizerHeuristic),
            ))
        }
    }
}

/// Returns `true` when the indexed group has at least 3 spans, all
/// sharing one non-empty `parent_span_id`, and forming a sequential
/// chain after sort: every consecutive pair satisfies
/// `prev.end_us <= next.start_us`. Strict's bare-driver branch uses
/// this to substitute for the ORM scope marker on stacks like Vert.x
/// reactive PG, pgx, asyncpg and Prisma `queryRaw`.
///
/// Bounds are computed in microseconds (start in ms × 1000 + duration
/// in µs) so two spans sharing the same millisecond timestamp but with
/// sub-millisecond durations are correctly identified as overlapping.
/// A pure-ms check would silently truncate `duration_us / 1000 = 0` and
/// let true concurrent spans pass the sequentiality gate.
#[must_use]
fn sequential_siblings_indexed(spans: &[NormalizedEvent], indices: &[usize]) -> bool {
    if indices.len() < 3 {
        return false;
    }
    let Some(first_parent) = spans[indices[0]]
        .event
        .parent_span_id
        .as_deref()
        .filter(|p| !p.is_empty())
    else {
        return false;
    };
    if indices[1..]
        .iter()
        .any(|&i| spans[i].event.parent_span_id.as_deref() != Some(first_parent))
    {
        return false;
    }
    let mut bounds = Vec::with_capacity(indices.len());
    for &i in indices {
        let span = &spans[i];
        // Skip spans with unparseable timestamps instead of rejecting the
        // whole group: a single corrupted span (adversarial input or rare
        // ingest glitch) should not silently disable detection for the
        // 99 valid siblings around it. Re-test `bounds.len() >= 3` below
        // so the sequentiality verdict still has a stable variance sample.
        if let Some(start_ms) = parse_timestamp_ms(&span.event.timestamp) {
            let start_us = start_ms.saturating_mul(1000);
            let end_us = start_us.saturating_add(span.event.duration_us);
            bounds.push((start_us, end_us));
        }
    }
    if bounds.len() < 3 {
        return false;
    }
    bounds.sort_unstable_by_key(|(s, _)| *s);
    // `<=` (not `<`) so a zero-gap handoff (`prev.end_us == next.start_us`,
    // same microsecond) counts as sequential. Aligned with the boundary
    // chosen by `serialized.rs::compute_predecessors`, which uses the
    // same `end <= start` rule for its sweep-line.
    bounds.array_windows::<2>().all(|[a, b]| a.1 <= b.0)
}

/// Build a finding for a classified group. Returns `None` if the group's
/// time window exceeds `window_limit` ms.
#[allow(clippy::too_many_arguments)] // every arg is irreducibly distinct
fn build_finding(
    trace: &Trace,
    event_type: &EventType,
    template: &str,
    indices: &[usize],
    window_limit: u64,
    distinct_params: usize,
    classification_method: Option<ClassificationMethod>,
) -> Option<Finding> {
    let (window_ms, min_ts, max_ts) = compute_window_and_bounds_iter(
        indices
            .iter()
            .map(|&i| trace.spans[i].event.timestamp.as_str()),
    );
    if window_ms > window_limit {
        return None;
    }

    let first = &trace.spans[indices[0]];
    let severity = if indices.len() >= CRITICAL_OCCURRENCE_THRESHOLD {
        Severity::Critical
    } else {
        Severity::Warning
    };
    let suggestion = match event_type {
        EventType::Sql => format!(
            "Use WHERE ... IN (?) to batch {} queries into one",
            indices.len()
        ),
        EventType::HttpOut => format!(
            "Use batch endpoint with ?ids=... to batch {} calls into one",
            indices.len()
        ),
    };

    Some(super::build_per_trace_finding(super::PerTraceFindingArgs {
        finding_type: FindingType::from_event_type_n_plus_one(event_type),
        severity,
        trace_id: &trace.trace_id,
        first_span: first,
        template,
        occurrences: indices.len(),
        window_ms,
        distinct_params,
        suggestion,
        first_timestamp: min_ts,
        last_timestamp: max_ts,
        code_location: first.event.code_location(),
        instrumentation_scopes: first
            .event
            .instrumentation_scopes
            .iter()
            .map(ToString::to_string)
            .collect(),
        classification_method,
        span_durations_us: Some(
            indices
                .iter()
                .map(|&i| trace.spans[i].event.duration_us)
                .collect(),
        ),
    }))
}

/// Slice-based variant for computing window from a slice of timestamps.
#[cfg(test)]
pub(crate) fn compute_window_and_bounds<'a>(timestamps: &[&'a str]) -> (u64, &'a str, &'a str) {
    compute_window_and_bounds_iter(timestamps.iter().copied())
}

/// Compute the time window in milliseconds and the (min, max) timestamps in a single pass.
///
/// Returns `(window_ms, min_timestamp, max_timestamp)`.
/// ISO 8601 timestamps sort correctly lexicographically.
/// Uses an iterator to avoid intermediate Vec allocation.
pub(crate) fn compute_window_and_bounds_iter<'a>(
    mut iter: impl Iterator<Item = &'a str>,
) -> (u64, &'a str, &'a str) {
    let Some(first) = iter.next() else {
        return (0, "", "");
    };
    let mut min_ts = first;
    let mut max_ts = first;
    let mut has_second = false;
    for ts in iter {
        has_second = true;
        if ts < min_ts {
            min_ts = ts;
        }
        if ts > max_ts {
            max_ts = ts;
        }
    }
    if !has_second {
        return (0, min_ts, max_ts);
    }
    let window_ms = match (parse_timestamp_ms(min_ts), parse_timestamp_ms(max_ts)) {
        (Some(a), Some(b)) => b.saturating_sub(a),
        _ => 0,
    };
    (window_ms, min_ts, max_ts)
}

/// Compute the time window in milliseconds between the earliest and latest timestamps.
/// Expects ISO 8601 format: `YYYY-MM-DDTHH:MM:SS.mmmZ`
#[cfg(test)]
pub(crate) fn compute_window_ms(timestamps: &[&str]) -> u64 {
    compute_window_and_bounds(timestamps).0
}

/// Parse an ISO 8601 UTC timestamp to milliseconds since Unix epoch.
///
/// Thin adapter over the canonical [`crate::time::parse_iso8601_utc_to_ms`]
/// so the detect pipeline keeps its `Option<u64>` shape while the
/// civil-date arithmetic stays in `time.rs` (single source of truth).
pub(crate) fn parse_timestamp_ms(ts: &str) -> Option<u64> {
    crate::time::parse_iso8601_utc_to_ms(ts).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SpanEvent;
    use crate::test_helpers::{
        make_http_event, make_http_event_with_duration, make_sql_event, make_trace,
    };

    #[test]
    fn detects_n_plus_one_sql() {
        let events = crate::test_helpers::make_n_plus_one_events();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 6);
        assert_eq!(findings[0].pattern.distinct_params, 6);
        assert!(findings[0].suggestion.contains("batch"));
    }

    #[test]
    fn detects_n_plus_one_http() {
        let events: Vec<SpanEvent> = (101..=106)
            .map(|i| {
                make_http_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("http://user-svc:5000/api/users/{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", (i - 100) * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneHttp);
        assert_eq!(findings[0].pattern.occurrences, 6);
        assert!(findings[0].suggestion.contains("batch endpoint"));
    }

    #[test]
    fn below_threshold_no_finding() {
        let events = crate::test_helpers::make_sql_series_events(4);

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert!(findings.is_empty());
    }

    #[test]
    fn mixed_templates_no_finding() {
        let events = vec![
            make_sql_event(
                "trace-1",
                "span-1",
                "SELECT * FROM users WHERE id = 1",
                "2025-07-10T14:32:01.000Z",
            ),
            make_sql_event(
                "trace-1",
                "span-2",
                "SELECT * FROM orders WHERE id = 2",
                "2025-07-10T14:32:01.050Z",
            ),
            make_sql_event(
                "trace-1",
                "span-3",
                "INSERT INTO logs (msg) VALUES ('hello')",
                "2025-07-10T14:32:01.100Z",
            ),
        ];

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert!(findings.is_empty());
    }

    #[test]
    fn critical_severity_for_10_or_more() {
        let events = crate::test_helpers::make_sql_series_events_with_stride(12, 10);

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[0].pattern.occurrences, 12);
    }

    #[test]
    fn same_params_not_n_plus_one() {
        // 6 events with same template AND same params -> not N+1 (that's redundant)
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_item WHERE order_id = 42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        // Only 1 distinct param set, below threshold of 5
        assert!(findings.is_empty());
    }

    #[test]
    fn window_exceeded_no_finding() {
        // 6 events spread over 10 seconds: exceeds 500ms window limit
        let events: Vec<SpanEvent> = (1..=6)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:{:02}.000Z", i * 2),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert!(findings.is_empty());
    }

    #[test]
    fn compute_window_ms_basic() {
        let timestamps = vec![
            "2025-07-10T14:32:01.000Z",
            "2025-07-10T14:32:01.250Z",
            "2025-07-10T14:32:01.100Z",
        ];
        assert_eq!(compute_window_ms(&timestamps), 250);
    }

    /// Helper: `days_from_civil(2025, 7, 10) * 86_400_000`
    const JUL10_2025_MS: u64 = 20_279 * 86_400_000;

    #[test]
    fn parse_timestamp_ms_basic() {
        assert_eq!(
            parse_timestamp_ms("2025-07-10T14:32:01.123Z"),
            Some(JUL10_2025_MS + 14 * 3_600_000 + 32 * 60_000 + 1_000 + 123)
        );
    }

    #[test]
    fn parse_timestamp_ms_single_frac_digit() {
        // "01.1Z" should be 100ms
        assert_eq!(
            parse_timestamp_ms("2025-07-10T00:00:01.1Z"),
            Some(JUL10_2025_MS + 1_100)
        );
    }

    #[test]
    fn parse_timestamp_ms_two_frac_digits() {
        // "01.12Z" should be 120ms
        assert_eq!(
            parse_timestamp_ms("2025-07-10T00:00:01.12Z"),
            Some(JUL10_2025_MS + 1_120)
        );
    }

    #[test]
    fn window_at_exact_limit_still_detected() {
        // 5 events spanning exactly 500ms -> window_ms == 500, limit == 500
        // Code uses `>` so `==` should pass
        let events: Vec<SpanEvent> = (0..5)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {}", i + 1),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 125),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern.window_ms, 500);
    }

    #[test]
    fn window_zero_limit_filters_all() {
        let events = crate::test_helpers::make_sql_series_events(5);

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 0, SanitizerAwareMode::Auto);
        assert!(findings.is_empty());
    }

    #[test]
    fn severity_boundary_9_is_warning() {
        let events = crate::test_helpers::make_sql_series_events_with_stride(9, 10);

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pattern.occurrences, 9);
    }

    #[test]
    fn severity_boundary_10_is_critical() {
        let events: Vec<SpanEvent> = (1..=10)
            .map(|i| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[0].pattern.occurrences, 10);
    }

    #[test]
    fn compute_window_ms_single_timestamp() {
        let timestamps = vec!["2025-07-10T14:32:01.000Z"];
        assert_eq!(compute_window_ms(&timestamps), 0);
    }

    #[test]
    fn compute_window_ms_empty() {
        let timestamps: Vec<&str> = vec![];
        assert_eq!(compute_window_ms(&timestamps), 0);
    }

    #[test]
    fn parse_timestamp_ms_no_fractional() {
        // No fractional part -> millis = 0
        assert_eq!(
            parse_timestamp_ms("2025-07-10T14:32:01Z"),
            Some(JUL10_2025_MS + 14 * 3_600_000 + 32 * 60_000 + 1_000)
        );
    }

    #[test]
    fn compute_window_ms_across_midnight() {
        let timestamps = vec!["2025-07-10T23:59:59.900Z", "2025-07-11T00:00:00.100Z"];
        assert_eq!(compute_window_ms(&timestamps), 200);
    }

    #[test]
    fn parse_timestamp_ms_invalid_returns_none() {
        assert_eq!(parse_timestamp_ms("not-a-timestamp"), None);
    }

    #[test]
    fn parse_timestamp_ms_missing_parts() {
        // Only 2 colon-separated parts (HH:MM, no seconds) -> None
        assert_eq!(parse_timestamp_ms("2025-07-10T14:32Z"), None);
    }

    #[test]
    fn n_plus_one_finding_has_first_last_timestamps() {
        let events = crate::test_helpers::make_n_plus_one_events();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].first_timestamp, "2025-07-10T14:32:01.050Z");
        assert_eq!(findings[0].last_timestamp, "2025-07-10T14:32:01.300Z");
    }

    // --- Sanitizer-aware reclassification ---

    #[test]
    fn reclassifies_n_plus_one_when_sanitizer_on_with_orm_scope() {
        let events = crate::test_helpers::make_sanitized_n_plus_one_events(
            10,
            Some("io.opentelemetry.spring-data-jpa-3.0"),
            None,
        );
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            findings[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
        assert_eq!(findings[0].pattern.occurrences, 10);
        // distinct_params reflects the on-wire reality: the sanitizer
        // erased every literal, so there is one distinct (empty) params
        // slice in the group.
        assert_eq!(findings[0].pattern.distinct_params, 1);
    }

    #[test]
    fn reclassifies_n_plus_one_when_sanitizer_on_with_high_variance_timing() {
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let events =
            crate::test_helpers::make_sanitized_n_plus_one_events(10, None, Some(&durations));
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            findings[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
        // distinct_params reflects the on-wire reality on a sanitized
        // group: every span has the same empty params slice.
        assert_eq!(findings[0].pattern.distinct_params, 1);
    }

    #[test]
    fn does_not_reclassify_when_sanitizer_on_but_low_variance() {
        let durations = [100u64, 102, 98, 101, 99, 100, 101, 99, 100, 102];
        let events =
            crate::test_helpers::make_sanitized_n_plus_one_events(10, None, Some(&durations));
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        // Inconclusive verdict: heuristic stays silent, leaves the group
        // for the redundant detector.
        assert!(findings.is_empty());
    }

    #[test]
    fn mode_never_disables_reclassification_entirely() {
        let events = crate::test_helpers::make_sanitized_n_plus_one_events(
            10,
            Some("io.opentelemetry.spring-data-jpa-3.0"),
            None,
        );
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Never);
        assert!(findings.is_empty());
    }

    #[test]
    fn mode_always_reclassifies_regardless_of_signals() {
        let durations = [100u64, 102, 98, 101, 99, 100, 101, 99, 100, 102];
        let events =
            crate::test_helpers::make_sanitized_n_plus_one_events(10, None, Some(&durations));
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Always);
        assert_eq!(n_plus_one.len(), 1);
        assert_eq!(n_plus_one[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            n_plus_one[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
        // The cross-detector skip must prevent the redundant detector
        // from emitting the same template twice.
        let redundant_findings = crate::detect::redundant::detect_redundant(&trace, &n_plus_one);
        assert!(
            redundant_findings.is_empty(),
            "redundant detector should skip a template already classified as n+1, got: {:?}",
            redundant_findings
                .iter()
                .map(|f| &f.finding_type)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn strict_mode_keeps_redundant_for_orm_scope_with_low_variance() {
        // Legacy polling loop preserved as redundant_sql: 7 identical
        // queries via Spring Data JPA, all served from the same cached
        // row. ORM scope present but timing tight, occurrence count
        // under the `3 * threshold` cache-warm bar so high_occurrence
        // does not fire. Strict must let the redundant detector pick
        // it up. Counts above 3*threshold (15 for the default
        // threshold of 5) flip to n_plus_one_sql; see
        // `strict_mode_reclassifies_orm_high_occurrence_cache_warm`.
        let durations = [100u64; 7];
        let events = crate::test_helpers::make_sanitized_n_plus_one_events(
            7,
            Some("io.opentelemetry.hibernate-6.0"),
            Some(&durations),
        );
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(
            n_plus_one.is_empty(),
            "Strict must not reclassify when timing variance is flat and \
             occurrence count is below the cache-warm bar: got {:?}",
            n_plus_one
                .iter()
                .map(|f| &f.finding_type)
                .collect::<Vec<_>>()
        );
        let redundant = crate::detect::redundant::detect_redundant(&trace, &n_plus_one);
        assert_eq!(redundant.len(), 1);
        assert_eq!(redundant[0].finding_type, FindingType::RedundantSql);
        assert_eq!(redundant[0].pattern.occurrences, 7);
        // The redundant detector must not stamp the heuristic marker on
        // its own findings, even when Strict declined to reclassify.
        assert_eq!(redundant[0].classification_method, None);
    }

    #[test]
    fn strict_mode_reclassifies_orm_high_occurrence_cache_warm() {
        // Lab dotnet-svc shape: 15 LINQ-by-PK queries on EF Core hitting
        // a warm Npgsql pool. ORM scope present, per-span timings cluster
        // tight (no variance), but the occurrence count clears the
        // `3 * threshold` cache-warm bar — Strict must reclassify rather
        // than miss a real n+1 just because the rows happen to be in
        // memory. Quarkus + Hibernate would land on the same path.
        let durations = [100u64; 15];
        let events = crate::test_helpers::make_sanitized_n_plus_one_events(
            15,
            Some("OpenTelemetry.Instrumentation.EntityFrameworkCore"),
            Some(&durations),
        );
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert_eq!(n_plus_one.len(), 1);
        assert_eq!(n_plus_one[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            n_plus_one[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
        assert_eq!(n_plus_one[0].pattern.occurrences, 15);
    }

    #[test]
    fn strict_mode_still_flags_n_plus_one_when_variance_high() {
        // Real ORM-induced N+1: 10 lookups against different rows, the
        // cache hit/miss spread the durations enough to clear CV > 0.5.
        // Strict emits because both signals (ORM scope + variance) fire.
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let events = crate::test_helpers::make_sanitized_n_plus_one_events(
            10,
            Some("io.opentelemetry.spring-data-jpa-3.0"),
            Some(&durations),
        );
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert_eq!(n_plus_one.len(), 1);
        assert_eq!(n_plus_one[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            n_plus_one[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
    }

    /// Build a bare-driver-shaped sanitized N+1: no ORM scope, every
    /// span under a common parent, timestamps strictly increasing with
    /// the supplied stride (ms). Used by the Strict bare-driver tests.
    /// Timestamps roll over to the seconds field when `count * stride_ms`
    /// exceeds 999, so callers don't silently produce 4-digit fractional
    /// (invalid ISO 8601) timestamps.
    fn make_bare_driver_sanitized_events(
        count: usize,
        parent_id: &str,
        stride_ms: usize,
        durations_us: &[u64],
    ) -> Vec<SpanEvent> {
        (0..count)
            .map(|i| {
                let duration = durations_us.get(i).copied().unwrap_or(800);
                let total_ms = i * stride_ms;
                let secs = 1 + total_ms / 1000;
                let ms = total_ms % 1000;
                assert!(secs < 60, "make_bare_driver_sanitized_events: count*stride exceeds one minute, extend the helper to roll minutes");
                let mut event = crate::test_helpers::make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_items WHERE order_id = ?",
                    &format!("2025-07-10T14:32:{secs:02}.{ms:03}Z"),
                    duration,
                );
                event.parent_span_id = Some(parent_id.to_string());
                event
            })
            .collect()
    }

    #[test]
    fn strict_bare_driver_reclassifies_when_sequential() {
        // Mutiny / Vert.x reactive PG shape: no ORM scope, 10 sanitized
        // SQL spans under one parent, 30ms stride, dispersed durations
        // (CV ~ 0.68). Strict's bare-driver branch must reclassify.
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let events = make_bare_driver_sanitized_events(10, "reactive-root-span", 30, &durations);
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            findings[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
        assert_eq!(findings[0].pattern.occurrences, 10);
        assert_eq!(findings[0].pattern.distinct_params, 1);
    }

    #[test]
    fn strict_bare_driver_10_overlapping_stays_redundant() {
        // 10 concurrent overlapping spans (below the 15-span
        // high_occurrence bar). No ORM scope, no sequentiality, no
        // variance. Strict declines, redundant picks it up.
        let durations = [100_000u64; 10];
        let events = make_bare_driver_sanitized_events(10, "fanout-root-span", 30, &durations);
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(
            n_plus_one.is_empty(),
            "Strict must not reclassify overlapping bare-driver spans below threshold, got: {:?}",
            n_plus_one
                .iter()
                .map(|f| &f.finding_type)
                .collect::<Vec<_>>()
        );
        let redundant = crate::detect::redundant::detect_redundant(&trace, &n_plus_one);
        assert_eq!(redundant.len(), 1);
        assert_eq!(redundant[0].finding_type, FindingType::RedundantSql);
    }

    #[test]
    fn strict_bare_driver_15_overlapping_reclassifies_via_high_occurrence() {
        // 15 concurrent overlapping spans (at the high_occurrence bar).
        // high_occurrence fires as both primary and corroborator, so
        // even concurrent fan-out is reclassified as n+1. Under the
        // looks_sanitized guard, 15 identical parameterized queries in
        // one request is structurally n+1.
        let durations = [100_000u64; 15];
        let events = make_bare_driver_sanitized_events(15, "fanout-root-span", 30, &durations);
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert_eq!(n_plus_one.len(), 1);
        assert_eq!(n_plus_one[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            n_plus_one[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
    }

    #[test]
    fn strict_bare_driver_detects_sub_ms_overlap() {
        // Two adjacent spans share start_ms = 10 but the first runs 800µs:
        // a ms-precision check would report `10 <= 10` (sequential), the
        // µs-precision check correctly rejects the group as overlapping.
        // Regression test for the silent truncation in
        // `sequential_siblings_indexed`.
        let events: Vec<SpanEvent> = (0..10)
            .map(|i| {
                let mut event = crate::test_helpers::make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_items WHERE order_id = ?",
                    "2025-07-10T14:32:01.010Z",
                    800,
                );
                event.parent_span_id = Some("overlap-root-span".to_string());
                event
            })
            .collect();
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(
            n_plus_one.is_empty(),
            "sub-ms overlap must not pass the sequentiality gate, got: {:?}",
            n_plus_one
                .iter()
                .map(|f| &f.finding_type)
                .collect::<Vec<_>>()
        );
        // Sequentiality gate rejected the group, but the spans are still
        // identical sanitized calls: the redundant detector must emit so
        // the operator still sees the duplicate work.
        let redundant = crate::detect::redundant::detect_redundant(&trace, &n_plus_one);
        assert_eq!(redundant.len(), 1);
        assert_eq!(redundant[0].finding_type, FindingType::RedundantSql);
    }

    #[test]
    fn strict_bare_driver_rejects_cross_parent() {
        // Half the spans under parent A, half under parent B. Both halves
        // are individually sequential but the group spans two parents so
        // the gate must reject.
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let mut events = make_bare_driver_sanitized_events(10, "parent-a", 30, &durations);
        for event in events.iter_mut().skip(5) {
            event.parent_span_id = Some("parent-b".to_string());
        }
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(n_plus_one.is_empty());
    }

    #[test]
    fn strict_bare_driver_rejects_missing_parent_span_id() {
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let mut events = make_bare_driver_sanitized_events(10, "any-parent", 30, &durations);
        events[0].parent_span_id = None;
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(n_plus_one.is_empty());
    }

    #[test]
    fn strict_bare_driver_rejects_empty_parent_span_id() {
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let events = make_bare_driver_sanitized_events(10, "", 30, &durations);
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(n_plus_one.is_empty());
    }

    #[test]
    fn strict_bare_driver_tolerates_single_unparseable_timestamp() {
        // A single corrupted timestamp inside a 10-span group must not
        // wholesale-disable detection on the 9 valid siblings around it.
        // `sequential_siblings_indexed` skips the bad span and re-checks
        // `bounds.len() >= 3` so the variance gate still has a stable
        // sample. The finding still reports occurrences=10 (every span
        // matched the template) so the operator sees the full call count.
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let mut events = make_bare_driver_sanitized_events(10, "ts-root-span", 30, &durations);
        events[3].timestamp = "not-a-timestamp".to_string();
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert_eq!(n_plus_one.len(), 1);
        assert_eq!(n_plus_one[0].finding_type, FindingType::NPlusOneSql);
        assert_eq!(
            n_plus_one[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
    }

    #[test]
    fn strict_bare_driver_rejects_when_too_few_valid_timestamps() {
        // If corruption knocks bounds.len() below the 3-span variance
        // threshold, the gate must reject — no stable signal possible.
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let mut events = make_bare_driver_sanitized_events(10, "ts-root-span", 30, &durations);
        for event in events.iter_mut().take(8) {
            event.timestamp = "bad".to_string();
        }
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(n_plus_one.is_empty());
    }

    #[test]
    fn first_pass_findings_carry_no_classification_method() {
        // Non-regression: the standard distinct-params path should not
        // stamp the heuristic marker.
        let events = crate::test_helpers::make_n_plus_one_events();
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].classification_method, None);
    }

    #[test]
    fn n_plus_one_timestamps_unsorted_input() {
        // Timestamps out of order: should still find correct min/max
        let timestamps = [
            "2025-07-10T14:32:01.200Z",
            "2025-07-10T14:32:01.050Z",
            "2025-07-10T14:32:01.300Z",
            "2025-07-10T14:32:01.100Z",
            "2025-07-10T14:32:01.150Z",
        ];
        let events: Vec<SpanEvent> = timestamps
            .iter()
            .enumerate()
            .map(|(i, ts)| {
                make_sql_event(
                    "trace-1",
                    &format!("span-{i}"),
                    &format!("SELECT * FROM order_item WHERE order_id = {}", i + 1),
                    ts,
                )
            })
            .collect();

        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].first_timestamp, "2025-07-10T14:32:01.050Z");
        assert_eq!(findings[0].last_timestamp, "2025-07-10T14:32:01.300Z");
    }

    // ── HTTP heuristic path ─────────────────────────────────────

    fn http_same_id_events(durations: &[u64]) -> Vec<SpanEvent> {
        durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                make_http_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "http://user-svc:5000/api/users/42",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    *d,
                )
            })
            .collect()
    }

    #[test]
    fn http_heuristic_auto_reclassifies_on_high_variance() {
        let events = http_same_id_events(&[100, 50, 200, 60, 250, 80, 300]);
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneHttp);
        assert_eq!(
            findings[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
    }

    #[test]
    fn http_heuristic_auto_low_variance_no_finding() {
        let events = http_same_id_events(&[100, 100, 100, 100, 100, 100, 100]);
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Auto);
        assert!(findings.is_empty());
    }

    #[test]
    fn http_heuristic_never_mode_no_finding() {
        let events = http_same_id_events(&[100, 50, 200, 60, 250, 80, 300]);
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Never);
        assert!(findings.is_empty());
    }

    #[test]
    fn http_heuristic_always_mode_reclassifies_unconditionally() {
        let events = http_same_id_events(&[100, 100, 100, 100, 100, 100, 100]);
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Always);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneHttp);
        assert_eq!(
            findings[0].classification_method,
            Some(ClassificationMethod::SanitizerHeuristic)
        );
    }

    #[test]
    fn http_heuristic_strict_placeholder_plus_variance() {
        let events = http_same_id_events(&[100, 50, 200, 60, 250, 80, 300]);
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].finding_type, FindingType::NPlusOneHttp);
    }

    #[test]
    fn http_heuristic_strict_no_placeholder_no_variance_no_finding() {
        let events: Vec<SpanEvent> = (0..7)
            .map(|i| {
                make_http_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "http://svc:5000/api/health",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    100,
                )
            })
            .collect();
        let trace = make_trace(events);
        let findings = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(findings.is_empty());
    }
}
