//! SARIF v2.1.0 report export.
//!
//! Generates a [SARIF](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html)
//! report from perf-sentinel findings. Uses logical locations (service + endpoint)
//! since perf-sentinel analyzes traces, not source code.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::detect::{Finding, FindingType, Severity};
use crate::report::Report;

// ── SARIF v2.1.0 structs ───────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLog {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub version: String,
    pub runs: Vec<SarifRun>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRun {
    pub tool: SarifTool,
    pub results: Vec<SarifResult>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifTool {
    pub driver: SarifDriver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifDriver {
    pub name: String,
    pub version: String,
    pub information_uri: String,
    pub rules: Vec<SarifRule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRule {
    pub id: String,
    pub short_description: SarifMessage,
    pub default_configuration: SarifDefaultConfiguration,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifDefaultConfiguration {
    pub level: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifResult {
    pub rule_id: String,
    pub level: String,
    pub message: SarifMessage,
    pub logical_locations: Vec<SarifLogicalLocation>,
    /// SARIF v2.1.0 property bag. uses it to expose the
    /// tool-specific `confidence` field for perf-lint interop.
    /// Empty-by-default so pre-5b consumers are unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<SarifProperties>,
    /// SARIF v2.1.0 `rank` field (0-100). populates this from
    /// the finding's [`Confidence`] so SARIF consumers that don't read
    /// the custom `properties` bag still get a useful ordering signal.
    /// Mapping: `ci_batch = 30`, `daemon_staging = 60`,
    /// `daemon_production = 90`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    /// Physical source code locations from `OTel` `code.*` span attributes.
    /// Enables inline annotations in GitHub/GitLab code scanning when
    /// the instrumentation agent emits source code attributes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<SarifLocation>,
    /// SARIF v2.1.0 `fixes` array. Populated from the finding's
    /// `suggested_fix` field when a framework was inferred and a
    /// recommendation exists. Empty otherwise so pre-7.3 consumers are
    /// unaffected.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<SarifFix>,
    /// SARIF v2.1.0 section 3.27.17 `fingerprints`. Single-entry map keyed
    /// by `"perfsentinel/v1"`, value is the canonical finding signature.
    /// Used by GitHub Code Scanning and GitLab SAST for deduplication.
    /// Skipped when the source finding has no signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprints: Option<BTreeMap<String, String>>,
}

/// SARIF v2.1.0 `fix` object. perf-sentinel emits the description-only
/// form: a free-text recommendation under `description.text`. We do not
/// emit `artifactChanges` because perf-sentinel infers the fix at the
/// framework level (e.g. "use JOIN FETCH"), not as a literal source
/// patch.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifFix {
    pub description: SarifMessage,
}

/// Wrapper for SARIF `location` objects containing physical locations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLocation {
    pub physical_location: SarifPhysicalLocation,
}

/// Custom properties attached to a [`SarifResult`].
///
/// perf-sentinel specific fields that don't fit the SARIF v2.1.0 schema
/// natively. perf-lint reads this bag to boost / reduce severity in the IDE
/// based on where the finding came from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifProperties {
    /// Source context of the finding: `"ci_batch"`, `"daemon_staging"`, or
    /// `"daemon_production"`. See [`Confidence`] for semantics.
    pub confidence: &'static str,
    /// `true` when the result was emitted from an acknowledged finding
    /// (the operator suppressed it via `.perf-sentinel-acknowledgments.toml`
    /// and re-included it in the output via `--show-acknowledged`).
    /// Skipped when the result is a normal active finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<bool>,
    /// Free-text reason recorded with the ack entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgment_reason: Option<String>,
    /// Author of the ack entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgment_by: Option<String>,
    /// ISO 8601 date when the ack was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgment_at: Option<String>,
    /// Canonical perf-sentinel signature, also exposed at the result level
    /// in `fingerprints["perfsentinel/v1"]` for SARIF-native deduplication.
    /// Skipped when the source finding has no signature (legacy baselines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifMessage {
    pub text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifLogicalLocation {
    pub name: String,
    pub kind: String,
}

/// Physical source code location. Populated when the finding carries
/// a [`CodeLocation`](crate::event::CodeLocation) with at least a filepath.
/// Enables inline annotations in GitHub/GitLab code scanning.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifPhysicalLocation {
    pub artifact_location: SarifArtifactLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<SarifRegion>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifArtifactLocation {
    pub uri: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SarifRegion {
    pub start_line: u32,
}

// ── Conversion ──────────────────────────────────────────────────────

#[allow(clippy::trivially_copy_pass_by_ref)]
fn severity_to_sarif_level(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

fn finding_type_description(ft: &FindingType) -> &'static str {
    match ft {
        FindingType::NPlusOneSql => "N+1 SQL query pattern detected",
        FindingType::NPlusOneHttp => "N+1 HTTP call pattern detected",
        FindingType::RedundantSql => "Redundant SQL query detected",
        FindingType::RedundantHttp => "Redundant HTTP call detected",
        FindingType::SlowSql => "Slow SQL query pattern detected",
        FindingType::SlowHttp => "Slow HTTP call pattern detected",
        FindingType::ExcessiveFanout => "Excessive span fanout detected",
        FindingType::ChattyService => "Chatty service pattern detected",
        FindingType::PoolSaturation => "Connection pool saturation risk detected",
        FindingType::SerializedCalls => "Serialized-but-parallelizable calls detected",
    }
}

fn build_rules() -> Vec<SarifRule> {
    let variants = [
        FindingType::NPlusOneSql,
        FindingType::NPlusOneHttp,
        FindingType::RedundantSql,
        FindingType::RedundantHttp,
        FindingType::SlowSql,
        FindingType::SlowHttp,
        FindingType::ExcessiveFanout,
        FindingType::ChattyService,
        FindingType::PoolSaturation,
        FindingType::SerializedCalls,
    ];
    variants
        .iter()
        .map(|ft| SarifRule {
            id: ft.as_str().to_string(),
            short_description: SarifMessage {
                text: finding_type_description(ft).to_string(),
            },
            default_configuration: SarifDefaultConfiguration {
                level: "warning".to_string(),
            },
        })
        .collect()
}

fn finding_to_result(finding: &Finding) -> SarifResult {
    let message = format!(
        "{} in {} on {}: {} ({} occurrences, {}ms window). {}",
        finding.finding_type.as_str(),
        finding.service,
        finding.source_endpoint,
        finding.pattern.template,
        finding.pattern.occurrences,
        finding.pattern.window_ms,
        finding.suggestion
    );

    let signature = (!finding.signature.is_empty()).then(|| finding.signature.clone());
    let fingerprints = signature.as_ref().map(|sig| {
        let mut map = BTreeMap::new();
        map.insert("perfsentinel/v1".to_string(), sig.clone());
        map
    });

    SarifResult {
        rule_id: finding.finding_type.as_str().to_string(),
        level: severity_to_sarif_level(&finding.severity).to_string(),
        message: SarifMessage { text: message },
        logical_locations: vec![
            SarifLogicalLocation {
                name: finding.service.clone(),
                kind: "module".to_string(),
            },
            SarifLogicalLocation {
                name: finding.source_endpoint.clone(),
                kind: "function".to_string(),
            },
        ],
        // expose the confidence for perf-lint interop.
        properties: Some(SarifProperties {
            confidence: finding.confidence.as_str(),
            acknowledged: None,
            acknowledgment_reason: None,
            acknowledgment_by: None,
            acknowledgment_at: None,
            signature,
        }),
        rank: Some(finding.confidence.sarif_rank()),
        locations: finding
            .code_location
            .as_ref()
            .and_then(|loc| {
                loc.filepath.as_ref().and_then(|fp| {
                    sanitize_sarif_filepath(fp).map(|uri| {
                        vec![SarifLocation {
                            physical_location: SarifPhysicalLocation {
                                artifact_location: SarifArtifactLocation { uri },
                                region: loc.lineno.map(|ln| SarifRegion { start_line: ln }),
                            },
                        }]
                    })
                })
            })
            .unwrap_or_default(),
        fixes: finding
            .suggested_fix
            .as_ref()
            .map(|fix| {
                let text = match fix.reference_url.as_ref() {
                    Some(url) => format!("{} (see: {url})", fix.recommendation),
                    None => fix.recommendation.clone(),
                };
                vec![SarifFix {
                    description: SarifMessage { text },
                }]
            })
            .unwrap_or_default(),
        fingerprints,
    }
}

/// Sanitize a `code.filepath` span attribute before emitting it as a SARIF
/// `artifactLocation.uri`. Rejects potentially hostile inputs that could
/// phish a user when the SARIF report is rendered by GitHub/GitLab code
/// scanning UIs. The `code.filepath` attribute is attacker-controlled
/// (a hostile span can set it to anything).
///
/// Returns `None` when the filepath should not be emitted at all.
fn sanitize_sarif_filepath(fp: &str) -> Option<String> {
    // Reject absolute paths (POSIX `/etc/...` and Windows `\Foo\Bar`).
    if fp.starts_with('/') || fp.starts_with('\\') {
        return None;
    }

    // Reject ANY colon. This is stricter than the previous drive-letter
    // exception: legitimate source paths in instrumented apps do not
    // contain colons, and accepting them opens subtle bypasses
    // (`A:B:C://...`, `javascript:`, `data:`, etc.). If a user genuinely
    // has a colon in their source path (extremely rare), they can strip
    // it in their instrumentation layer.
    if fp.contains(':') {
        return None;
    }

    // Reject path traversal segments. Both literal `..` and percent-encoded
    // variants (`%2e%2e`, `%2E%2E`, mixed case) are rejected because SARIF
    // consumers may percent-decode the URI before resolving it.
    if fp.split(&['/', '\\'][..]).any(|seg| seg == "..") {
        return None;
    }
    if contains_percent_encoded_dot(fp) {
        return None;
    }

    // Reject double-encoded percent sequences. `%252e` decodes to `%2e`
    // under single-decode, then to `.` under a second decode. Any `%25`
    // in a source path is suspicious enough to reject.
    if fp.contains("%25") {
        return None;
    }

    // Reject overlong UTF-8 encoding of `.`. `%c0%ae` and `%e0%80%ae`
    // are non-canonical encodings of U+002E that some lax decoders
    // accept (classic IIS unicode bug). Blanket-reject any `%c0`/`%c1`
    // (2-byte overlong prefixes) and `%e0%80` (3-byte overlong prefix)
    // as cheap defense-in-depth.
    if fp.contains("%c0")
        || fp.contains("%C0")
        || fp.contains("%c1")
        || fp.contains("%C1")
        || fp.contains("%e0%80")
        || fp.contains("%E0%80")
    {
        return None;
    }

    // Reject control characters (newlines, NUL, etc.) that could break
    // the SARIF consumer's tokenizer or inject into logs.
    if fp.chars().any(char::is_control) {
        return None;
    }

    // Reject Unicode BiDi overrides and invisible format characters.
    // `char::is_control` doesn't catch these (they're format chars, not
    // control chars), but they can confuse SARIF-rendered UIs by making
    // a filename display differently than it reads. See Trojan Source
    // (CVE-2021-42574) for the class of attack.
    if fp.chars().any(is_bidi_or_invisible) {
        return None;
    }

    Some(fp.to_string())
}

/// Detect percent-encoded `.` (`%2e` / `%2E`) pairs that form a
/// `..` traversal after decoding. Handles mixed case.
fn contains_percent_encoded_dot(fp: &str) -> bool {
    let bytes = fp.as_bytes();
    let mut i = 0;
    while i + 5 < bytes.len() {
        // Match `%2e%2e` case-insensitively.
        if bytes[i] == b'%'
            && (bytes[i + 1] == b'2')
            && (bytes[i + 2] == b'e' || bytes[i + 2] == b'E')
            && bytes[i + 3] == b'%'
            && bytes[i + 4] == b'2'
            && (bytes[i + 5] == b'e' || bytes[i + 5] == b'E')
        {
            return true;
        }
        i += 1;
    }
    // Also catch mixed literal/encoded forms like `.%2e` or `%2e.`.
    if fp.contains(".%2e") || fp.contains(".%2E") {
        return true;
    }
    if fp.contains("%2e.") || fp.contains("%2E.") {
        return true;
    }
    false
}

/// Return true for Unicode `BiDi` override and invisible format characters
/// that can confuse text renderers (Trojan Source class of attack,
/// CVE-2021-42574).
fn is_bidi_or_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' // Arabic Letter Mark (BiDi formatting)
        | '\u{180E}' // Mongolian Vowel Separator (deprecated invisible)
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
        | '\u{200B}'..='\u{200F}' // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | '\u{FEFF}' // BOM / zero-width no-break space
    )
}

const SARIF_SCHEMA: &str = "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json";

/// Convert a perf-sentinel Report to a SARIF log.
///
/// Active findings come first, then acknowledged findings (when the
/// caller asked to keep them). SARIF consumers see one combined
/// `results[]` array, with ack entries marked via the `properties.acknowledged`
/// boolean so they can be filtered downstream.
#[must_use]
pub fn report_to_sarif(report: &Report) -> SarifLog {
    let mut results: Vec<SarifResult> = report.findings.iter().map(finding_to_result).collect();
    for ack in &report.acknowledged_findings {
        results.push(acknowledged_finding_to_result(ack));
    }
    SarifLog {
        schema: SARIF_SCHEMA.to_string(),
        version: "2.1.0".to_string(),
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "perf-sentinel".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    information_uri: "https://github.com/robintra/perf-sentinel".to_string(),
                    rules: build_rules(),
                },
            },
            results,
        }],
    }
}

