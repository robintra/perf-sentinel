//! Sanitizer-aware classification for SQL N+1 vs redundant.
//!
//! OpenTelemetry agents collapse SQL literals to `?` by default to keep
//! PII out of trace attributes. The sanitized statement
//! (`SELECT ... WHERE id = ?`) reaches perf-sentinel with the `?`
//! already in place, and `normalize_sql` leaves it as-is (it only
//! extracts numeric/string literals, not literal `?` placeholders). So
//! for an ORM-induced N+1 every span ends up with the same `template`
//! containing `?` and an empty `params` vector. The standard
//! `distinct_params >= threshold` check in [`super::n_plus_one`] sees
//! one distinct empty params slice and never fires, the redundant
//! detector then groups all the spans together and misclassifies them
//! as `redundant_sql`. This module provides the heuristic that recovers
//! the correct classification.
//!
//! Three signals, evaluated in order:
//! 1. [`looks_sanitized`]: every span has a `?` placeholder in its
//!    template and an empty `params` vector. Required to activate the
//!    heuristic at all.
//! 2. [`has_orm_scope`]: at least one OpenTelemetry instrumentation scope
//!    on the spans matches a known ORM marker (Hibernate, Spring Data,
//!    EF Core, `SQLAlchemy`, `ActiveRecord`, GORM, Prisma, Diesel, etc.).
//!    Markers are matched with a word-boundary check (preceded and
//!    followed by a non-alphanumeric byte), so `jpa` only fires on
//!    `spring-data-jpa` and friends, never on `myappjpastats`. A positive
//!    match is treated as strong evidence of N+1.
//! 3. [`timing_variance_suggests_n_plus_one`]: when the scope signal is
//!    absent, fall back to the coefficient of variation of `duration_us`.
//!    True N+1 hits different rows with different cache states, so the
//!    spread is wider, cached redundant calls cluster tightly. Threshold
//!    `0.5` is empirical.
//!
//! The configurable [`SanitizerAwareMode`] gates final emission:
//! `Auto` (default) requires `LikelyNPlusOne` from the OR-logic
//! classifier (either signal fires), `Strict` (0.5.8+) requires
//! `LikelyNPlusOne` from the AND-logic classifier (both signals fire
//! conjointly), `Always` always emits, `Never` keeps pre-0.5.7
//! behavior.
//!
//! Known limit: `looks_sanitized` cannot tell a sanitized literal `?`
//! apart from a `PostgreSQL` JSONB existence operator (`data ? 'key'`)
//! when the latter happens to appear in a query with no other
//! literals. The harm direction is asymmetric: a misclassified JSONB
//! group flips from `redundant_sql` to `n_plus_one_sql`, both of which
//! contribute equally to `GreenOps` `avoidable_io_ops`, only the
//! suggestion text differs.

use crate::normalize::NormalizedEvent;

/// How aggressively to reclassify sanitizer-collapsed SQL groups as N+1.
///
/// Wired from `[detection] sanitizer_aware_classification` in
/// `.perf-sentinel.toml`. Default is [`SanitizerAwareMode::Auto`].
///
/// Modes trade recall (catch more N+1) against precision (preserve
/// `redundant_sql` findings on legitimate repeated identical queries).
/// `Auto` favors recall, `Strict` favors precision, `Always` and `Never`
/// are the two ends of the dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SanitizerAwareMode {
    /// Reclassify when **either** the ORM scope signal **or** the timing
    /// variance signal fires. Default. Best recall on production stacks
    /// where the ORM scope is almost always present, at the cost of
    /// hiding `redundant_sql` findings on truly repeated identical
    /// queries served from cache.
    #[default]
    Auto,
    /// Reclassify any sanitized group with `>= threshold` occurrences.
    /// Most aggressive: may flag a single-param redundancy as N+1.
    Always,
    /// Disable the heuristic entirely. Reproduces pre-0.5.7 behavior.
    Never,
    /// Reclassify only when **both** signals fire conjointly: ORM scope
    /// present **and** per-span timing variance high enough to indicate
    /// distinct row lookups. Preserves `redundant_sql` precision on
    /// repeated identical queries served from cache, at the cost of
    /// missing N+1 on stacks where row-level cache makes timings cluster.
    ///
    /// The precision gain is most valuable on hot-path identical
    /// queries that **do** traverse an ORM scope (cache-warming loops,
    /// polling repositories, unmemoized `findById(sameId)` in legacy
    /// code), since `Auto` would mislabel those as `n_plus_one_sql`. On
    /// queries that bypass the ORM (raw JDBC, hand-rolled drivers) the
    /// scope signal is absent under both modes, so `Strict` and `Auto`
    /// behave identically (`Auto` falls through to the variance signal).
    Strict,
}

