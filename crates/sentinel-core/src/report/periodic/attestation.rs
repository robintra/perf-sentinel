//! In-toto v1 statement builder for periodic disclosure reports.
//!
//! Produces a sidecar attestation document that pins the report's
//! SHA-256 digest and a lean methodology summary. The output is a
//! complete in-toto v1 Statement (`_type`, `predicateType`, `subject`,
//! `predicate`), not just a predicate. Operators sign it with
//! `cosign sign-blob <statement> --bundle <bundle.sig>
//! --new-bundle-format`. Do not pass the statement to
//! `cosign attest-blob --predicate`: that would wrap it in another
//! statement and produce a permanent double-wrapped entry in the
//! Rekor public log.
//!
//! Wire format reference: `docs/design/10-SIGSTORE-ATTESTATION.md`
//! and <https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md>.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::schema::{PeriodicReport, ReportIntent};

/// in-toto v1 statement type URL.
pub const IN_TOTO_STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";

/// perf-sentinel custom predicate type URL.
///
/// The `perf-sentinel.io` host is a namespace convention (the domain is
/// not formally owned by the project, see design doc 10 for the
/// rationale). Verifiers identify the predicate by exact string match.
pub const PERF_SENTINEL_PREDICATE_TYPE: &str = "https://perf-sentinel.io/attestation/v1";

/// Conventional subject name for the report file, used when the caller
/// does not pass an explicit name to [`build_in_toto_statement`].
pub const DEFAULT_SUBJECT_NAME: &str = "perf-sentinel-report.json";

