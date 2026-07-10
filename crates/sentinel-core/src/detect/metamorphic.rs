//! Metamorphic property tests for the detection stage.
//!
//! Ground-truth labels are expensive; invariants are not. Each property
//! asserts a *relation* between detection runs on transformed inputs
//! instead of an expected output, so the whole detector logic is
//! exercised on thousands of generated workloads without a single
//! hand-labeled corpus:
//!
//! - **amplification**: growing an already-detected N+1 group can only
//!   strengthen the finding, never weaken or drop it
//! - **permutation**: span arrival order never changes what is found
//! - **duplication**: a duplicated trace exactly doubles per-class findings
//! - **additivity**: per-trace detection over a set equals the union of
//!   detections per trace - the structural guarantee behind "sampling
//!   whole traces upstream never *creates* findings". Deliberately NOT
//!   covered: [`super::run_full_detection`]'s cross-trace percentile
//!   detector ([`super::slow::detect_slow_cross_trace`]), which is
//!   non-additive by design (p50/p95/p99 shift with the population);
//!   that exclusion is itself pinned by
//!   `cross_trace_slow_findings_are_not_additive` below.
//! - **silence**: below-threshold workloads never emit
//! - **span removal**: dropping spans (collector loss) never creates a
//!   finding nor inflates occurrences, under the strict distinct-params
//!   rule (`SanitizerAwareMode::Never`; the sanitizer/timing heuristics
//!   are intentionally out of scope - their verdicts depend on group
//!   composition, so span removal can legitimately flip them)
//! - **exclusivity**: a template never appears in both the N+1 and the
//!   redundant finding sets for the same trace, under any
//!   `SanitizerAwareMode` and any mix of duplicate / distinct /
//!   sanitized workloads
//! - **auto reclassification (HTTP)**: under `Auto`, span removal may
//!   flip a group between the direct rule and the timing-variance
//!   heuristic, but it never invents a template the workload does not
//!   contain, never counts more occurrences than spans present, and
//!   never splits one group into several findings
//! - **sanitizer non-monotonicity (pinned)**: the known corner where
//!   removing a span CAN create a finding under `Auto`, kept as a
//!   characterization test so the trade-off stays documented
//!
//! The amplification, permutation, silence, and span-removal properties
//! run twice, on SQL workloads and on their HTTP twins (`*_http_*`):
//! `detect_n_plus_one` shares one grouping path for both event types
//! but classifies them through different heuristics.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;

use super::n_plus_one::{CRITICAL_OCCURRENCE_THRESHOLD, detect_n_plus_one};
use super::redundant::detect_redundant;
use super::sanitizer_aware::SanitizerAwareMode;
use super::{
    ClassificationMethod, DetectConfig, Finding, FindingType, Severity, detect, run_full_detection,
};
use crate::correlate::Trace;
use crate::event::SpanEvent;
use crate::test_helpers::{
    make_http_event, make_http_event_with_duration, make_sanitized_n_plus_one_events,
    make_sql_event, make_sql_event_with_duration, make_trace,
};

const THRESHOLD: u32 = 5;
const WINDOW_MS: u64 = 500;

fn default_config() -> DetectConfig {
    DetectConfig {
        n_plus_one_threshold: THRESHOLD,
        window_ms: WINDOW_MS,
        slow_threshold_ms: 500,
        slow_min_occurrences: 3,
        max_fanout: 20,
        chatty_service_min_calls: 15,
        pool_saturation_concurrent_threshold: 10,
        serialized_min_sequential: 3,
        sanitizer_aware_classification: SanitizerAwareMode::default(),
    }
}

/// One same-template SQL series: `count` spans on `table`, distinct ids
/// (distinct params, so only the direct classification rule can fire),
/// `stride_ms` apart starting at `start_ms`. Generator bounds keep every
/// series inside `WINDOW_MS` (max 30 spans x 10 ms stride + 100 ms offset).
fn sql_series(table: &str, count: usize, stride_ms: usize, start_ms: usize) -> Vec<SpanEvent> {
    (0..count)
        .map(|i| {
            make_sql_event(
                "trace-p",
                &format!("span-{table}-{i}"),
                &format!("SELECT * FROM {table} WHERE id = {}", i + 1),
                &format!("2025-07-10T14:32:01.{:03}Z", start_ms + i * stride_ms),
            )
        })
        .collect()
}