impl SanitizerAwareMode {
    /// Parse the TOML string. Unknown values warn and fall back to
    /// [`SanitizerAwareMode::Auto`].
    ///
    /// The warning emits a sanitized form of the offending value
    /// (control characters replaced, length capped at 32 bytes) so a
    /// stray credential-shaped string in the config file does not land
    /// verbatim in logs.
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") => Self::Auto,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "auto" => Self::Auto,
                "always" => Self::Always,
                "never" => Self::Never,
                "strict" => Self::Strict,
                _ => {
                    tracing::warn!(
                        value = sanitize_for_log(raw).as_ref(),
                        "unknown sanitizer_aware_classification value, defaulting to 'auto'"
                    );
                    Self::Auto
                }
            },
        }
    }

    /// Returns the lowercase string label used in the TOML config.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
            Self::Strict => "strict",
        }
    }
}

/// Verdict from the heuristic on a single sanitized SQL group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizerVerdict {
    /// Group looks like a sanitized N+1: emit `n_plus_one_sql`.
    LikelyNPlusOne,
    /// Heuristic could not gather enough signal: leave to the redundant
    /// detector.
    Inconclusive,
}

/// Instrumentation scope substrings that indicate an ORM is in the call
/// stack. Matched case-insensitively via `contains`, so vendor variants
/// (`io.opentelemetry.spring-data-3.0`, `io.opentelemetry.SpringData`)
/// both hit. List intentionally errs on the side of recall: a false
/// positive only swaps a `redundant_sql` finding for `n_plus_one_sql` on
/// a sanitized group, which is the harm-reduction direction.
const ORM_SCOPE_MARKERS: &[&str] = &[
    // Java / JVM
    "spring-data",
    "hibernate",
    "jpa",
    "micronaut-data",
    "jdbi",
    "r2dbc",
    // .NET
    "entityframeworkcore",
    "entity-framework",
    // Python
    "sqlalchemy",
    "django.db",
    // Ruby
    "active-record",
    "activerecord",
    // Go
    "gorm",
    "sqlx",
    // Node.js
    "sequelize",
    "prisma",
    "typeorm",
    "mongoose",
    // Rust
    "sea-orm",
    "diesel",
];

/// Returns `true` when every span in the group looks like the OpenTelemetry
/// SQL sanitizer collapsed its literals: the template carries at least
/// one `?` placeholder, and `params` is empty (because `normalize_sql`
/// only extracts literal numbers and strings, not pre-existing `?`
/// placeholders). A non-sanitized N+1 has `params` populated with one
/// entry per literal, a sanitized N+1 has `params == []` on every span.
///
/// See the module-level note for the JSONB `?` operator caveat.
#[must_use]
pub fn looks_sanitized(spans: &[&NormalizedEvent]) -> bool {
    !spans.is_empty()
        && spans
            .iter()
            .all(|s| s.params.is_empty() && s.template.contains('?'))
}