// Only `PartialEq` (no `Eq`) on the statement, predicate, and methodology
// summary: `MethodologySummary.period_coverage: f64` is not `Eq` because
// NaN breaks reflexivity. `PartialEq` is sufficient for `assert_eq!` and
// the serde round-trip tests, and matches Rust's stdlib stance on f64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InTotoStatement {
    #[serde(rename = "_type")]
    pub statement_type: String,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub subject: Vec<InTotoSubject>,
    pub predicate: PerfSentinelPredicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InTotoSubject {
    pub name: String,
    pub digest: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfSentinelPredicate {
    pub perf_sentinel_version: String,
    pub report_uuid: String,
    pub period: PeriodSummary,
    pub intent: String,
    pub confidentiality_level: String,
    pub organisation: OrganisationSummary,
    pub methodology_summary: MethodologySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodSummary {
    pub from_date: String,
    pub to_date: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganisationSummary {
    pub name: String,
    pub country: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub identifiers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodologySummary {
    pub sci_specification: String,
    pub conformance: String,
    pub calibration_applied: bool,
    pub period_coverage: f64,
    /// Count of patterns flagged as core-required by the validator at
    /// the time of signing. A consumer can compare this to the live
    /// `core_patterns_required` set defined by the perf-sentinel
    /// version recorded in `predicate.perf_sentinel_version`. A
    /// mismatch suggests the report was generated with a non-standard
    /// core set.
    pub core_patterns_count: u32,
    /// Count of patterns actively enabled at the time of report
    /// generation. Combined with `core_patterns_count`, lets a consumer
    /// detect reports where the enabled set is a strict subset of core
    /// (core patterns dropped post-hoc).
    pub enabled_patterns_count: u32,
    /// Count of patterns explicitly disabled at the time of report
    /// generation. Lets a consumer flag reports that disabled too many
    /// patterns or disabled core patterns.
    pub disabled_patterns_count: u32,
    /// SHA-256 over the sorted, colon-joined names of the core pattern
    /// set declared in `methodology.core_patterns_required`. Lets a
    /// consumer detect not just shrinkage (covered by
    /// `core_patterns_count`) but substitution: replacing a canonical
    /// core pattern with another keeps the count constant yet changes
    /// the hash. The reference value for a given
    /// `predicate.perf_sentinel_version` is the hash of the canonical
    /// `core_patterns_required()` slice in that version.
    pub core_patterns_hash: String,
}

/// Build an in-toto v1 statement that pins `report_file_sha256` as the
/// hash of the report file and folds in a lean projection of the
/// disclosure's methodology block. The 64-hex value must come from
/// hashing the serialised report file on disk, not the canonical
/// `content_hash` (which blanks one field).
#[must_use]
pub fn build_in_toto_statement(
    report: &PeriodicReport,
    report_file_sha256: &str,
) -> InTotoStatement {
    build_in_toto_statement_named(report, report_file_sha256, DEFAULT_SUBJECT_NAME)
}

/// Same as [`build_in_toto_statement`] but lets the caller pick the
/// subject name. Useful when the report file is published under a
/// non-default name (e.g. `2026-Q1-disclosure.json`).
#[must_use]
pub fn build_in_toto_statement_named(
    report: &PeriodicReport,
    report_file_sha256: &str,
    subject_name: &str,
) -> InTotoStatement {
    let mut digest = BTreeMap::new();
    digest.insert("sha256".to_string(), report_file_sha256.to_string());

    let mut identifiers = BTreeMap::new();
    let ids = &report.organisation.identifiers;
    if let Some(v) = &ids.siren {
        identifiers.insert("siren".to_string(), v.clone());
    }
    if let Some(v) = &ids.vat {
        identifiers.insert("vat".to_string(), v.clone());
    }
    if let Some(v) = &ids.lei {
        identifiers.insert("lei".to_string(), v.clone());
    }
    if let Some(v) = &ids.opencorporates_url {
        identifiers.insert("opencorporates_url".to_string(), v.clone());
    }
    if let Some(v) = &ids.domain {
        identifiers.insert("domain".to_string(), v.clone());
    }

    InTotoStatement {
        statement_type: IN_TOTO_STATEMENT_TYPE.to_string(),
        predicate_type: PERF_SENTINEL_PREDICATE_TYPE.to_string(),
        subject: vec![InTotoSubject {
            name: subject_name.to_string(),
            digest,
        }],
        predicate: PerfSentinelPredicate {
            perf_sentinel_version: report.report_metadata.perf_sentinel_version.clone(),
            report_uuid: report.report_metadata.report_uuid.to_string(),
            period: PeriodSummary {
                from_date: report.period.from_date.to_string(),
                to_date: report.period.to_date.to_string(),
            },
            intent: intent_str(report.report_metadata.intent).to_string(),
            confidentiality_level: confidentiality_str(
                report.report_metadata.confidentiality_level,
            )
            .to_string(),
            organisation: OrganisationSummary {
                name: report.organisation.name.clone(),
                country: report.organisation.country.clone(),
                identifiers,
            },
            methodology_summary: MethodologySummary {
                sci_specification: report.methodology.sci_specification.clone(),
                conformance: conformance_str(report.methodology.conformance).to_string(),
                calibration_applied: report.methodology.calibration_inputs.calibration_applied,
                period_coverage: report.aggregate.period_coverage,
                core_patterns_count: u32::try_from(report.methodology.core_patterns_required.len())
                    .unwrap_or(u32::MAX),
                enabled_patterns_count: u32::try_from(report.methodology.enabled_patterns.len())
                    .unwrap_or(u32::MAX),
                disabled_patterns_count: u32::try_from(report.methodology.disabled_patterns.len())
                    .unwrap_or(u32::MAX),
                core_patterns_hash: hash_core_patterns(&report.methodology.core_patterns_required),
            },
        },
    }
}

/// SHA-256 over the sorted, colon-joined core pattern names. Sort is
/// applied to make the hash invariant of input order. 64-hex output,
/// no `sha256:` prefix, to match the in-toto subject digest format.
/// Exposed so `verify-hash` can recompute the canonical value from
/// the local binary and cross-check against the predicate.
#[must_use]
pub fn hash_core_patterns(patterns: &[String]) -> String {
    let mut sorted: Vec<&str> = patterns.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let joined = sorted.join(":");
    let digest = Sha256::digest(joined.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

const fn intent_str(intent: ReportIntent) -> &'static str {
    match intent {
        ReportIntent::Internal => "internal",
        ReportIntent::Official => "official",
        ReportIntent::Audited => "audited",
    }
}

const fn confidentiality_str(c: super::schema::Confidentiality) -> &'static str {
    match c {
        super::schema::Confidentiality::Internal => "internal",
        super::schema::Confidentiality::Public => "public",
    }
}

const fn conformance_str(c: super::schema::Conformance) -> &'static str {
    match c {
        super::schema::Conformance::CoreRequired => "core-required",
        super::schema::Conformance::Extended => "extended",
        super::schema::Conformance::Partial => "partial",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::periodic::schema::PeriodicReport;
    use std::path::PathBuf;

    fn load_g2_example() -> PeriodicReport {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = PathBuf::from(manifest_dir)
            .join("..")
            .join("..")
            .join("docs/schemas/examples/example-official-public-G2.json");
        let raw = std::fs::read_to_string(&path).expect("read G2 example");
        serde_json::from_str(&raw).expect("parse G2 example")
    }

    const DIGEST_64: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[test]
    fn statement_carries_expected_top_level_fields() {
        let r = load_g2_example();
        let s = build_in_toto_statement(&r, DIGEST_64);
        assert_eq!(s.statement_type, IN_TOTO_STATEMENT_TYPE);
        assert_eq!(s.predicate_type, PERF_SENTINEL_PREDICATE_TYPE);
        assert_eq!(s.subject.len(), 1);
        assert_eq!(s.subject[0].name, DEFAULT_SUBJECT_NAME);
        assert_eq!(s.subject[0].digest.get("sha256").unwrap(), DIGEST_64);
    }

    #[test]
    fn predicate_projects_methodology_summary() {
        let r = load_g2_example();
        let s = build_in_toto_statement(&r, DIGEST_64);
        assert_eq!(s.predicate.intent, "official");
        assert_eq!(s.predicate.confidentiality_level, "public");
        assert_eq!(
            s.predicate.methodology_summary.sci_specification,
            "ISO/IEC 21031:2024"
        );
        assert_eq!(s.predicate.methodology_summary.conformance, "core-required");
        assert!(
            (s.predicate.methodology_summary.period_coverage - r.aggregate.period_coverage).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn predicate_pattern_counts_match_g2_methodology() {
        let r = load_g2_example();
        let s = build_in_toto_statement(&r, DIGEST_64);
        let m = &s.predicate.methodology_summary;
        // G2 ships the four canonical core patterns, the ten enabled
        // patterns from FindingType, and no disabled patterns.
        assert_eq!(m.core_patterns_count, 4);
        assert_eq!(m.enabled_patterns_count, 10);
        assert_eq!(m.disabled_patterns_count, 0);
    }

    #[test]
    fn predicate_pattern_counts_reflect_disabled_overrides() {
        use crate::report::periodic::schema::DisabledPattern;
        let mut r = load_g2_example();
        r.methodology.enabled_patterns.truncate(8);
        r.methodology.disabled_patterns = vec![
            DisabledPattern {
                name: "pool_saturation".to_string(),
                reason: "noisy on this stack".to_string(),
            },
            DisabledPattern {
                name: "serialized_calls".to_string(),
                reason: "false positives in batch jobs".to_string(),
            },
        ];
        let s = build_in_toto_statement(&r, DIGEST_64);
        let m = &s.predicate.methodology_summary;
        assert_eq!(m.enabled_patterns_count, 8);
        assert_eq!(m.disabled_patterns_count, 2);
    }

    #[test]
    fn predicate_enabled_count_is_at_least_core_count() {
        // Audit invariant: every core pattern must be enabled, so
        // enabled_patterns_count >= core_patterns_count. A consumer
        // can fail an audit on the reverse.
        let r = load_g2_example();
        let s = build_in_toto_statement(&r, DIGEST_64);
        let m = &s.predicate.methodology_summary;
        assert!(m.enabled_patterns_count >= m.core_patterns_count);
    }

    #[test]
    fn predicate_core_patterns_hash_is_64_hex_and_stable() {
        let r = load_g2_example();
        let s1 = build_in_toto_statement(&r, DIGEST_64);
        let s2 = build_in_toto_statement(&r, DIGEST_64);
        let h1 = &s1.predicate.methodology_summary.core_patterns_hash;
        let h2 = &s2.predicate.methodology_summary.core_patterns_hash;
        assert_eq!(h1, h2, "hash must be deterministic");
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn core_patterns_hash_is_invariant_under_input_order() {
        let a = hash_core_patterns(&[
            "n_plus_one_sql".to_string(),
            "n_plus_one_http".to_string(),
            "redundant_sql".to_string(),
            "redundant_http".to_string(),
        ]);
        let b = hash_core_patterns(&[
            "redundant_http".to_string(),
            "n_plus_one_http".to_string(),
            "redundant_sql".to_string(),
            "n_plus_one_sql".to_string(),
        ]);
        assert_eq!(a, b);
    }

    #[test]
    fn core_patterns_hash_changes_on_substitution() {
        // Substituting a core pattern with another (same count) must
        // change the hash. Defence in depth beyond core_patterns_count.
        let baseline = hash_core_patterns(&[
            "n_plus_one_sql".to_string(),
            "n_plus_one_http".to_string(),
            "redundant_sql".to_string(),
            "redundant_http".to_string(),
        ]);
        let substituted = hash_core_patterns(&[
            "n_plus_one_sql".to_string(),
            "n_plus_one_http".to_string(),
            "redundant_sql".to_string(),
            "slow_sql".to_string(),
        ]);
        assert_ne!(baseline, substituted);
    }

    #[test]
    fn core_patterns_hash_matches_canonical_set_for_g2() {
        // Anchor: the G2 example carries the canonical 4-pattern core
        // set, so the predicate hash equals the hash of the canonical
        // reference list a consumer would hardcode.
        let r = load_g2_example();
        let s = build_in_toto_statement(&r, DIGEST_64);
        let canonical =
            hash_core_patterns(&crate::report::periodic::schema::core_patterns_required());
        assert_eq!(
            s.predicate.methodology_summary.core_patterns_hash,
            canonical
        );
    }

    #[test]
    fn statement_serde_roundtrip() {
        let r = load_g2_example();
        let s = build_in_toto_statement(&r, DIGEST_64);
        let json = serde_json::to_string(&s).unwrap();
        let back: InTotoStatement = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        // in-toto wire spec uses `_type` and `predicateType`, not the
        // Rust struct field names.
        assert!(json.contains("\"_type\""));
        assert!(json.contains("\"predicateType\""));
    }

    #[test]
    fn custom_subject_name_overrides_default() {
        let r = load_g2_example();
        let s = build_in_toto_statement_named(&r, DIGEST_64, "2026-Q1-disclosure.json");
        assert_eq!(s.subject[0].name, "2026-Q1-disclosure.json");
    }
}
