//! Ignore rules / acknowledgments for findings.
//!
//! Loads `.perf-sentinel-acknowledgments.toml`, computes a canonical
//! signature per [`Finding`], filters findings flagged as acknowledged
//! at the post-processing stage, and re-evaluates the quality gate on
//! the surviving set so an ack can flip a previously failing gate to
//! green.
//!
//! Out of scope here: daemon-side runtime ack (deferred to 0.5.18, if
//! the architecture review confirms the need).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::detect::Finding;
use crate::quality_gate;
use crate::report::{AcknowledgedFinding, Report};

/// Hard cap on the size of `.perf-sentinel-acknowledgments.toml`. Mirrors
/// the trace-ingest payload-cap discipline so a stray
/// `--acknowledgments /dev/zero` or a multi-GB malformed TOML cannot
/// silently exhaust process memory.
pub const MAX_ACKNOWLEDGMENTS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// A single acknowledgment entry deserialized from the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acknowledgment {
    /// Canonical signature: `<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix>`.
    pub signature: String,
    /// Email or identifier of the user who created the ack.
    pub acknowledged_by: String,
    /// ISO 8601 date when the ack was created (`YYYY-MM-DD`).
    pub acknowledged_at: String,
    /// Free-text reason / context for the ack.
    pub reason: String,
    /// Optional ISO 8601 date (`YYYY-MM-DD`) at which the ack expires.
    /// `None` means the ack is permanent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Container for the deserialized TOML file.
///
/// The TOML root is `[[acknowledged]]` blocks. Empty file (no blocks)
/// deserializes to a default value, making "file exists but is empty" a
/// no-op.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcknowledgmentsFile {
    #[serde(default)]
    pub acknowledged: Vec<Acknowledgment>,
}

/// Compute the canonical signature of a finding.
///
/// Format: `<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>`.
/// The `sha256` prefix uses the first 8 bytes (16 hex characters), giving
/// ~64 bits of collision resistance. The triple
/// `(finding_type, service, sanitized_endpoint)` is already part of the
/// signature, so the hash only needs to disambiguate templates within the
/// same triple, an extremely small population in practice. The 16-char
/// prefix is defense in depth against accidental ack masking after a SQL
/// refactor or a service rename.
///
/// Sanitization replaces `/` and ` ` (space) inside `source_endpoint`
/// with `_` so the resulting signature uses `:` as a single, unambiguous
/// separator that operators can split on in shell pipelines. `BiDi`
/// override and invisible-format characters (Trojan Source, CVE-2021-42574)
/// are stripped from both `service` and `source_endpoint` so two visually
/// identical signatures cannot map to distinct ack entries.
#[must_use]
pub fn compute_signature(finding: &Finding) -> String {
    let mut hasher = Sha256::new();
    hasher.update(finding.pattern.template.as_bytes());
    let digest = hasher.finalize();
    let safe_service = crate::report::sarif::strip_bidi_and_invisible(&finding.service);
    let safe_endpoint = crate::report::sarif::strip_bidi_and_invisible(&sanitize_endpoint(
        &finding.source_endpoint,
    ));
    format!(
        "{}:{}:{}:{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        finding.finding_type.as_str(),
        safe_service,
        safe_endpoint,
        digest[0],
        digest[1],
        digest[2],
        digest[3],
        digest[4],
        digest[5],
        digest[6],
        digest[7],
    )
}

fn sanitize_endpoint(endpoint: &str) -> String {
    endpoint.replace(['/', ' '], "_")
}

/// Fill in the `signature` field of every finding in place.
///
/// Idempotent: an existing signature is overwritten so re-running this
/// function on a baseline that already carries signatures (e.g. a
/// pre-0.5.17 dump that was just re-emitted) keeps the values fresh
/// against the current signature scheme.
pub fn enrich_with_signatures(findings: &mut [Finding]) {
    for finding in findings.iter_mut() {
        finding.signature = compute_signature(finding);
    }
}