/// A mixed workload: one `users` series above/below threshold, one
/// `orders` series, plus an unrelated single statement as noise.
fn mixed_workload() -> impl Strategy<Value = Vec<SpanEvent>> {
    (1usize..=12, 0usize..=8, 1usize..=10).prop_map(|(n_users, n_orders, stride)| {
        let mut events = sql_series("users", n_users, stride, 0);
        events.extend(sql_series("orders", n_orders, stride, 100));
        events.push(make_sql_event(
            "trace-p",
            "span-noise",
            "INSERT INTO logs (msg) VALUES ('x')",
            "2025-07-10T14:32:01.400Z",
        ));
        events
    })
}

/// One same-template HTTP series: `count` GETs on `/api/{stub}/{i}`,
/// distinct path ids (distinct params), `stride_ms` apart starting at
/// `start_ms`. Same in-window generator bounds as [`sql_series`].
fn http_series(stub: &str, count: usize, stride_ms: usize, start_ms: usize) -> Vec<SpanEvent> {
    (0..count)
        .map(|i| {
            make_http_event(
                "trace-p",
                &format!("span-{stub}-{i}"),
                &format!("http://svc:5000/api/{stub}/{}", i + 1),
                &format!("2025-07-10T14:32:01.{:03}Z", start_ms + i * stride_ms),
            )
        })
        .collect()
}

/// HTTP twin of [`mixed_workload`]: two `{id}`-templated series plus an
/// unrelated parameterless call as noise.
fn mixed_http_workload() -> impl Strategy<Value = Vec<SpanEvent>> {
    (1usize..=12, 0usize..=8, 1usize..=10).prop_map(|(n_items, n_orders, stride)| {
        let mut events = http_series("items", n_items, stride, 0);
        events.extend(http_series("orders", n_orders, stride, 100));
        events.push(make_http_event(
            "trace-p",
            "span-noise",
            "http://svc:5000/api/health",
            "2025-07-10T14:32:01.400Z",
        ));
        events
    })
}

fn any_mode() -> impl Strategy<Value = SanitizerAwareMode> {
    prop_oneof![
        Just(SanitizerAwareMode::Never),
        Just(SanitizerAwareMode::Auto),
        Just(SanitizerAwareMode::Strict),
        Just(SanitizerAwareMode::Always),
    ]
}

/// Workload mixing every classification path on overlapping templates:
/// an exact-duplicate SQL series (redundant candidate), a distinct-params
/// series (direct N+1 candidate), and an `OTel`-sanitized series
/// (heuristic candidate, ORM scope toggled) all share the `order_items /
/// order_id` template and merge into one group; a duplicate HTTP series
/// and a noise statement stay separate.
fn exclusivity_workload() -> impl Strategy<Value = (Vec<SpanEvent>, SanitizerAwareMode)> {
    (
        0usize..=8,
        0usize..=8,
        0usize..=8,
        0usize..=6,
        1usize..=6,
        any::<bool>(),
        any_mode(),
    )
        .prop_map(
            |(n_dup, n_distinct, n_sanitized, n_http, stride, orm, mode)| {
                let mut events = Vec::new();
                for i in 0..n_dup {
                    events.push(make_sql_event(
                        "trace-p",
                        &format!("span-dup-{i}"),
                        "SELECT * FROM order_items WHERE order_id = 7",
                        &format!("2025-07-10T14:32:01.{:03}Z", i * stride),
                    ));
                }
                for i in 0..n_distinct {
                    events.push(make_sql_event(
                        "trace-p",
                        &format!("span-distinct-{i}"),
                        &format!("SELECT * FROM order_items WHERE order_id = {}", 100 + i),
                        &format!("2025-07-10T14:32:01.{:03}Z", 100 + i * stride),
                    ));
                }
                let scope = orm.then_some("io.opentelemetry.spring-data-jpa-3.0");
                events.extend(make_sanitized_n_plus_one_events(n_sanitized, scope, None));
                for i in 0..n_http {
                    events.push(make_http_event(
                        "trace-p",
                        &format!("span-http-{i}"),
                        "http://svc:5000/api/items/7",
                        &format!("2025-07-10T14:32:01.{:03}Z", 300 + i * stride),
                    ));
                }
                events.push(make_sql_event(
                    "trace-p",
                    "span-noise",
                    "INSERT INTO logs (msg) VALUES ('x')",
                    "2025-07-10T14:32:01.450Z",
                ));
                (events, mode)
            },
        )
}