fn acknowledged_finding_to_result(ack: &crate::report::AcknowledgedFinding) -> SarifResult {
    let mut result = finding_to_result(&ack.finding);
    // The ack metadata is operator-controlled free text. SARIF consumers
    // (GitHub Code Scanning, GitLab) escape JSON values for HTML, so XSS
    // is closed at the consumer, but BiDi / invisible-format characters
    // can still spoof the displayed identity (`alice<RLO>@evil.com`).
    // Strip them defensively at emission, matching the existing
    // `code.filepath` discipline in `sanitize_sarif_filepath`.
    let signature = (!ack.finding.signature.is_empty()).then(|| ack.finding.signature.clone());
    result.properties = Some(SarifProperties {
        confidence: ack.finding.confidence.as_str(),
        acknowledged: Some(true),
        acknowledgment_reason: Some(strip_bidi_and_invisible(&ack.acknowledgment.reason)),
        acknowledgment_by: Some(strip_bidi_and_invisible(
            &ack.acknowledgment.acknowledged_by,
        )),
        acknowledgment_at: Some(strip_bidi_and_invisible(
            &ack.acknowledgment.acknowledged_at,
        )),
        signature,
    });
    result
}

/// Drop Unicode BiDi-override and invisible-format characters from a
/// free-text string before emitting it into SARIF. Reuses the same
/// classifier as `sanitize_sarif_filepath` so the policy stays
/// consistent across SARIF surfaces.
fn strip_bidi_and_invisible(s: &str) -> String {
    s.chars().filter(|c| !is_bidi_or_invisible(*c)).collect()
}