/// Load acknowledgments from a TOML file.
///
/// Returns `Ok(default)` when the file does not exist, so a project
/// without any acks observes the legacy behavior with zero error noise.
/// Returns `Err` on TOML parse failure or on a malformed `expires_at`
/// date so a typo in the ack file fails the run loud rather than
/// silently widening the matched set.
///
/// Reads with a hard cap of [`MAX_ACKNOWLEDGMENTS_FILE_BYTES`]. The TOML
/// crate has no public depth limiter, but the size cap keeps the worst
/// case bounded and rejects `/dev/zero` and the like.
///
/// # Errors
///
/// - [`AcknowledgmentLoadError::Io`] when the file exists but cannot be read.
/// - [`AcknowledgmentLoadError::TooLarge`] when the file exceeds the cap.
/// - [`AcknowledgmentLoadError::Parse`] when the TOML cannot be parsed.
/// - [`AcknowledgmentLoadError::InvalidDate`] when an `expires_at` value is
///   not a valid `YYYY-MM-DD` ISO 8601 date.
pub fn load_from_file(path: &Path) -> Result<AcknowledgmentsFile, AcknowledgmentLoadError> {
    if !path.exists() {
        return Ok(AcknowledgmentsFile::default());
    }
    let file = std::fs::File::open(path).map_err(AcknowledgmentLoadError::Io)?;
    // `take(cap + 1)` closes the TOCTOU window between metadata().len()
    // and read(): we read at most cap+1 bytes, and reject if we hit the
    // cap+1th byte. Same pattern as `read_file_capped` in the CLI.
    let mut buf = String::new();
    file.take(MAX_ACKNOWLEDGMENTS_FILE_BYTES + 1)
        .read_to_string(&mut buf)
        .map_err(AcknowledgmentLoadError::Io)?;
    if buf.len() as u64 > MAX_ACKNOWLEDGMENTS_FILE_BYTES {
        return Err(AcknowledgmentLoadError::TooLarge {
            cap: MAX_ACKNOWLEDGMENTS_FILE_BYTES,
        });
    }
    let parsed: AcknowledgmentsFile =
        toml::from_str(&buf).map_err(AcknowledgmentLoadError::Parse)?;

    for (idx, ack) in parsed.acknowledged.iter().enumerate() {
        if let Some(ref expires) = ack.expires_at {
            NaiveDate::parse_from_str(expires, "%Y-%m-%d").map_err(|e| {
                AcknowledgmentLoadError::InvalidDate {
                    entry_index: idx,
                    field: "expires_at",
                    value: expires.clone(),
                    message: e.to_string(),
                }
            })?;
        }
    }

    Ok(parsed)
}

/// Apply acknowledgments to a `Report` in place.
///
/// 1. Clears any prior `report.acknowledged_findings` so a Report fed
///    back through this function (e.g. a baseline JSON round-trip)
///    cannot accumulate stale ack pairs across runs.
/// 2. Filters `report.findings`, moving acked entries into
///    `report.acknowledged_findings`.
/// 3. Re-evaluates the quality gate on the surviving set so an ack can
///    flip a previously failing gate to green (the entire point of
///    "won't fix / accepted" semantics). Re-evaluation runs even when no
///    ack matched, so the gate field is always self-consistent with the
///    final `findings` slice.
///
/// Acks with an `expires_at` strictly before `now` are treated as inactive
/// and the corresponding finding is preserved in `report.findings`.
pub fn apply_to_report(
    report: &mut Report,
    acks: &AcknowledgmentsFile,
    config: &Config,
    now: DateTime<Utc>,
) {
    // Drop any prior ack pairs from the source Report. The caller may
    // have loaded a baseline that already carried `acknowledged_findings`
    // from a previous `--show-acknowledged` run, which we do not want to
    // double-count or treat as authoritative.
    report.acknowledged_findings.clear();

    let active: HashMap<&str, &Acknowledgment> = acks
        .acknowledged
        .iter()
        .filter(|a| is_ack_active(a, now))
        .map(|a| (a.signature.as_str(), a))
        .collect();

    if !active.is_empty() {
        let original = std::mem::take(&mut report.findings);
        let mut kept = Vec::with_capacity(original.len());
        for finding in original {
            let sig: Cow<'_, str> = if finding.signature.is_empty() {
                Cow::Owned(compute_signature(&finding))
            } else {
                Cow::Borrowed(finding.signature.as_str())
            };
            if let Some(ack) = active.get(sig.as_ref()) {
                report.acknowledged_findings.push(AcknowledgedFinding {
                    finding,
                    acknowledgment: (*ack).clone(),
                });
            } else {
                kept.push(finding);
            }
        }
        report.findings = kept;
    }

    report.quality_gate = quality_gate::evaluate(&report.findings, &report.green_summary, config);
}

