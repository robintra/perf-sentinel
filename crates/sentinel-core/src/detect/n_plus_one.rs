//! N+1 detection for SQL queries and HTTP calls.
//!
//! Detects patterns where the same normalized template is called N times
//! with different parameters within a single trace, inside a configurable
//! time window.

use std::collections::{HashMap, HashSet};

use crate::correlate::Trace;
use crate::event::EventType;

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
/// finding, `None` to skip the group. Two emit paths:
/// - direct rule (`distinct_params >= threshold`): returns
///   `(distinct_params, None)`.
/// - sanitizer heuristic (SQL only, gated on `mode`): returns
///   `(1, Some(ClassificationMethod::SanitizerHeuristic))`.
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
    if mode == SanitizerAwareMode::Never
        || *event_type != EventType::Sql
        || !sanitizer_aware::looks_sanitized_indexed(&trace.spans, indices)
    {
        return None;
    }
    // `Always` short-circuits the verdict computation entirely: the
    // mode emits on every sanitized group regardless of signal, so
    // running `has_orm_scope` and `timing_variance_suggests_n_plus_one`
    // would only allocate and discard.
    if mode == SanitizerAwareMode::Always {
        return Some((1, Some(ClassificationMethod::SanitizerHeuristic)));
    }
    let verdict =
        sanitizer_aware::classify_sanitized_sql_group_indexed(&trace.spans, indices, mode);
    matches!(verdict, SanitizerVerdict::LikelyNPlusOne)
        .then_some((1, Some(ClassificationMethod::SanitizerHeuristic)))
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
    use crate::test_helpers::{make_http_event, make_sql_event, make_trace};

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
        // Reproduces the simulation-lab redundant_sql case: 15 identical
        // queries via Spring Data JPA, all served from the same cached
        // row. ORM scope present but timing tight. Auto would flip this
        // to n_plus_one_sql, Strict must let the redundant detector
        // pick it up.
        let durations = [100u64; 15];
        let events = crate::test_helpers::make_sanitized_n_plus_one_events(
            15,
            Some("io.opentelemetry.hibernate-6.0"),
            Some(&durations),
        );
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, 5, 500, SanitizerAwareMode::Strict);
        assert!(
            n_plus_one.is_empty(),
            "Strict must not reclassify when timing variance is flat: got {:?}",
            n_plus_one
                .iter()
                .map(|f| &f.finding_type)
                .collect::<Vec<_>>()
        );
        let redundant = crate::detect::redundant::detect_redundant(&trace, &n_plus_one);
        assert_eq!(redundant.len(), 1);
        assert_eq!(redundant[0].finding_type, FindingType::RedundantSql);
        assert_eq!(redundant[0].pattern.occurrences, 15);
        // The redundant detector must not stamp the heuristic marker on
        // its own findings, even when Strict declined to reclassify.
        assert_eq!(redundant[0].classification_method, None);
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
}