/// Index-based variant of [`looks_sanitized`] for the detection hot path.
/// Avoids materializing a `Vec<&NormalizedEvent>` before the cheap
/// per-span check, so the heavy `classify_sanitized_sql_group_indexed`
/// only runs on groups that already pass the fast gate.
pub(super) fn looks_sanitized_indexed(spans: &[NormalizedEvent], indices: &[usize]) -> bool {
    !indices.is_empty()
        && indices.iter().all(|&i| {
            let s = &spans[i];
            s.params.is_empty() && s.template.contains('?')
        })
}

/// Returns `true` when any of the supplied instrumentation scopes contains
/// an ORM marker. Matching is ASCII-case-insensitive and word-bounded:
/// the marker substring must be preceded and followed by a non-word
/// byte (anything that is not `[A-Za-z0-9_]`) or by the start/end of the
/// scope. This prevents `jpa` from firing on `myappjpastats` or `sqlx`
/// from firing on `mysqlxapackage`. Allocation-free.
#[must_use]
pub fn has_orm_scope(scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|scope| ORM_SCOPE_MARKERS.iter().any(|m| contains_marker(scope, m)))
}

/// ASCII case-insensitive substring search with word-boundary anchoring.
fn contains_marker(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return false;
    }
    h.windows(n.len()).enumerate().any(|(i, w)| {
        if !w.eq_ignore_ascii_case(n) {
            return false;
        }
        let before_ok = i == 0 || !is_word_byte(h[i - 1]);
        let after = i + n.len();
        let after_ok = after == h.len() || !is_word_byte(h[after]);
        before_ok && after_ok
    })
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Aggregate the instrumentation scopes from every span in a group,
/// deduplicated. Spans in a single ORM-induced N+1 share the same scope
/// chain, so most groups produce a single-entry vector. Bounded by the
/// per-span scope cap enforced at ingest (`event::cap_instrumentation_scopes`),
/// so the linear-scan dedup stays cheap.
#[must_use]
pub fn collect_scopes(spans: &[&NormalizedEvent]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for span in spans {
        for scope in &span.event.instrumentation_scopes {
            if !out.iter().any(|existing| existing == scope) {
                out.push(scope.clone());
            }
        }
    }
    out
}

/// Returns `true` when the coefficient of variation (std-dev / mean) of
/// the per-span `duration_us` values exceeds `0.5`. True N+1 hits
/// different rows with different cache states, so durations spread out,
/// truly redundant calls hit the same cache lines and cluster tightly.
///
/// Requires at least 3 spans for a stable variance estimate. Returns
/// `false` for fewer spans, zero mean, or empty input.
///
/// Asymmetric harm under `Auto`: a false positive flips a sanitized
/// `redundant_sql` group to `n_plus_one_sql` (same `avoidable_io_ops`
/// weight, the suggestion text differs), a false negative leaves a real
/// N+1 silent for the redundant detector to pick up. Threshold tuned to
/// favor false positives over silent misses.
///
/// Under `Strict` (0.5.8+) this signal becomes load-bearing: it is the
/// only gate that lets a sanitized group reach `LikelyNPlusOne` once
/// the ORM scope check has passed. A real ORM-induced N+1 against a
/// fully warm row cache (e.g. 100 lookups by primary key with all rows
/// in `shared_buffers`) can cluster within ±10% (CV ~ 0.1) and stay
/// silent under `Strict`. The 0.5 threshold is preserved across modes
/// pending empirical validation in the simulation lab. If lab traffic
/// shows the threshold to be too restrictive under `Strict`, the right
/// follow-up is exposing a `[detection] sanitizer_aware_min_cv` knob
/// rather than picking a new global default.
#[must_use]
pub fn timing_variance_suggests_n_plus_one(spans: &[&NormalizedEvent]) -> bool {
    if spans.len() < 3 {
        return false;
    }
    #[allow(clippy::cast_precision_loss)] // duration_us fits in f64 to ~9e15 µs
    let durations: Vec<f64> = spans.iter().map(|s| s.event.duration_us as f64).collect();
    #[allow(clippy::cast_precision_loss)]
    let n = durations.len() as f64;
    let mean = durations.iter().sum::<f64>() / n;
    if mean <= 0.0 {
        return false;
    }
    let variance = durations.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n;
    let cv = variance.sqrt() / mean;
    cv > 0.5
}