fn is_ack_active(ack: &Acknowledgment, now: DateTime<Utc>) -> bool {
    let Some(ref expires) = ack.expires_at else {
        return true;
    };
    let Ok(parsed) = NaiveDate::parse_from_str(expires, "%Y-%m-%d") else {
        // Malformed dates are rejected at load time; defensively treat a
        // bad value as inactive rather than ack-everything.
        return false;
    };
    // Treat the entire expiry day as still valid: an ack `expires_at =
    // 2026-12-31` is honored through 2026-12-31 23:59:59 UTC.
    let Some(end_of_day) = parsed.and_hms_opt(23, 59, 59) else {
        return false;
    };
    end_of_day.and_utc() >= now
}

/// Errors that can occur when loading the acknowledgments file.
#[derive(Debug, thiserror::Error)]
pub enum AcknowledgmentLoadError {
    #[error("Failed to read acknowledgments file: {0}")]
    Io(#[from] std::io::Error),

    #[error("Acknowledgments file exceeds the {cap}-byte cap")]
    TooLarge { cap: u64 },

    #[error("Failed to parse acknowledgments TOML: {0}")]
    Parse(toml::de::Error),

    #[error("Entry {entry_index}: invalid {field} value '{value}': {message}")]
    InvalidDate {
        entry_index: usize,
        field: &'static str,
        value: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{FindingType, Severity};
    use crate::report::{Analysis, GreenSummary, QualityGate};
    use crate::test_helpers::make_finding;
    use chrono::TimeZone;

    fn empty_report(findings: Vec<Finding>) -> Report {
        Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: findings.len(),
                traces_analyzed: 1,
            },
            findings,
            green_summary: GreenSummary::disabled(0),
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            warnings: vec![],
            acknowledged_findings: vec![],
        }
    }

    fn ack(signature: &str, expires_at: Option<&str>) -> Acknowledgment {
        Acknowledgment {
            signature: signature.to_string(),
            acknowledged_by: "test@example.com".to_string(),
            acknowledged_at: "2026-05-02".to_string(),
            reason: "test".to_string(),
            expires_at: expires_at.map(str::to_string),
        }
    }