fn clone_with_trace_id(events: &[SpanEvent], trace_id: &str) -> Vec<SpanEvent> {
    events
        .iter()
        .cloned()
        .map(|mut event| {
            event.trace_id = trace_id.to_string();
            event
        })
        .collect()
}

/// Order-independent comparison key: everything that identifies a finding
/// except span-order-dependent context (source endpoint / code location
/// come from the group's first span in input order, legitimately).
fn finding_key(finding: &Finding) -> String {
    format!(
        "{:?}|{}|{}|{}|{:?}",
        finding.finding_type,
        finding.pattern.template,
        finding.pattern.occurrences,
        finding.pattern.distinct_params,
        finding.severity,
    )
}

fn sorted_keys(findings: &[Finding]) -> Vec<String> {
    let mut keys: Vec<String> = findings.iter().map(finding_key).collect();
    keys.sort();
    keys
}

fn key_counts(findings: &[Finding]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for finding in findings {
        *counts.entry(finding_key(finding)).or_insert(0) += 1;
    }
    counts
}

proptest! {
    /// Amplification: appending more distinct-param occurrences to a group
    /// already past the threshold keeps exactly one finding, counts every
    /// occurrence, and never de-escalates severity.
    #[test]
    fn growing_an_n_plus_one_group_preserves_or_strengthens(
        base in (THRESHOLD as usize)..=20,
        extra in 1usize..=10,
        stride in 1usize..=10,
    ) {
        let before = make_trace(sql_series("users", base, stride, 0));
        let after = make_trace(sql_series("users", base + extra, stride, 0));

        let f_before = detect_n_plus_one(&before, THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        let f_after = detect_n_plus_one(&after, THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);

        prop_assert_eq!(f_before.len(), 1);
        prop_assert_eq!(f_after.len(), 1);
        prop_assert_eq!(f_before[0].pattern.occurrences, base);
        prop_assert_eq!(f_after[0].pattern.occurrences, base + extra);
        // Severity is anchored on the occurrence count: growth may escalate
        // Warning -> Critical but must never do the reverse.
        if f_before[0].severity == Severity::Critical {
            prop_assert_eq!(&f_after[0].severity, &Severity::Critical);
        }
        if base + extra >= CRITICAL_OCCURRENCE_THRESHOLD {
            prop_assert_eq!(&f_after[0].severity, &Severity::Critical);
        }
    }

    /// Permutation: span arrival order never changes what is found.
    /// (Windows come from min/max timestamps and grouping is by template,
    /// so any order sensitivity here is a detector bug.)
    #[test]
    fn findings_invariant_under_span_permutation(
        (original, shuffled) in mixed_workload()
            .prop_flat_map(|events| (Just(events.clone()), Just(events).prop_shuffle())),
    ) {
        let f_original = detect_n_plus_one(
            &make_trace(original), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        let f_shuffled = detect_n_plus_one(
            &make_trace(shuffled), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        prop_assert_eq!(sorted_keys(&f_original), sorted_keys(&f_shuffled));
    }

    /// Duplication: feeding the same spans again under a new trace id
    /// exactly doubles every per-class finding count - no cross-trace
    /// leakage between per-trace detectors.
    #[test]
    fn duplicating_a_trace_doubles_per_class_findings(events in mixed_workload()) {
        let config = default_config();
        let solo = detect(&[make_trace(events.clone())], &config);
        let duo = detect(
            &[
                make_trace(events.clone()),
                make_trace(clone_with_trace_id(&events, "trace-q")),
            ],
            &config,
        );

        let solo_counts = key_counts(&solo);
        let duo_counts = key_counts(&duo);
        prop_assert_eq!(duo_counts.len(), solo_counts.len());
        for (key, count) in &solo_counts {
            prop_assert_eq!(duo_counts.get(key), Some(&(count * 2)), "class {}", key);
        }
    }

    /// Additivity: per-trace detection over a set is exactly the union of
    /// per-trace detections. This is the structural guarantee behind
    /// "head/tail-sampling whole traces upstream never creates findings"
    /// (the lab's sampling-degradation monotone gate, proven at the unit
    /// level). Cross-trace percentile detection is excluded on purpose.
    #[test]
    fn per_trace_detection_is_additive(
        workloads in prop::collection::vec(mixed_workload(), 2..=4),
    ) {
        let config = default_config();
        let traces: Vec<Trace> = workloads
            .iter()
            .enumerate()
            .map(|(i, events)| make_trace(clone_with_trace_id(events, &format!("trace-{i}"))))
            .collect();

        let combined = detect(&traces, &config);
        let mut separate = Vec::new();
        for trace in &traces {
            separate.extend(detect(std::slice::from_ref(trace), &config));
        }

        // Keys extended with the trace id: additivity must hold per trace,
        // not just in aggregate.
        let tag = |findings: &[Finding]| {
            let mut keys: Vec<String> = findings
                .iter()
                .map(|f| format!("{}|{}", f.trace_id, finding_key(f)))
                .collect();
            keys.sort();
            keys
        };
        prop_assert_eq!(tag(&combined), tag(&separate));
    }

    /// Silence: a workload where no template reaches the threshold must
    /// produce no N+1 finding at all.
    #[test]
    fn below_threshold_workloads_stay_silent(
        count in 1usize..(THRESHOLD as usize),
        stride in 1usize..=10,
    ) {
        let trace = make_trace(sql_series("users", count, stride, 0));
        let findings = detect_n_plus_one(&trace, THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        prop_assert!(findings.is_empty(), "found {:?}", sorted_keys(&findings));
    }

    /// Span removal (collector loss): dropping any subset of spans never
    /// creates a finding for a template the full trace did not flag, and
    /// never inflates occurrences. Scoped to the strict distinct-params
    /// rule (`Never`): the sanitizer/timing heuristics judge group
    /// composition, so removal can legitimately flip their verdicts.
    #[test]
    fn removing_spans_never_creates_or_inflates_findings(
        (events, keep_mask) in mixed_workload().prop_flat_map(|events| {
            let len = events.len();
            (Just(events), prop::collection::vec(any::<bool>(), len))
        }),
    ) {
        let kept: Vec<SpanEvent> = events
            .iter()
            .zip(&keep_mask)
            .filter(|(_, keep)| **keep)
            .map(|(event, _)| event.clone())
            .collect();
        prop_assume!(!kept.is_empty());

        let f_full = detect_n_plus_one(
            &make_trace(events), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Never);
        let f_kept = detect_n_plus_one(
            &make_trace(kept), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Never);

        prop_assert!(f_kept.len() <= f_full.len());
        for finding in &f_kept {
            let full_match = f_full.iter().find(|f| {
                f.finding_type == finding.finding_type
                    && f.pattern.template == finding.pattern.template
            });
            prop_assert!(
                full_match.is_some(),
                "finding appeared only after span removal: {}",
                finding.pattern.template
            );
            prop_assert!(
                finding.pattern.occurrences <= full_match.unwrap().pattern.occurrences
            );
        }
    }

    /// HTTP twin of the amplification property.
    #[test]
    fn growing_an_http_n_plus_one_group_preserves_or_strengthens(
        base in (THRESHOLD as usize)..=20,
        extra in 1usize..=10,
        stride in 1usize..=10,
    ) {
        let before = make_trace(http_series("items", base, stride, 0));
        let after = make_trace(http_series("items", base + extra, stride, 0));

        let f_before = detect_n_plus_one(&before, THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        let f_after = detect_n_plus_one(&after, THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);

        prop_assert_eq!(f_before.len(), 1);
        prop_assert_eq!(f_after.len(), 1);
        prop_assert_eq!(f_before[0].pattern.occurrences, base);
        prop_assert_eq!(f_after[0].pattern.occurrences, base + extra);
        if f_before[0].severity == Severity::Critical {
            prop_assert_eq!(&f_after[0].severity, &Severity::Critical);
        }
        if base + extra >= CRITICAL_OCCURRENCE_THRESHOLD {
            prop_assert_eq!(&f_after[0].severity, &Severity::Critical);
        }
    }

    /// HTTP twin of the permutation property.
    #[test]
    fn http_findings_invariant_under_span_permutation(
        (original, shuffled) in mixed_http_workload()
            .prop_flat_map(|events| (Just(events.clone()), Just(events).prop_shuffle())),
    ) {
        let f_original = detect_n_plus_one(
            &make_trace(original), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        let f_shuffled = detect_n_plus_one(
            &make_trace(shuffled), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        prop_assert_eq!(sorted_keys(&f_original), sorted_keys(&f_shuffled));
    }

    /// HTTP twin of the silence property.
    #[test]
    fn below_threshold_http_workloads_stay_silent(
        count in 1usize..(THRESHOLD as usize),
        stride in 1usize..=10,
    ) {
        let trace = make_trace(http_series("items", count, stride, 0));
        let findings = detect_n_plus_one(&trace, THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        prop_assert!(findings.is_empty(), "found {:?}", sorted_keys(&findings));
    }

    /// HTTP twin of the span-removal property, scoped to `Never` for the
    /// same reason as the SQL variant: the HTTP timing-variance heuristic
    /// judges group composition, so removal can legitimately flip it.
    #[test]
    fn removing_http_spans_never_creates_or_inflates_findings(
        (events, keep_mask) in mixed_http_workload().prop_flat_map(|events| {
            let len = events.len();
            (Just(events), prop::collection::vec(any::<bool>(), len))
        }),
    ) {
        let kept: Vec<SpanEvent> = events
            .iter()
            .zip(&keep_mask)
            .filter(|(_, keep)| **keep)
            .map(|(event, _)| event.clone())
            .collect();
        prop_assume!(!kept.is_empty());

        let f_full = detect_n_plus_one(
            &make_trace(events), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Never);
        let f_kept = detect_n_plus_one(
            &make_trace(kept), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Never);

        prop_assert!(f_kept.len() <= f_full.len());
        for finding in &f_kept {
            let full_match = f_full.iter().find(|f| {
                f.finding_type == finding.finding_type
                    && f.pattern.template == finding.pattern.template
            });
            prop_assert!(
                full_match.is_some(),
                "finding appeared only after span removal: {}",
                finding.pattern.template
            );
            prop_assert!(
                finding.pattern.occurrences <= full_match.unwrap().pattern.occurrences
            );
        }
    }

    /// Exclusivity: `detect_redundant` receives the N+1 findings so a
    /// template already classified as N+1 (direct rule or heuristic) is
    /// never double-reported as redundant, whatever the mode and the mix
    /// of duplicate / distinct / sanitized spans sharing that template.
    #[test]
    fn n_plus_one_and_redundant_never_share_a_template(
        (events, mode) in exclusivity_workload(),
    ) {
        let trace = make_trace(events);
        let n_plus_one = detect_n_plus_one(&trace, THRESHOLD, WINDOW_MS, mode);
        let redundant = detect_redundant(&trace, &n_plus_one);

        let claimed: HashSet<(&FindingType, &str)> = n_plus_one
            .iter()
            .map(|f| (&f.finding_type, f.pattern.template.as_str()))
            .collect();
        for finding in &redundant {
            // detect_redundant only emits Redundant{Sql,Http}.
            let twin = match finding.finding_type {
                FindingType::RedundantSql => FindingType::NPlusOneSql,
                _ => FindingType::NPlusOneHttp,
            };
            prop_assert!(
                !claimed.contains(&(&twin, finding.pattern.template.as_str())),
                "template classified both n+1 and redundant: {}",
                finding.pattern.template
            );
        }
    }

    /// Characterization (Auto, HTTP): one template, few distinct path
    /// ids, spread durations. Span removal may flip the group between
    /// the direct distinct-params rule and the timing-variance heuristic,
    /// or silence it - that flip is legitimate and NOT asserted against.
    /// What must hold: at most one finding per run, only the workload's
    /// template, never more occurrences than spans, and a classification
    /// that is either direct (`None`) or the heuristic.
    #[test]
    fn http_auto_reclassification_never_invents_templates(
        (ids, durations, keep_mask) in (6usize..=18).prop_flat_map(|n| (
            prop::collection::vec(1usize..=8, n),
            prop::collection::vec(100u64..=100_000, n),
            prop::collection::vec(any::<bool>(), n),
        )),
    ) {
        let events: Vec<SpanEvent> = ids
            .iter()
            .zip(&durations)
            .enumerate()
            .map(|(i, (id, duration))| {
                make_http_event_with_duration(
                    "trace-p",
                    &format!("span-{i}"),
                    &format!("http://svc:5000/api/items/{id}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                    *duration,
                )
            })
            .collect();
        let kept: Vec<SpanEvent> = events
            .iter()
            .zip(&keep_mask)
            .filter(|(_, keep)| **keep)
            .map(|(event, _)| event.clone())
            .collect();
        prop_assume!(!kept.is_empty());

        let kept_len = kept.len();
        let full = detect_n_plus_one(
            &make_trace(events.clone()), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);
        let partial = detect_n_plus_one(
            &make_trace(kept), THRESHOLD, WINDOW_MS, SanitizerAwareMode::Auto);

        prop_assert!(full.len() <= 1);
        prop_assert!(partial.len() <= 1);
        for (findings, span_count) in [(&full, events.len()), (&partial, kept_len)] {
            for finding in findings {
                prop_assert_eq!(finding.pattern.template.as_str(), "GET /api/items/{id}");
                prop_assert!(finding.pattern.occurrences <= span_count);
                prop_assert!(matches!(
                    finding.classification_method,
                    None | Some(ClassificationMethod::SanitizerHeuristic)
                ));
            }
        }
    }
}

/// Characterization: the sanitizer heuristic is deliberately non-monotone
/// under span removal, which is why the removal properties above are
/// scoped to `SanitizerAwareMode::Never`. A mixed group (sanitized spans
/// plus one span carrying an extracted literal) fails `looks_sanitized`,
/// so the whole group stays silent; dropping the literal-carrying span
/// leaves a uniformly sanitized group whose ORM scope flips the verdict
/// to `LikelyNPlusOne`. Pinned so the corner stays a documented trade-off
/// instead of resurfacing as a proptest failure.
#[test]
fn sanitizer_heuristic_can_fire_after_span_removal() {
    let mut events =
        make_sanitized_n_plus_one_events(6, Some("io.opentelemetry.spring-data-jpa-3.0"), None);
    events.push(make_sql_event(
        "trace-1",
        "span-literal",
        "SELECT * FROM order_items WHERE order_id = 42",
        "2025-07-10T14:32:01.300Z",
    ));

    let full = detect_n_plus_one(
        &make_trace(events.clone()),
        THRESHOLD,
        WINDOW_MS,
        SanitizerAwareMode::Auto,
    );
    assert!(
        full.is_empty(),
        "mixed group must stay silent: {:?}",
        sorted_keys(&full)
    );

    events.pop();
    let after_removal = detect_n_plus_one(
        &make_trace(events),
        THRESHOLD,
        WINDOW_MS,
        SanitizerAwareMode::Auto,
    );
    assert_eq!(after_removal.len(), 1);
    assert_eq!(
        after_removal[0].classification_method,
        Some(ClassificationMethod::SanitizerHeuristic)
    );
    assert_eq!(after_removal[0].pattern.occurrences, 6);
}

/// Characterization: cross-trace slow detection is non-additive by
/// design, which is why the additivity property above targets `detect`
/// and the lab's sampling-degradation gate asserts *total* findings
/// only. Two slow spans per trace stay below `slow_min_occurrences` (3)
/// in isolation, and `run_full_detection` skips cross-trace analysis
/// for a single trace - but the combined population reaches 4
/// occurrences across 2 traces with p99 above the threshold, so a
/// `slow_sql` finding exists in the combined run that no per-trace run
/// contains.
#[test]
fn cross_trace_slow_findings_are_not_additive() {
    let slow_span = |trace: &str, span: &str, id: usize, ts: &str| {
        make_sql_event_with_duration(
            trace,
            span,
            &format!("SELECT * FROM big_table WHERE id = {id}"),
            ts,
            600_000,
        )
    };
    let trace_a = make_trace(vec![
        slow_span("trace-a", "a1", 1, "2025-07-10T14:32:01.000Z"),
        slow_span("trace-a", "a2", 2, "2025-07-10T14:32:01.050Z"),
    ]);
    let trace_b = make_trace(vec![
        slow_span("trace-b", "b1", 3, "2025-07-10T14:32:01.100Z"),
        slow_span("trace-b", "b2", 4, "2025-07-10T14:32:01.150Z"),
    ]);
    let config = default_config();
    let slow_count = |findings: &[Finding]| {
        findings
            .iter()
            .filter(|f| matches!(f.finding_type, FindingType::SlowSql | FindingType::SlowHttp))
            .count()
    };

    let solo_a = run_full_detection(std::slice::from_ref(&trace_a), &config);
    let solo_b = run_full_detection(std::slice::from_ref(&trace_b), &config);
    let combined = run_full_detection(&[trace_a, trace_b], &config);

    assert_eq!(slow_count(&solo_a), 0);
    assert_eq!(slow_count(&solo_b), 0);
    assert_eq!(slow_count(&combined), 1);
}
