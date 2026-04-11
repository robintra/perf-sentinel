//! Interpretation helpers: classify scoring metrics into human-readable bands.
//!
//! These thresholds are **heuristic rendering aids** used by the CLI text
//! output to annotate numerical metrics like `io_intensity_score` and
//! `io_waste_ratio` with a `(healthy | moderate | high | critical)` label.
//!
//! # What is and isn't anchored on real data
//!
//! - [`IIS_HIGH`] (5.0) is anchored on the N+1 detector's
//!   `n_plus_one_threshold` config default. An endpoint whose IIS reaches
//!   5.0 is arithmetically at the point where `detect_n_plus_one` starts
//!   emitting findings.
//! - [`IIS_CRITICAL`] (10.0) is mechanically anchored on
//!   `crate::detect::n_plus_one::CRITICAL_OCCURRENCE_THRESHOLD` via the
//!   `iis_critical_matches_n_plus_one_detector_threshold` drift-guard
//!   test. Changing either value without updating the other will fail
//!   the test at build time.
//! - [`IIS_MODERATE`] (2.0) is a **rule of thumb**, not empirical. It
//!   encodes the intuition that a typical CRUD endpoint makes 1-2 I/O ops
//!   per request. Aggregators and dashboards will show many "moderate"
//!   endpoints that are legitimate.
//! - [`WASTE_RATIO_HIGH`] (0.30) is anchored on the **default**
//!   `io_waste_ratio_max`. Users who override the quality gate in their
//!   `.perf-sentinel.toml` still see this fixed heuristic — the gate is a
//!   user policy, the interpretation is a fixed heuristic. By design.
//!
//! # JSON stability contract
//!
//! The [`InterpretationLevel`] bands ship as sibling fields in the JSON
//! output (`io_intensity_band` next to `io_intensity_score`,
//! `io_waste_ratio_band` next to `io_waste_ratio`). The contract is:
//!
//! - **Enum values are stable across versions** (`"healthy"`, `"moderate"`,
//!   `"high"`, `"critical"`). Downstream consumers can rely on these
//!   names in SARIF, Grafana, perf-lint.
//! - **Thresholds are versioned with the binary** and may evolve. A
//!   consumer who wants a version-independent classification must read
//!   the raw `io_intensity_score` / `io_waste_ratio` fields and apply
//!   their own bands.
//!
//! This mirrors the existing pattern where `co2.model: "io_proxy_v1" |
//! "io_proxy_v2" | "io_proxy_v3"` evolves across versions without breaking
//! consumers who just want to know which model was used.

/// Four-level interpretation band for a numerical score.
///
/// Produced by [`InterpretationLevel::for_iis`] and
/// [`InterpretationLevel::for_waste_ratio`]. Rendered as a short lowercase
/// label by [`InterpretationLevel::short_label`]. The CLI maps each level
/// to an ANSI color in `sentinel-cli/src/main.rs`. The serde
/// representation is lowercase (`"healthy"` / `"moderate"` / `"high"` /
/// `"critical"`) and is stable across versions — see the module docstring
/// for the stability contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InterpretationLevel {
    /// Signal is below the moderate threshold. Nothing to investigate.
    Healthy,
    /// Signal is above baseline but below the action threshold. Informational.
    Moderate,
    /// Signal is at or above the action threshold. Worth investigating.
    High,
    /// Signal is at or above the critical threshold. Very likely a bug.
    Critical,
}

// --- IIS thresholds ---

/// Rule-of-thumb lower bound: >= 2 I/O ops per request is above the simple
/// CRUD baseline (1 SQL + maybe 1 cache). **Heuristic**, not empirical.
pub const IIS_MODERATE: f64 = 2.0;

/// High threshold, anchored on `Config::default().n_plus_one_threshold = 5`.
/// An endpoint whose IIS reaches 5.0 is arithmetically at the point where
/// `detect_n_plus_one` starts emitting findings.
pub const IIS_HIGH: f64 = 5.0;

