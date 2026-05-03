//! Structured snapshot warnings for [`Report`].
//!
//! Coexists with the legacy `Report.warnings: Vec<String>` (0.5.16+).
//! Renderers (CLI, HTML) prefer `warning_details` when non-empty,
//! fall back to `warnings` otherwise. Pre-0.5.19 baselines parse fine
//! thanks to `serde(default)`.
//!
//! # Sanitization contract for `Warning::kind` and `Warning::message`
//!
//! Both fields land verbatim in the JSON `Report` payload, the HTML
//! dashboard's embedded payload, and the CLI's terminal output. Today
//! both 0.5.19 producer sites use trusted inputs (hardcoded literals
//! or a `format!` over a `u64` counter), so [`Warning::new`] does no
//! sanitization. **A future contributor wiring a `Warning` from any
//! source that touches user-controlled bytes (OTLP attributes, span
//! names, request headers, config strings) MUST construct it via
//! [`Warning::from_untrusted`]**, which strips `BiDi` format codes and
//! invisible control characters per the same defense applied to the
//! SARIF emitter (cf. `report::sarif::strip_bidi_and_invisible`).

use serde::{Deserialize, Serialize};

/// Stable kind for the daemon cold-start `warning_details` entry.
pub const COLD_START: &str = "cold_start";

/// Stable kind for the report-level summary of OTLP requests dropped
/// due to channel saturation.
pub const INGESTION_DROPS: &str = "ingestion_drops";

/// Operator-facing snapshot warning with a stable category.
///
/// `kind` is suitable for alerting and aggregation across runs (e.g.
/// [`COLD_START`], [`INGESTION_DROPS`]). `message` is human-readable
/// and can include dynamic values (counts, thresholds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub kind: String,
    pub message: String,
}

impl Warning {
    /// Build a `Warning` from trusted inputs (hardcoded literals,
    /// numeric formatters, or values already validated by another
    /// stage). For attacker-controlled or otherwise untrusted bytes,
    /// use [`Warning::from_untrusted`] instead.
    #[must_use]
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }

    /// Build a `Warning` from inputs that may carry attacker-controlled
    /// bytes. Strips `BiDi` format codes (`U+202A..U+202E`,
    /// `U+2066..U+2069`) and invisible control characters from both
    /// `kind` and `message`, mirroring the defense applied by the
    /// SARIF emitter to acknowledgment metadata. Defends against
    /// Trojan Source (CVE-2021-42574) and similar log-poisoning
    /// vectors.
    #[must_use]
    pub fn from_untrusted(kind: &str, message: &str) -> Self {
        Self {
            kind: crate::report::sarif::strip_bidi_and_invisible(kind),
            message: crate::report::sarif::strip_bidi_and_invisible(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_serde_roundtrip() {
        let w = Warning::new(COLD_START, "daemon has not yet processed any events");
        let json = serde_json::to_string(&w).expect("serialize");
        let parsed: Warning = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, w);
    }

    #[test]
    fn warning_new_accepts_str_and_string() {
        let from_str = Warning::new("k", "m");
        let from_string = Warning::new(String::from("k"), String::from("m"));
        assert_eq!(from_str, from_string);
    }

    #[test]
    fn warning_from_untrusted_strips_bidi_and_invisible() {
        // U+202E RIGHT-TO-LEFT OVERRIDE in kind, U+200B ZWSP in message.
        let w = Warning::from_untrusted(
            "alert\u{202E}_kind",
            "msg with\u{202E}override and\u{200B}zwsp",
        );
        assert!(!w.kind.contains('\u{202E}'));
        assert!(!w.message.contains('\u{202E}'));
        assert!(!w.message.contains('\u{200B}'));
    }

    #[test]
    fn warning_kind_constants_match_documented_values() {
        // Lock the wire-format value of the two stable kinds shipped in
        // 0.5.19. Operators write alert rules against these strings, so
        // a rename is a breaking change.
        assert_eq!(COLD_START, "cold_start");
        assert_eq!(INGESTION_DROPS, "ingestion_drops");
    }
}