/// Combined verdict for `Auto` mode: ORM scope wins (high-confidence
/// reclassification), otherwise fall back to timing variance. Either
/// signal alone is enough to return `LikelyNPlusOne`.
#[must_use]
pub fn classify_sanitized_sql_group(
    spans: &[&NormalizedEvent],
    scopes: &[String],
) -> SanitizerVerdict {
    if has_orm_scope(scopes) || timing_variance_suggests_n_plus_one(spans) {
        SanitizerVerdict::LikelyNPlusOne
    } else {
        SanitizerVerdict::Inconclusive
    }
}

/// Combined verdict for `Strict` mode: requires the two signals to fire
/// **conjointly**. Preserves `redundant_sql` precision on cached
/// identical queries (where the ORM scope is present but the timing
/// variance is low), at the cost of missing N+1 patterns whose rows all
/// happen to be cache-warm.
#[must_use]
pub fn classify_sanitized_sql_group_strict(
    spans: &[&NormalizedEvent],
    scopes: &[String],
) -> SanitizerVerdict {
    if has_orm_scope(scopes) && timing_variance_suggests_n_plus_one(spans) {
        SanitizerVerdict::LikelyNPlusOne
    } else {
        SanitizerVerdict::Inconclusive
    }
}

/// Index-based variant for the detection hot path: borrows directly
/// from the trace's span vector without an intermediate
/// `Vec<&NormalizedEvent>`. Dispatches to the OR-logic
/// (`classify_sanitized_sql_group`) or AND-logic
/// (`classify_sanitized_sql_group_strict`) entry point based on `mode`.
///
/// `Always` and `Never` are filtered upstream in
/// [`super::n_plus_one`] so they never reach this dispatcher in
/// production, but the match stays exhaustive (no `_`) so a future
/// fifth variant on [`SanitizerAwareMode`] fails to compile here
/// rather than silently picking the OR fallback.
pub(super) fn classify_sanitized_sql_group_indexed(
    spans: &[NormalizedEvent],
    indices: &[usize],
    mode: SanitizerAwareMode,
) -> SanitizerVerdict {
    let group: Vec<&NormalizedEvent> = indices.iter().map(|&i| &spans[i]).collect();
    let scopes = collect_scopes(&group);
    match mode {
        SanitizerAwareMode::Strict => classify_sanitized_sql_group_strict(&group, &scopes),
        SanitizerAwareMode::Auto | SanitizerAwareMode::Always | SanitizerAwareMode::Never => {
            classify_sanitized_sql_group(&group, &scopes)
        }
    }
}