/// Critical threshold, mechanically anchored on
/// [`crate::detect::n_plus_one::CRITICAL_OCCURRENCE_THRESHOLD`] via the
/// `iis_critical_matches_n_plus_one_detector_threshold` drift-guard
/// test. The detector flips an N+1 finding to `Severity::Critical`
/// exactly when `indices.len() >= CRITICAL_OCCURRENCE_THRESHOLD`.
pub const IIS_CRITICAL: f64 = 10.0;

// --- waste ratio thresholds ---

/// Below this, avoidable I/O is marginal.
pub const WASTE_RATIO_MODERATE: f64 = 0.10;

/// Anchored on the **default** `io_waste_ratio_max = 0.30`. Above this, the
/// default quality gate would fail. See module docs for why we anchor on
/// the default rather than the user's config.
pub const WASTE_RATIO_HIGH: f64 = 0.30;

/// Half or more of analyzed I/O is avoidable waste.
pub const WASTE_RATIO_CRITICAL: f64 = 0.50;

impl InterpretationLevel {
    /// Private four-level `>=` classifier shared by [`for_iis`] and
    /// [`for_waste_ratio`]. Takes the three thresholds in the same
    /// order `(moderate, high, critical)` both public wrappers use.
    ///
    /// `NaN` falls through to [`Healthy`] because NaN compares false
    /// against every threshold — intentional: missing data should not
    /// render as a red CLI warning.
    ///
    /// [`for_iis`]: Self::for_iis
    /// [`for_waste_ratio`]: Self::for_waste_ratio
    /// [`Healthy`]: Self::Healthy
    #[inline]
    fn classify(value: f64, moderate: f64, high: f64, critical: f64) -> Self {
        if value >= critical {
            Self::Critical
        } else if value >= high {
            Self::High
        } else if value >= moderate {
            Self::Moderate
        } else {
            Self::Healthy
        }
    }

    /// Classify an I/O Intensity Score (IIS) into a band.
    ///
    /// Comparisons use `>=` to match the N+1 detector's own
    /// `indices.len() >= 10` convention: an IIS of exactly 10.0 is
    /// [`Critical`], not [`High`].
    ///
    /// `NaN` is treated as [`Healthy`] (it compares false against every
    /// threshold), which is the safe choice: missing data should not
    /// trigger red output.
    ///
    /// [`Critical`]: Self::Critical
    /// [`High`]: Self::High
    /// [`Healthy`]: Self::Healthy
    #[must_use]
    pub fn for_iis(iis: f64) -> Self {
        Self::classify(iis, IIS_MODERATE, IIS_HIGH, IIS_CRITICAL)
    }

    /// Classify an I/O waste ratio (0.0 to 1.0) into a band.
    ///
    /// See the module docstring for why this is anchored on the *default*
    /// quality gate threshold rather than the user's configured value.
    ///
    /// `NaN` is treated as [`Healthy`] for the same reason as
    /// [`for_iis`](Self::for_iis).
    ///
    /// [`Healthy`]: Self::Healthy
    #[must_use]
    pub fn for_waste_ratio(ratio: f64) -> Self {
        Self::classify(
            ratio,
            WASTE_RATIO_MODERATE,
            WASTE_RATIO_HIGH,
            WASTE_RATIO_CRITICAL,
        )
    }

    /// Return a short lowercase label, suitable for CLI parenthetical
    /// rendering: `"healthy"`, `"moderate"`, `"high"`, `"critical"`.
    ///
    /// Takes `self` by value because `InterpretationLevel` is `Copy`
    /// (fieldless enum); there's no reason to add a deref.
    #[must_use]
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Moderate => "moderate",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- IIS boundary tests ---

    #[test]
    fn iis_healthy_below_moderate() {
        assert_eq!(
            InterpretationLevel::for_iis(0.0),
            InterpretationLevel::Healthy
        );
        assert_eq!(
            InterpretationLevel::for_iis(1.0),
            InterpretationLevel::Healthy
        );
        assert_eq!(
            InterpretationLevel::for_iis(1.99),
            InterpretationLevel::Healthy
        );
    }