/// Convert a slice of findings to a SARIF log. Used by `report_to_sarif`
/// and by `perf-sentinel diff --format sarif` (which only emits the
/// `new_findings` from a `DiffReport`, since "resolved" is not a
/// SARIF-native concept).
#[must_use]
pub fn findings_to_sarif(findings: &[Finding]) -> SarifLog {
    SarifLog {
        schema: SARIF_SCHEMA.to_string(),
        version: "2.1.0".to_string(),
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "perf-sentinel".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    information_uri: "https://github.com/robintra/perf-sentinel".to_string(),
                    rules: build_rules(),
                },
            },
            results: findings.iter().map(finding_to_result).collect(),
        }],
    }
}

/// Emit a Report as SARIF JSON to stdout.
///
/// # Errors
///
/// Returns an error if JSON serialization or stdout write fails.
pub fn emit_sarif(report: &Report) -> Result<(), SarifError> {
    use std::io::Write;
    let sarif = report_to_sarif(report);
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer_pretty(&mut lock, &sarif)?;
    let _ = writeln!(lock);
    Ok(())
}

/// Errors from SARIF emission.
///
/// `#[non_exhaustive]` for SemVer-minor variant additions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SarifError {
    /// JSON serialization failed.
    #[error("SARIF serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Confidence;

    fn make_report(findings: Vec<Finding>) -> Report {
        Report {
            analysis: crate::report::Analysis {
                duration_ms: 1,
                events_processed: 6,
                traces_analyzed: 1,
            },
            findings,
            green_summary: crate::report::GreenSummary::disabled(0),
            quality_gate: crate::report::QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: vec![],
            correlations: vec![],
            warnings: vec![],
            acknowledged_findings: vec![],
        }
    }

    fn make_finding(ft: FindingType, sev: Severity) -> Finding {
        let mut f = crate::test_helpers::make_finding(ft, sev);
        f.pattern.template = "SELECT * FROM order_item WHERE order_id = ?".to_string();
        f.pattern.window_ms = 250;
        f.suggestion = "Use WHERE ... IN (?)".to_string();
        f
    }

    #[test]
    fn sarif_version_is_2_1_0() {
        let report = make_report(vec![]);
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.version, "2.1.0");
        assert!(sarif.schema.contains("sarif-schema-2.1.0"));
    }

    #[test]
    fn sarif_has_all_rule_definitions() {
        let report = make_report(vec![]);
        let sarif = report_to_sarif(&report);
        let rules = &sarif.runs[0].tool.driver.rules;
        assert_eq!(rules.len(), 10);
        assert_eq!(rules[0].id, "n_plus_one_sql");
        assert_eq!(rules[6].id, "excessive_fanout");
        assert_eq!(rules[7].id, "chatty_service");
        assert_eq!(rules[8].id, "pool_saturation");
        assert_eq!(rules[9].id, "serialized_calls");
    }

    #[test]
    fn sarif_maps_severity_to_level() {
        assert_eq!(severity_to_sarif_level(&Severity::Critical), "error");
        assert_eq!(severity_to_sarif_level(&Severity::Warning), "warning");
        assert_eq!(severity_to_sarif_level(&Severity::Info), "note");
    }

    #[test]
    fn sarif_result_has_logical_locations() {
        let finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let result = finding_to_result(&finding);
        assert_eq!(result.rule_id, "n_plus_one_sql");
        assert_eq!(result.level, "warning");
        assert_eq!(result.logical_locations.len(), 2);
        assert_eq!(result.logical_locations[0].name, "order-svc");
        assert_eq!(result.logical_locations[0].kind, "module");
        assert_eq!(
            result.logical_locations[1].name,
            "POST /api/orders/42/submit"
        );
        assert_eq!(result.logical_locations[1].kind, "function");
    }

    #[test]
    fn sarif_results_from_report() {
        let report = make_report(vec![
            make_finding(FindingType::NPlusOneSql, Severity::Warning),
            make_finding(FindingType::NPlusOneHttp, Severity::Critical),
        ]);
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.runs[0].results.len(), 2);
        assert_eq!(sarif.runs[0].results[0].level, "warning");
        assert_eq!(sarif.runs[0].results[1].level, "error");
    }

    #[test]
    fn sarif_empty_findings_produces_valid_json() {
        let report = make_report(vec![]);
        let sarif = report_to_sarif(&report);
        let json = serde_json::to_string(&sarif).unwrap();
        assert!(json.contains("\"version\":\"2.1.0\""));
        assert!(json.contains("\"results\":[]"));
    }

    #[test]
    fn sarif_special_chars_in_service_and_endpoint() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.service = "svc-with-\"quotes\"".to_string();
        finding.source_endpoint = "POST /api/items?a=1&b=<2>".to_string();
        let result = finding_to_result(&finding);
        // Should serialize without error
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#"svc-with-\"quotes\""#));
        assert!(json.contains("POST /api/items?a=1&b=<2>"));
    }

    #[test]
    fn sarif_finding_without_green_impact() {
        let mut finding = make_finding(FindingType::SlowSql, Severity::Warning);
        finding.green_impact = None;
        let result = finding_to_result(&finding);
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("slow_sql"));
    }

    // --- confidence exposure via properties + rank ---

    #[test]
    fn sarif_result_contains_ci_batch_confidence_by_default() {
        // Default finding has CiBatch confidence (detector emits it).
        let finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        let result = finding_to_result(&finding);
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            json.contains(r#""confidence":"ci_batch""#),
            "SARIF should expose confidence in properties bag, got: {json}"
        );
        assert!(
            json.contains(r#""rank":30"#),
            "SARIF should populate rank=30 for CiBatch, got: {json}"
        );
    }

    #[test]
    fn sarif_result_daemon_staging_rank_60() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.confidence = Confidence::DaemonStaging;
        let result = finding_to_result(&finding);
        assert_eq!(result.rank, Some(60));
        let props = result.properties.as_ref().unwrap();
        assert_eq!(props.confidence, "daemon_staging");
    }

    #[test]
    fn sarif_result_daemon_production_rank_90() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Critical);
        finding.confidence = Confidence::DaemonProduction;
        let result = finding_to_result(&finding);
        assert_eq!(result.rank, Some(90));
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#""confidence":"daemon_production""#));
        assert!(json.contains(r#""rank":90"#));
    }

    #[test]
    fn sarif_log_round_trip_with_mixed_confidence() {
        // Full report → SARIF serialization should expose all three
        // confidence values across a mixed batch of findings.
        let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        f1.confidence = Confidence::CiBatch;
        let mut f2 = make_finding(FindingType::RedundantSql, Severity::Info);
        f2.confidence = Confidence::DaemonStaging;
        let mut f3 = make_finding(FindingType::ExcessiveFanout, Severity::Critical);
        f3.confidence = Confidence::DaemonProduction;
        let report = make_report(vec![f1, f2, f3]);
        let sarif = report_to_sarif(&report);
        assert_eq!(sarif.runs[0].results.len(), 3);
        assert_eq!(sarif.runs[0].results[0].rank, Some(30));
        assert_eq!(sarif.runs[0].results[1].rank, Some(60));
        assert_eq!(sarif.runs[0].results[2].rank, Some(90));
    }

    // ── sanitize_sarif_filepath ──────────────────────────────────

    #[test]
    fn sanitize_accepts_relative_path() {
        assert_eq!(
            sanitize_sarif_filepath("src/Order.java"),
            Some("src/Order.java".to_string())
        );
        assert_eq!(
            sanitize_sarif_filepath("order-service/src/main/OrderService.java"),
            Some("order-service/src/main/OrderService.java".to_string())
        );
    }

    #[test]
    fn sanitize_rejects_url_schemes() {
        assert_eq!(
            sanitize_sarif_filepath("http://attacker.example/steal"),
            None
        );
        assert_eq!(sanitize_sarif_filepath("https://evil.com/xss"), None);
        assert_eq!(sanitize_sarif_filepath("file:///etc/passwd"), None);
        assert_eq!(sanitize_sarif_filepath("javascript:alert(1)"), None);
    }

    #[test]
    fn sanitize_rejects_absolute_paths() {
        assert_eq!(sanitize_sarif_filepath("/etc/passwd"), None);
        assert_eq!(sanitize_sarif_filepath("\\Windows\\System32"), None);
        assert_eq!(sanitize_sarif_filepath("C:\\secret.txt"), None);
        assert_eq!(sanitize_sarif_filepath("D:/secrets"), None);
    }

    #[test]
    fn sanitize_rejects_path_traversal() {
        assert_eq!(sanitize_sarif_filepath("../etc/passwd"), None);
        assert_eq!(sanitize_sarif_filepath("src/../../../etc/passwd"), None);
        assert_eq!(sanitize_sarif_filepath("a\\..\\b"), None);
    }

    #[test]
    fn sanitize_rejects_control_characters() {
        assert_eq!(sanitize_sarif_filepath("src\nevil"), None);
        assert_eq!(sanitize_sarif_filepath("src\0Order.java"), None);
        assert_eq!(sanitize_sarif_filepath("src\rOrder.java"), None);
    }

    #[test]
    fn sanitize_rejects_bidi_override_characters() {
        // Trojan Source: a Right-to-Left Override embedded in a filename
        // makes the rendered text look different from what it resolves to.
        assert_eq!(
            sanitize_sarif_filepath("src/\u{202E}cod.rs"),
            None,
            "RLO override must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/\u{202D}evil.rs"),
            None,
            "LRO override must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/\u{2066}hidden"),
            None,
            "LRI must be rejected"
        );
    }

    #[test]
    fn sanitize_rejects_invisible_format_characters() {
        // Zero-width characters and BOM can hide path components in UIs.
        assert_eq!(
            sanitize_sarif_filepath("src/\u{200B}sneaky.rs"),
            None,
            "zero-width space must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("\u{FEFF}src/Order.rs"),
            None,
            "BOM must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/\u{061C}evil.rs"),
            None,
            "Arabic Letter Mark must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/\u{180E}hidden.rs"),
            None,
            "Mongolian Vowel Separator must be rejected"
        );
    }

    #[test]
    fn sanitize_rejects_double_encoded_sequences() {
        // `%252e%252e` decodes to `%2e%2e` on first pass, then `..` on
        // second pass. Any `%25` in a filepath is suspicious.
        assert_eq!(
            sanitize_sarif_filepath("src/%252e%252e/etc"),
            None,
            "double-encoded traversal must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/%25nasty"),
            None,
            "any %25 must be rejected"
        );
    }

    #[test]
    fn sanitize_rejects_overlong_utf8_encoding() {
        // `%c0%ae` is an overlong UTF-8 encoding of `.` that some lax
        // decoders accept (classic IIS Unicode bug).
        assert_eq!(
            sanitize_sarif_filepath("src/%c0%ae%c0%ae/etc"),
            None,
            "overlong UTF-8 dot must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/%C0%AE"),
            None,
            "uppercase overlong UTF-8 must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/%c1%anything"),
            None,
            "any %c1 must be rejected"
        );
        // 3-byte overlong UTF-8: `%e0%80%ae` decodes to `.` in lax decoders.
        // A pair forms `..`, which a permissive consumer could then resolve
        // as path traversal.
        assert_eq!(
            sanitize_sarif_filepath("src/%e0%80%ae%e0%80%ae/etc/passwd"),
            None,
            "3-byte overlong UTF-8 dot must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/%E0%80%AE"),
            None,
            "uppercase 3-byte overlong UTF-8 must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/%e0%80something"),
            None,
            "any %e0%80 prefix must be rejected"
        );
    }

    #[test]
    fn sanitize_rejects_percent_encoded_traversal() {
        assert_eq!(sanitize_sarif_filepath("src/%2e%2e/etc/passwd"), None);
        assert_eq!(sanitize_sarif_filepath("src/%2E%2E/etc/passwd"), None);
        assert_eq!(
            sanitize_sarif_filepath("src/%2e%2E/etc/passwd"),
            None,
            "mixed case %2e%2E must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/.%2e/etc"),
            None,
            "literal dot + encoded dot must be rejected"
        );
        assert_eq!(
            sanitize_sarif_filepath("src/%2e./etc"),
            None,
            "encoded dot + literal dot must be rejected"
        );
    }

    #[test]
    fn sanitize_rejects_any_colon() {
        // Previous implementation had a drive-letter exception; new
        // implementation rejects any colon unconditionally. Legitimate
        // source paths do not contain colons.
        assert_eq!(sanitize_sarif_filepath("a:b"), None);
        assert_eq!(sanitize_sarif_filepath("src:Order.java"), None);
        assert_eq!(sanitize_sarif_filepath("data:text/html,x"), None);
    }

    #[test]
    fn finding_to_sarif_rejects_hostile_filepath() {
        let mut f = crate::test_helpers::make_finding(FindingType::NPlusOneSql, Severity::Warning);
        f.code_location = Some(crate::event::CodeLocation {
            function: Some("exploit".to_string()),
            filepath: Some("http://attacker.example/steal".to_string()),
            lineno: Some(42),
            namespace: None,
        });
        let result = finding_to_result(&f);
        assert!(
            result.locations.is_empty(),
            "hostile URI must not be emitted in SARIF"
        );
    }

    #[test]
    fn finding_to_sarif_emits_safe_filepath() {
        let mut f = crate::test_helpers::make_finding(FindingType::NPlusOneSql, Severity::Warning);
        f.code_location = Some(crate::event::CodeLocation {
            function: Some("processItems".to_string()),
            filepath: Some("src/Order.java".to_string()),
            lineno: Some(42),
            namespace: None,
        });
        let result = finding_to_result(&f);
        assert_eq!(result.locations.len(), 1);
        assert_eq!(
            result.locations[0].physical_location.artifact_location.uri,
            "src/Order.java"
        );
    }

    // --- 0.5.18: signature exposed in properties + fingerprints ---

    const SAMPLE_SIGNATURE: &str = "n_plus_one_sql:order-svc:POST_/api/orders:abc12345abc12345";

    #[test]
    fn sarif_emits_signature_in_properties() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.signature = SAMPLE_SIGNATURE.to_string();
        let result = finding_to_result(&finding);
        let props = result.properties.as_ref().unwrap();
        assert_eq!(props.signature.as_deref(), Some(SAMPLE_SIGNATURE));
    }

    #[test]
    fn sarif_emits_signature_in_fingerprints() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.signature = SAMPLE_SIGNATURE.to_string();
        let result = finding_to_result(&finding);
        let fp = result.fingerprints.as_ref().unwrap();
        assert_eq!(fp.len(), 1);
        assert_eq!(
            fp.get("perfsentinel/v1").map(String::as_str),
            Some(SAMPLE_SIGNATURE)
        );
    }

    #[test]
    fn sarif_omits_signature_when_empty() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.signature = String::new();
        let result = finding_to_result(&finding);
        assert!(result.properties.as_ref().unwrap().signature.is_none());
        assert!(result.fingerprints.is_none());
        let value = serde_json::to_value(&result).unwrap();
        assert!(value.get("fingerprints").is_none());
        assert!(value["properties"].get("signature").is_none());
    }

    #[test]
    fn sarif_acknowledged_finding_carries_signature() {
        let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
        finding.signature = SAMPLE_SIGNATURE.to_string();
        let ack = crate::report::AcknowledgedFinding {
            finding,
            acknowledgment: crate::acknowledgments::Acknowledgment {
                signature: SAMPLE_SIGNATURE.to_string(),
                acknowledged_by: "alice@example.com".to_string(),
                acknowledged_at: "2026-05-03".to_string(),
                reason: "intentional cache invalidation".to_string(),
                expires_at: None,
            },
        };
        let result = acknowledged_finding_to_result(&ack);
        let props = result.properties.as_ref().unwrap();
        assert_eq!(props.acknowledged, Some(true));
        assert_eq!(props.signature.as_deref(), Some(SAMPLE_SIGNATURE));
        let fp = result.fingerprints.as_ref().unwrap();
        assert_eq!(
            fp.get("perfsentinel/v1").map(String::as_str),
            Some(SAMPLE_SIGNATURE)
        );
    }
}