/// Sanitize an arbitrary user-controlled string for inclusion in a
/// `tracing` event: replace control characters with `_` and cap at 32
/// bytes (UTF-8 boundary preserved). Mirrors the project's pattern of
/// never letting raw config values reach the log surface unchecked.
fn sanitize_for_log(value: &str) -> std::borrow::Cow<'_, str> {
    const MAX_LEN: usize = 32;
    let truncated = if value.len() > MAX_LEN {
        let cut = value
            .char_indices()
            .take_while(|(i, _)| *i <= MAX_LEN)
            .last()
            .map_or(0, |(i, c)| i + c.len_utf8());
        &value[..cut.min(value.len())]
    } else {
        value
    };
    if truncated.chars().any(char::is_control) {
        std::borrow::Cow::Owned(
            truncated
                .chars()
                .map(|c| if c.is_control() { '_' } else { c })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SpanEvent;
    use crate::normalize;
    use crate::test_helpers::make_sql_event_with_duration;

    fn sanitized_event_with_scope(span_id: &str, ts: &str, duration_us: u64) -> SpanEvent {
        let mut e = make_sql_event_with_duration(
            "trace-1",
            span_id,
            "SELECT * FROM order_items WHERE order_id = ?",
            ts,
            duration_us,
        );
        e.instrumentation_scopes = vec!["io.opentelemetry.spring-data-jpa-3.0".to_string()];
        e
    }

    fn normalize_one(event: SpanEvent) -> NormalizedEvent {
        normalize::normalize_all(vec![event]).remove(0)
    }

    /// Build N normalized sanitized events with the supplied per-span
    /// `duration_us`, no scope. Shared by the timing-variance tests so
    /// the boilerplate (build / normalize / collect refs) stays in one
    /// place.
    fn sanitized_normalized_with_durations(durations: &[u64]) -> Vec<NormalizedEvent> {
        durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mut e = make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_items WHERE order_id = ?",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    *d,
                );
                e.instrumentation_scopes = Vec::new();
                normalize_one(e)
            })
            .collect()
    }

    #[test]
    fn from_config_parses_known_values() {
        assert_eq!(
            SanitizerAwareMode::from_config(None),
            SanitizerAwareMode::Auto
        );
        assert_eq!(
            SanitizerAwareMode::from_config(Some("auto")),
            SanitizerAwareMode::Auto
        );
        assert_eq!(
            SanitizerAwareMode::from_config(Some("ALWAYS")),
            SanitizerAwareMode::Always
        );
        assert_eq!(
            SanitizerAwareMode::from_config(Some(" Never ")),
            SanitizerAwareMode::Never
        );
        assert_eq!(
            SanitizerAwareMode::from_config(Some("strict")),
            SanitizerAwareMode::Strict
        );
        assert_eq!(
            SanitizerAwareMode::from_config(Some("STRICT")),
            SanitizerAwareMode::Strict
        );
    }

    #[test]
    fn as_str_round_trips_every_variant() {
        for mode in [
            SanitizerAwareMode::Auto,
            SanitizerAwareMode::Always,
            SanitizerAwareMode::Never,
            SanitizerAwareMode::Strict,
        ] {
            assert_eq!(SanitizerAwareMode::from_config(Some(mode.as_str())), mode);
        }
    }

    #[test]
    fn from_config_unknown_value_warns_and_defaults_to_auto() {
        // tracing::warn! is surfaced to stderr in tests; we only assert
        // the fallback behavior here. The warn macro itself is exercised
        // by invocation.
        assert_eq!(
            SanitizerAwareMode::from_config(Some("foo")),
            SanitizerAwareMode::Auto
        );
        assert_eq!(
            SanitizerAwareMode::from_config(Some("")),
            SanitizerAwareMode::Auto
        );
    }

    #[test]
    fn looks_sanitized_true_for_sanitized_template() {
        let events: Vec<SpanEvent> = (1..=3)
            .map(|i| {
                sanitized_event_with_scope(
                    &format!("span-{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 50),
                    100,
                )
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
        // normalize_sql leaves the literal `?` in the template and adds
        // nothing to params (it only extracts numeric/string literals).
        for event in &normalized {
            assert_eq!(event.params, Vec::<String>::new());
            assert!(event.template.contains('?'));
        }
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert!(looks_sanitized(&refs));
    }

    #[test]
    fn looks_sanitized_false_when_any_param_is_literal() {
        let mut e1 = sanitized_event_with_scope("span-1", "2025-07-10T14:32:01.000Z", 100);
        let mut e2 = sanitized_event_with_scope("span-2", "2025-07-10T14:32:01.050Z", 100);
        e1.target = "SELECT * FROM order_items WHERE order_id = ?".to_string();
        e2.target = "SELECT * FROM order_items WHERE order_id = 42".to_string();
        let normalized: Vec<NormalizedEvent> =
            vec![e1, e2].into_iter().map(normalize_one).collect();
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert!(!looks_sanitized(&refs));
    }

    #[test]
    fn looks_sanitized_false_when_template_has_no_placeholder() {
        // No literals at all (`SELECT NOW()`): template has no `?`,
        // params is empty. Must not be flagged as sanitized.
        let event = make_sql_event_with_duration(
            "trace-1",
            "span-1",
            "SELECT NOW()",
            "2025-07-10T14:32:01.000Z",
            100,
        );
        let normalized = normalize_one(event);
        let refs = vec![&normalized];
        assert!(!looks_sanitized(&refs));
    }

    #[test]
    fn has_orm_scope_matches_case_insensitively() {
        assert!(has_orm_scope(&[
            "io.opentelemetry.spring-data-3.0".to_string()
        ]));
        assert!(has_orm_scope(&[
            "IO.OPENTELEMETRY.HIBERNATE-ORM-6.0".to_string()
        ]));
        assert!(has_orm_scope(&["EntityFrameworkCore".to_string()]));
        assert!(has_orm_scope(&["opentelemetry.gorm.v1".to_string()]));
        assert!(!has_orm_scope(&["io.opentelemetry.jdbc-3.1".to_string()]));
        assert!(!has_orm_scope(&[]));
    }

    #[test]
    fn has_orm_scope_respects_word_boundary() {
        // Short markers like `jpa` and `sqlx` must not match arbitrary
        // substrings: a hostile or coincidental scope like
        // `mysqlxapackage` or `myappjpastats` must NOT trigger the
        // heuristic.
        assert!(!has_orm_scope(&["mysqlxapackage".to_string()]));
        assert!(!has_orm_scope(&["myappjpastats".to_string()]));
        assert!(!has_orm_scope(&["my-jpastore".to_string()]));
        assert!(!has_orm_scope(&["spring-database".to_string()]));
        // Real OTel scope shapes still match.
        assert!(has_orm_scope(&[
            "io.opentelemetry.spring-data-jpa-3.0".to_string()
        ]));
        assert!(has_orm_scope(&["io.opentelemetry.go.gorm.v1".to_string()]));
    }

    #[test]
    fn sanitize_for_log_redacts_control_chars_and_truncates() {
        assert_eq!(sanitize_for_log("ab\x00c\nd").as_ref(), "ab_c_d");
        assert_eq!(sanitize_for_log("abc").as_ref(), "abc");
        let long = "x".repeat(200);
        let out = sanitize_for_log(&long);
        assert!(
            out.len() <= 40,
            "expected truncation, got {} bytes",
            out.len()
        );
    }

    #[test]
    fn timing_variance_high_cv_returns_true() {
        // Dispersed durations: cache cold/warm states across N+1 row
        // lookups. CV ~ 0.68 on this set.
        let normalized =
            sanitized_normalized_with_durations(&[100, 50, 200, 60, 250, 80, 300, 70, 150, 400]);
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert!(timing_variance_suggests_n_plus_one(&refs));
    }

    #[test]
    fn timing_variance_low_cv_returns_false() {
        let normalized =
            sanitized_normalized_with_durations(&[100, 102, 98, 101, 99, 100, 101, 99, 100, 102]);
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert!(!timing_variance_suggests_n_plus_one(&refs));
    }

    #[test]
    fn timing_variance_too_few_spans_returns_false() {
        let events: Vec<SpanEvent> = (1u64..=2)
            .map(|i| {
                sanitized_event_with_scope(
                    &format!("span-{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    100 * i,
                )
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert!(!timing_variance_suggests_n_plus_one(&refs));
    }

    #[test]
    fn classify_returns_n_plus_one_when_orm_scope_present() {
        let events: Vec<SpanEvent> = (1..=10)
            .map(|i| {
                sanitized_event_with_scope(
                    &format!("span-{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 10),
                    100,
                )
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        let scopes = collect_scopes(&refs);
        assert_eq!(
            classify_sanitized_sql_group(&refs, &scopes),
            SanitizerVerdict::LikelyNPlusOne
        );
    }

    #[test]
    fn classify_returns_inconclusive_when_no_signal() {
        let durations = [100u64, 102, 98, 101, 99, 100, 101, 99, 100, 102];
        let events: Vec<SpanEvent> = durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mut e = make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_items WHERE order_id = ?",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    *d,
                );
                e.instrumentation_scopes = Vec::new();
                e
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        let scopes = collect_scopes(&refs);
        assert_eq!(
            classify_sanitized_sql_group(&refs, &scopes),
            SanitizerVerdict::Inconclusive
        );
    }

    // --- Strict mode (0.5.8+): both signals required ---

    /// Helper: build a sanitized group with explicit ORM scope and
    /// per-span durations, then return `(refs, scopes)` ready to feed
    /// into either classifier.
    fn build_sanitized_group_for_strict(
        scope: Option<&str>,
        durations: &[u64],
    ) -> (Vec<NormalizedEvent>, Vec<String>) {
        let events: Vec<SpanEvent> = durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mut e = make_sql_event_with_duration(
                    "trace-1",
                    &format!("span-{i}"),
                    "SELECT * FROM order_items WHERE order_id = ?",
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    *d,
                );
                e.instrumentation_scopes = scope.map(|s| vec![s.to_string()]).unwrap_or_default();
                e
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        let scopes = collect_scopes(&refs);
        (normalized, scopes)
    }

    #[test]
    fn strict_orm_scope_only_low_variance_returns_inconclusive() {
        // The simulation lab redundant_sql case: 15 identical SELECT
        // count(*) from a Spring Data JPA repository, all served from
        // the same cached row. ORM scope present, timing tight.
        // Auto would reclassify, Strict must not.
        let low_variance = [
            100u64, 102, 98, 101, 99, 100, 101, 99, 100, 102, 98, 101, 99, 100, 102,
        ];
        let (normalized, scopes) =
            build_sanitized_group_for_strict(Some("io.opentelemetry.hibernate-6.0"), &low_variance);
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert_eq!(
            classify_sanitized_sql_group_strict(&refs, &scopes),
            SanitizerVerdict::Inconclusive
        );
    }

    #[test]
    fn strict_orm_scope_and_high_variance_returns_likely_n_plus_one() {
        // Real ORM-induced N+1: 10 lookups against different rows, cache
        // hit/miss spread the durations. Both signals fire, Strict emits.
        let high_variance = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let (normalized, scopes) = build_sanitized_group_for_strict(
            Some("io.opentelemetry.spring-data-jpa-3.0"),
            &high_variance,
        );
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert_eq!(
            classify_sanitized_sql_group_strict(&refs, &scopes),
            SanitizerVerdict::LikelyNPlusOne
        );
    }

    #[test]
    fn strict_no_orm_scope_high_variance_returns_inconclusive() {
        // Variance alone is not enough under Strict: an N+1 from a
        // hand-rolled JDBC layer (no ORM marker on the scope chain) stays
        // unclassified. Auto would emit on the variance signal.
        let high_variance = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let (normalized, scopes) = build_sanitized_group_for_strict(None, &high_variance);
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert_eq!(
            classify_sanitized_sql_group_strict(&refs, &scopes),
            SanitizerVerdict::Inconclusive
        );
    }

    #[test]
    fn strict_no_signal_returns_inconclusive() {
        let low_variance = [100u64, 102, 98, 101, 99, 100, 101, 99, 100, 102];
        let (normalized, scopes) = build_sanitized_group_for_strict(None, &low_variance);
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert_eq!(
            classify_sanitized_sql_group_strict(&refs, &scopes),
            SanitizerVerdict::Inconclusive
        );
    }
}
