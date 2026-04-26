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
//! one distinct empty params slice and never fires; the redundant
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
//!    A positive match is treated as strong evidence of N+1.
//! 3. [`timing_variance_suggests_n_plus_one`]: when the scope signal is
//!    absent, fall back to the coefficient of variation of `duration_us`.
//!    True N+1 hits different rows with different cache states, so the
//!    spread is wider; cached redundant calls cluster tightly. Threshold
//!    `0.5` is empirical.
//!
//! The configurable [`SanitizerAwareMode`] gates final emission:
//! `Auto` (default) requires `LikelyNPlusOne`, `Always` always emits,
//! `Never` keeps pre-0.5.7 behavior.

use crate::normalize::NormalizedEvent;

/// How aggressively to reclassify sanitizer-collapsed SQL groups as N+1.
///
/// Wired from `[detection] sanitizer_aware_classification` in
/// `.perf-sentinel.toml`. Default is [`SanitizerAwareMode::Auto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SanitizerAwareMode {
    /// Reclassify only when the heuristic returns
    /// [`SanitizerVerdict::LikelyNPlusOne`].
    #[default]
    Auto,
    /// Reclassify any sanitized group with `>= threshold` occurrences.
    /// Most aggressive: may flag a single-param redundancy as N+1.
    Always,
    /// Disable the heuristic entirely. Reproduces pre-0.5.7 behavior.
    Never,
}

impl SanitizerAwareMode {
    /// Parse the TOML string. Unknown values warn and fall back to
    /// [`SanitizerAwareMode::Auto`].
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") => Self::Auto,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "auto" => Self::Auto,
                "always" => Self::Always,
                "never" => Self::Never,
                _ => {
                    tracing::warn!(
                        value = raw,
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
/// entry per literal; a sanitized N+1 has `params == []` on every span.
#[must_use]
pub fn looks_sanitized(spans: &[&NormalizedEvent]) -> bool {
    !spans.is_empty()
        && spans
            .iter()
            .all(|s| s.params.is_empty() && s.template.contains('?'))
}

/// Returns `true` when any of the supplied instrumentation scopes contains
/// an ORM marker (case-insensitive).
#[must_use]
pub fn has_orm_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|scope| {
        let lower = scope.to_ascii_lowercase();
        ORM_SCOPE_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
    })
}

/// Aggregate the instrumentation scopes from every span in a group,
/// deduplicated. Spans in a single ORM-induced N+1 share the same scope
/// chain, so most groups produce a single-entry vector, but the helper
/// stays defensive against multi-source aggregation.
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
/// different rows with different cache states, so durations spread out;
/// truly redundant calls hit the same cache lines and cluster tightly.
///
/// Requires at least 3 spans for a stable variance estimate. Returns
/// `false` for fewer spans, zero mean, or empty input.
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

/// Combined verdict: ORM scope wins (high-confidence reclassification),
/// otherwise fall back to timing variance.
#[must_use]
pub fn classify_sanitized_sql_group(
    spans: &[&NormalizedEvent],
    scopes: &[String],
) -> SanitizerVerdict {
    if has_orm_scope(scopes) {
        return SanitizerVerdict::LikelyNPlusOne;
    }
    if timing_variance_suggests_n_plus_one(spans) {
        return SanitizerVerdict::LikelyNPlusOne;
    }
    SanitizerVerdict::Inconclusive
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
    fn timing_variance_high_cv_returns_true() {
        // Dispersed durations: cache cold/warm states across N+1 row
        // lookups. CV ~ 0.68 on this set.
        let durations = [100u64, 50, 200, 60, 250, 80, 300, 70, 150, 400];
        let events: Vec<SpanEvent> = durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                sanitized_event_with_scope(
                    &format!("span-{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    *d,
                )
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
        let refs: Vec<&NormalizedEvent> = normalized.iter().collect();
        assert!(timing_variance_suggests_n_plus_one(&refs));
    }

    #[test]
    fn timing_variance_low_cv_returns_false() {
        let durations = [100u64, 102, 98, 101, 99, 100, 101, 99, 100, 102];
        let events: Vec<SpanEvent> = durations
            .iter()
            .enumerate()
            .map(|(i, d)| {
                sanitized_event_with_scope(
                    &format!("span-{i}"),
                    &format!("2025-07-10T14:32:01.{:03}Z", i * 30),
                    *d,
                )
            })
            .collect();
        let normalized: Vec<NormalizedEvent> = events.into_iter().map(normalize_one).collect();
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
}