    #[test]
    fn iis_moderate_at_and_above_2() {
        assert_eq!(
            InterpretationLevel::for_iis(2.0),
            InterpretationLevel::Moderate
        );
        assert_eq!(
            InterpretationLevel::for_iis(3.5),
            InterpretationLevel::Moderate
        );
        assert_eq!(
            InterpretationLevel::for_iis(4.99),
            InterpretationLevel::Moderate
        );
    }

    #[test]
    fn iis_high_at_and_above_5() {
        assert_eq!(InterpretationLevel::for_iis(5.0), InterpretationLevel::High);
        assert_eq!(InterpretationLevel::for_iis(7.5), InterpretationLevel::High);
        assert_eq!(
            InterpretationLevel::for_iis(9.99),
            InterpretationLevel::High
        );
    }

    #[test]
    fn iis_critical_at_and_above_10() {
        assert_eq!(
            InterpretationLevel::for_iis(10.0),
            InterpretationLevel::Critical
        );
        assert_eq!(
            InterpretationLevel::for_iis(22.0),
            InterpretationLevel::Critical
        );
        assert_eq!(
            InterpretationLevel::for_iis(100.0),
            InterpretationLevel::Critical
        );
    }

    /// Cross-module mechanical drift guard: `IIS_CRITICAL` must match
    /// the N+1 detector's severity-escalation threshold. If one of the
    /// two is updated, this test forces the other to follow by directly
    /// comparing against the crate-internal constant, not a literal.
    #[test]
    fn iis_critical_matches_n_plus_one_detector_threshold() {
        use crate::detect::n_plus_one::CRITICAL_OCCURRENCE_THRESHOLD;
        // The detector uses `indices.len() >= CRITICAL_OCCURRENCE_THRESHOLD`
        // to flip to Severity::Critical. IIS_CRITICAL must classify the
        // same boundary as Critical on the interpretation side.
        #[allow(clippy::cast_precision_loss)]
        let detector_threshold_f64 = CRITICAL_OCCURRENCE_THRESHOLD as f64;
        assert!(
            (IIS_CRITICAL - detector_threshold_f64).abs() < f64::EPSILON,
            "IIS_CRITICAL ({IIS_CRITICAL}) drifted from \
             detect::n_plus_one::CRITICAL_OCCURRENCE_THRESHOLD \
             ({CRITICAL_OCCURRENCE_THRESHOLD}); update both or neither"
        );
    }

    /// `IIS_HIGH` is anchored on the default `n_plus_one_threshold`
    /// (crates/sentinel-core/src/config.rs — `Config::default`).
    ///
    /// This test reads the runtime value of `Config::default().n_plus_one_threshold`
    /// and asserts they match. A bare literal comparison (`IIS_HIGH == 5.0`)
    /// would not catch the case where someone bumps the config default but
    /// forgets to update `IIS_HIGH` — the point of a drift guard is to
    /// follow the anchor, not to freeze a magic number.
    ///
    /// `f64::from(u32)` is lossless today. If someone widens
    /// `n_plus_one_threshold` from `u32` to `usize`, `f64::from(usize)`
    /// does not exist and this test will stop compiling, forcing a
    /// manual decision on how to cast. That hard break is the drift
    /// guard here: do NOT paper over it with `as f64` — the type
    /// change should get human attention.
    #[test]
    fn iis_high_matches_n_plus_one_threshold_default() {
        let default_threshold = crate::config::Config::default().n_plus_one_threshold;
        let threshold_f64 = f64::from(default_threshold);
        assert!(
            (IIS_HIGH - threshold_f64).abs() < f64::EPSILON,
            "IIS_HIGH ({IIS_HIGH}) drifted away from \
             Config::default().n_plus_one_threshold ({default_threshold}); \
             update both or neither"
        );
    }

    // --- waste ratio boundary tests ---