    fn now_2026_05_02() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap()
    }

    #[test]
    fn compute_signature_deterministic() {
        let f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let sig1 = compute_signature(&f);
        let sig2 = compute_signature(&f);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn compute_signature_differs_with_template() {
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.pattern.template = "SELECT * FROM users WHERE id = ?".to_string();
        f2.pattern.template = "SELECT * FROM orders WHERE id = ?".to_string();
        assert_ne!(compute_signature(&f1), compute_signature(&f2));
    }

    #[test]
    fn compute_signature_sanitizes_endpoint() {
        let mut f = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        f.source_endpoint = "GET /api/foo bar".to_string();
        let sig = compute_signature(&f);
        let parts: Vec<&str> = sig.split(':').collect();
        assert_eq!(
            parts.len(),
            4,
            "signature must have 4 colon-separated parts: {sig}"
        );
        assert!(
            !parts[2].contains('/'),
            "endpoint segment must not contain '/'"
        );
        assert!(
            !parts[2].contains(' '),
            "endpoint segment must not contain ' '"
        );
    }

    #[test]
    fn compute_signature_strips_bidi_and_invisible_from_service_and_endpoint() {
        // service "alice<RLO>@evil.com" should produce the same signature as
        // "alice@evil.com" so a hostile span attribute cannot fork ack matching.
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let mut f2 = f1.clone();
        f1.service = "alice\u{202E}@evil.com".to_string();
        f1.source_endpoint = "GET /api/items\u{200B}".to_string();
        f2.service = "alice@evil.com".to_string();
        f2.source_endpoint = "GET /api/items".to_string();
        assert_eq!(
            compute_signature(&f1),
            compute_signature(&f2),
            "BiDi/invisible characters must be stripped before signature construction"
        );
    }

    #[test]
    fn compute_signature_format_matches_brief() {
        let mut f = make_finding(FindingType::RedundantSql, Severity::Warning);
        f.service = "order-service".to_string();
        f.source_endpoint = "POST /api/orders".to_string();
        f.pattern.template = "SELECT 1".to_string();
        let sig = compute_signature(&f);
        // Format: redundant_sql:order-service:POST_/api/orders → after sanitization
        // POST_/api/orders becomes POST__api_orders.
        let mut parts = sig.splitn(4, ':');
        assert_eq!(parts.next(), Some("redundant_sql"));
        assert_eq!(parts.next(), Some("order-service"));
        assert_eq!(parts.next(), Some("POST__api_orders"));
        let hex = parts.next().expect("hex prefix present");
        assert_eq!(hex.len(), 16, "hex prefix is 16 characters (8 bytes)");
        assert!(
            hex.chars().all(|c| c.is_ascii_hexdigit()),
            "hex prefix is hex"
        );
    }

    #[test]
    fn load_from_file_rejects_oversized_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        let payload = vec![b'x'; (MAX_ACKNOWLEDGMENTS_FILE_BYTES + 1) as usize];
        std::fs::write(&path, &payload).unwrap();
        let err = load_from_file(&path).expect_err("oversized file must fail");
        assert!(
            matches!(err, AcknowledgmentLoadError::TooLarge { .. }),
            "expected TooLarge, got: {err:?}"
        );
    }

    #[test]
    fn apply_to_report_clears_prior_acked_entries() {
        // Simulate a Report fed back from a previous --show-acknowledged
        // run: it carries one stale ack pair. Applying a fresh empty
        // ack file must drop the stale pair, the gate is re-evaluated,
        // and findings are unchanged.
        let stale_finding = make_finding(FindingType::SlowSql, Severity::Warning);
        let stale_ack = Acknowledgment {
            signature: "stale".to_string(),
            acknowledged_by: "stale@example.com".to_string(),
            acknowledged_at: "2020-01-01".to_string(),
            reason: "from a previous run".to_string(),
            expires_at: None,
        };
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let mut report = empty_report(findings);
        report.acknowledged_findings.push(AcknowledgedFinding {
            finding: stale_finding,
            acknowledgment: stale_ack,
        });
        let acks = AcknowledgmentsFile::default();
        let config = Config::default();
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert!(
            report.acknowledged_findings.is_empty(),
            "stale ack pair must be cleared on entry"
        );
        assert_eq!(report.findings.len(), 1, "active findings preserved");
    }

    #[test]
    fn load_from_file_nonexistent_returns_empty() {
        let path = std::path::PathBuf::from("/tmp/perf-sentinel-acks-does-not-exist.toml");
        let result = load_from_file(&path).expect("missing file should be Ok");
        assert!(result.acknowledged.is_empty());
    }

    #[test]
    fn load_from_file_valid_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        std::fs::write(
            &path,
            r#"
[[acknowledged]]
signature = "n_plus_one_sql:svc:GET_/a:abcd1234"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "documented"

[[acknowledged]]
signature = "redundant_sql:svc:POST_/b:11223344"
acknowledged_by = "bob@example.com"
acknowledged_at = "2026-04-20"
reason = "won't fix"
expires_at = "2026-12-31"
"#,
        )
        .unwrap();
        let parsed = load_from_file(&path).expect("valid TOML parses");
        assert_eq!(parsed.acknowledged.len(), 2);
        assert_eq!(parsed.acknowledged[0].acknowledged_by, "alice@example.com");
        assert_eq!(
            parsed.acknowledged[1].expires_at.as_deref(),
            Some("2026-12-31")
        );
    }

    #[test]
    fn load_from_file_missing_signature_field_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        std::fs::write(
            &path,
            r#"
[[acknowledged]]
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "missing signature"
"#,
        )
        .unwrap();
        let err = load_from_file(&path).expect_err("missing field must fail");
        assert!(matches!(err, AcknowledgmentLoadError::Parse(_)));
    }

    #[test]
    fn load_from_file_invalid_expires_at_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.toml");
        std::fs::write(
            &path,
            r#"
[[acknowledged]]
signature = "redundant_sql:svc:POST_/b:11223344"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "bad date"
expires_at = "not-a-date"
"#,
        )
        .unwrap();
        let err = load_from_file(&path).expect_err("invalid date must fail");
        assert!(matches!(
            err,
            AcknowledgmentLoadError::InvalidDate {
                field: "expires_at",
                ..
            }
        ));
    }

    #[test]
    fn apply_to_report_filters_matching() {
        let mut findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            make_finding(FindingType::RedundantSql, Severity::Warning),
            make_finding(FindingType::SlowSql, Severity::Warning),
        ];
        // Distinguish the templates so signatures differ.
        findings[0].pattern.template = "T1".to_string();
        findings[1].pattern.template = "T2".to_string();
        findings[2].pattern.template = "T3".to_string();
        enrich_with_signatures(&mut findings);
        let target_sig = findings[1].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, None)],
        };
        let config = Config::default();
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.acknowledged_findings.len(), 1);
        assert_eq!(
            report.acknowledged_findings[0].finding.signature,
            target_sig
        );
    }

    #[test]
    fn apply_to_report_no_match_keeps_all() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack("n_plus_one_sql:nope:nope:00000000", None)],
        };
        let config = Config::default();
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert_eq!(report.findings.len(), 1);
        assert!(report.acknowledged_findings.is_empty());
    }

    #[test]
    fn apply_to_report_expired_ack_ignored() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, Some("2020-01-01"))],
        };
        let config = Config::default();
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert_eq!(report.findings.len(), 1);
        assert!(report.acknowledged_findings.is_empty());
    }

    #[test]
    fn apply_to_report_future_ack_applied() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, Some("2030-01-01"))],
        };
        let config = Config::default();
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert!(report.findings.is_empty());
        assert_eq!(report.acknowledged_findings.len(), 1);
    }

    #[test]
    fn apply_to_report_no_expires_at_permanent() {
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let mut report = empty_report(findings);
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, None)],
        };
        let config = Config::default();
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert_eq!(report.acknowledged_findings.len(), 1);
    }

    #[test]
    fn apply_to_report_reevaluates_quality_gate() {
        // 1 critical N+1 SQL finding, default config has
        // n_plus_one_sql_critical_max = 0, so the gate fails before the
        // ack and must pass after.
        let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)];
        enrich_with_signatures(&mut findings);
        let target_sig = findings[0].signature.clone();
        let config = Config::default();
        let pre_gate = quality_gate::evaluate(&findings, &GreenSummary::disabled(0), &config);
        assert!(!pre_gate.passed, "baseline gate must fail before ack");

        let mut report = empty_report(findings);
        report.quality_gate = pre_gate;
        let acks = AcknowledgmentsFile {
            acknowledged: vec![ack(&target_sig, None)],
        };
        apply_to_report(&mut report, &acks, &config, now_2026_05_02());
        assert!(
            report.quality_gate.passed,
            "gate must flip green after the offending finding is acked"
        );
    }

    #[test]
    fn enrich_with_signatures_overwrites() {
        let mut findings = vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            make_finding(FindingType::RedundantSql, Severity::Warning),
        ];
        // Simulate stale signatures (e.g. computed under an older scheme).
        findings[0].signature = "stale".to_string();
        findings[1].signature = "also-stale".to_string();
        enrich_with_signatures(&mut findings);
        assert_ne!(findings[0].signature, "stale");
        assert_ne!(findings[1].signature, "also-stale");
        assert!(!findings[0].signature.is_empty());
        assert!(!findings[1].signature.is_empty());
    }
}