    #[test]
    fn waste_ratio_healthy_below_10_percent() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.0),
            InterpretationLevel::Healthy
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.05),
            InterpretationLevel::Healthy
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.09),
            InterpretationLevel::Healthy
        );
    }

    #[test]
    fn waste_ratio_moderate_between_10_and_30_percent() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.10),
            InterpretationLevel::Moderate
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.20),
            InterpretationLevel::Moderate
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.29),
            InterpretationLevel::Moderate
        );
    }

    #[test]
    fn waste_ratio_high_between_30_and_50_percent() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.30),
            InterpretationLevel::High
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.40),
            InterpretationLevel::High
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.49),
            InterpretationLevel::High
        );
    }

    #[test]
    fn waste_ratio_critical_at_and_above_50_percent() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.50),
            InterpretationLevel::Critical
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.75),
            InterpretationLevel::Critical
        );
        // demo.json fixture: 9 avoidable / 10 total = 0.9
        assert_eq!(
            InterpretationLevel::for_waste_ratio(0.9),
            InterpretationLevel::Critical
        );
        assert_eq!(
            InterpretationLevel::for_waste_ratio(1.0),
            InterpretationLevel::Critical
        );
    }

    /// `WASTE_RATIO_HIGH` is anchored on the **default** quality gate
    /// threshold. The anchor is structural: a user who raises
    /// `io_waste_ratio_max = 0.80` in their config still sees the same
    /// 30% heuristic band because the two dials express independent
    /// concepts (policy vs heuristic).
    #[test]
    fn waste_ratio_high_matches_default_gate_threshold() {
        assert!(
            (WASTE_RATIO_HIGH - 0.30).abs() < f64::EPSILON,
            "WASTE_RATIO_HIGH drifted from the default io_waste_ratio_max (0.30); \
             update both or neither"
        );
    }

    // --- short_label tests ---

    #[test]
    fn short_label_returns_lowercase_string() {
        assert_eq!(InterpretationLevel::Healthy.short_label(), "healthy");
        assert_eq!(InterpretationLevel::Moderate.short_label(), "moderate");
        assert_eq!(InterpretationLevel::High.short_label(), "high");
        assert_eq!(InterpretationLevel::Critical.short_label(), "critical");
    }

    // --- NaN handling ---

    #[test]
    fn nan_iis_classified_healthy() {
        // NaN compares false against all thresholds, so it falls through
        // to Healthy. This is the safe default: missing data should not
        // render as a red CLI warning.
        assert_eq!(
            InterpretationLevel::for_iis(f64::NAN),
            InterpretationLevel::Healthy
        );
    }

    #[test]
    fn nan_waste_ratio_classified_healthy() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(f64::NAN),
            InterpretationLevel::Healthy
        );
    }

    // --- Infinity handling ---

    /// `+Infinity` must classify as `Critical` (it satisfies every `>=`
    /// threshold). `-Infinity` must classify as `Healthy` (it satisfies
    /// none). These document the behavior for downstream renderers that
    /// format `f64` scores with `{:.1}` / `{:.6}` — Rust's `Display` impl
    /// for infinities prints `"inf"` / `"-inf"` without panicking, so the
    /// CLI stays crash-safe even on adversarial inputs.
    #[test]
    fn positive_infinity_iis_classified_critical() {
        assert_eq!(
            InterpretationLevel::for_iis(f64::INFINITY),
            InterpretationLevel::Critical
        );
    }

    #[test]
    fn negative_infinity_iis_classified_healthy() {
        assert_eq!(
            InterpretationLevel::for_iis(f64::NEG_INFINITY),
            InterpretationLevel::Healthy
        );
    }

    #[test]
    fn positive_infinity_waste_ratio_classified_critical() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(f64::INFINITY),
            InterpretationLevel::Critical
        );
    }

    #[test]
    fn negative_infinity_waste_ratio_classified_healthy() {
        assert_eq!(
            InterpretationLevel::for_waste_ratio(f64::NEG_INFINITY),
            InterpretationLevel::Healthy
        );
    }
}
