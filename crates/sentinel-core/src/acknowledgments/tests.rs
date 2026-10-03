use super::*;
use crate::detect::{FindingType, Severity};
use crate::report::{Analysis, GreenSummary, QualityGate};
use crate::test_helpers::make_finding;
use chrono::TimeZone;
use core::assert_matches;

#[test]
fn a_bare_filename_resolves_against_the_current_directory() {
    // `Path::parent` of a bare name is the empty path, which canonicalizes
    // to nothing. Reading that as "no directory" would refuse the daemon's
    // own CWD-relative default, on every platform, so this stays out of the
    // unix-only symlink module. `Cargo.toml` is the crate root's own file,
    // so it is under the CWD by construction and needs no fixture.
    assert!(symlink_stays_in_its_directory(Path::new("Cargo.toml")));
}

#[test]
fn an_absent_file_is_told_from_an_empty_one() {
    // The daemon reload keeps the previous acks on absence and replaces
    // them on an empty file, so the two must not answer alike.
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("nope.toml");
    assert!(
        load_from_file_if_present(&missing)
            .expect("absence is not an error")
            .is_none()
    );
    let empty = dir.path().join("empty.toml");
    std::fs::write(&empty, "").expect("write");
    assert!(
        load_from_file_if_present(&empty)
            .expect("an empty file parses")
            .is_some_and(|f| f.acknowledged.is_empty())
    );
}

fn empty_report(findings: Vec<Finding>) -> Report {
    Report {
        analysis: Analysis {
            duration_ms: 0,
            events_processed: findings.len(),
            traces_analyzed: 1,
            ingest: None,
        },
        findings,
        green_summary: GreenSummary::disabled(0),
        quality_gate: QualityGate {
            passed: true,
            rules: vec![],
        },
        per_endpoint_io_ops: vec![],
        correlations: vec![],
        embedded_traces: vec![],
        warnings: vec![],
        warning_details: vec![],
        acknowledged_findings: vec![],
        binary_version: String::new(),
        detection_config: None,
        disclosure_waste: None,
    }
}

fn ack(signature: &str, expires_at: Option<&str>) -> Acknowledgment {
    Acknowledgment {
        signature: signature.to_string(),
        acknowledged_by: "test@example.com".to_string(),
        acknowledged_at: "2026-05-02".to_string(),
        reason: "test".to_string(),
        expires_at: expires_at.map(str::to_string),
        service: None,
        source_endpoint: None,
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
    // Format: redundant_sql:order-service:POST_/api/orders. After sanitization,
    // POST_/api/orders becomes POST__api_orders.
    let mut parts = sig.splitn(4, ':');
    assert_eq!(parts.next(), Some("redundant_sql"));
    assert_eq!(parts.next(), Some("order-service"));
    assert_eq!(parts.next(), Some("POST__api_orders"));
    let hex = parts.next().expect("hex prefix present");
    assert_eq!(hex.len(), 32, "hex prefix is 32 characters (16 bytes)");
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "hex prefix is hex"
    );
}

#[test]
fn signature_stable_across_trace_id_changes() {
    // Core ack contract: a service restart produces new trace_id and
    // span_id values, but the same finding type on the same service /
    // endpoint / template must yield the same signature. Without this
    // invariant, ack entries silently stop matching after a restart.
    let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    let mut f2 = f1.clone();
    f1.trace_id = "aaaaaaaaaaaaaaaa0000000000000000".to_string();
    f2.trace_id = "ffffffffffffffff1111111111111111".to_string();
    assert_ne!(f1.trace_id, f2.trace_id);
    assert_eq!(
        compute_signature(&f1),
        compute_signature(&f2),
        "signature must not depend on trace_id (acks survive service restarts)"
    );
}

#[test]
fn compute_signature_differs_with_endpoint() {
    let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    let mut f2 = f1.clone();
    f1.source_endpoint = "POST /api/orders".to_string();
    f2.source_endpoint = "POST /api/users".to_string();
    assert_ne!(compute_signature(&f1), compute_signature(&f2));
}

#[test]
fn compute_signature_differs_with_service() {
    let mut f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    let mut f2 = f1.clone();
    f1.service = "order-svc".to_string();
    f2.service = "user-svc".to_string();
    assert_ne!(compute_signature(&f1), compute_signature(&f2));
}

#[test]
fn compute_signature_differs_with_finding_type() {
    let f1 = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    let f2 = make_finding(FindingType::RedundantSql, Severity::Warning);
    assert_ne!(compute_signature(&f1), compute_signature(&f2));
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
        service: None,
        source_endpoint: None,
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
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
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
signature = "n_plus_one_sql:svc:GET_/a:abcd1234abcd1234abcd1234abcd1234"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "documented"

[[acknowledged]]
signature = "redundant_sql:svc:POST_/b:11223344112233441122334411223344"
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
    assert_matches!(err, AcknowledgmentLoadError::Parse(_));
}

#[test]
fn load_from_file_invalid_expires_at_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acks.toml");
    std::fs::write(
        &path,
        r#"
[[acknowledged]]
signature = "redundant_sql:svc:POST_/b:11223344112233441122334411223344"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-04-15"
reason = "bad date"
expires_at = "not-a-date"
"#,
    )
    .unwrap();
    let err = load_from_file(&path).expect_err("invalid date must fail");
    assert_matches!(
        err,
        AcknowledgmentLoadError::InvalidDate {
            field: "expires_at",
            ..
        }
    );
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
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
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
        acknowledged: vec![ack(
            "n_plus_one_sql:nope:nope:00000000000000000000000000000000",
            None,
        )],
    };
    let config = Config::default();
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
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
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
    assert_eq!(report.findings.len(), 1);
    assert!(report.acknowledged_findings.is_empty());
}

/// The signal a fix produces: the entry is still active and nothing in
/// the run carries its signature, so it is reported as removable.
#[test]
fn apply_to_report_reports_an_ack_that_matched_nothing() {
    let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
    enrich_with_signatures(&mut findings);
    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack("deadbeef", None)],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    assert_eq!(report.findings.len(), 1, "the unrelated finding survives");
    let unmatched: Vec<&Warning> = report
        .warning_details
        .iter()
        .filter(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .collect();
    assert_eq!(unmatched.len(), 1);
    assert!(
        unmatched[0].message.contains("deadbeef"),
        "the warning must name the entry to remove, got: {}",
        unmatched[0].message
    );
}

/// A pre-computed report may already be ack-filtered and its I/O op
/// counts describe another run, so no unmatched warning may be derived
/// from it, not even the indeterminate one.
#[test]
fn apply_to_report_precomputed_origin_emits_no_unmatched_warning() {
    let mut report = empty_report(vec![]);
    report.per_endpoint_io_ops = vec![crate::report::PerEndpointIoOps {
        service: "order-service".to_string(),
        endpoint: "GET /api/orders".to_string(),
        io_ops: 12,
    }];
    let acks = AcknowledgmentsFile {
        acknowledged: vec![Acknowledgment {
            service: Some("order-service".to_string()),
            source_endpoint: Some("GET /api/orders".to_string()),
            ..ack("deadbeef", None)
        }],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::Precomputed,
    );
    assert!(
        !report
            .warning_details
            .iter()
            .any(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT),
        "a precomputed report must not claim anything, got: {:?}",
        report.warning_details
    );
}

/// With service and endpoint on the entry, an exercised endpoint that
/// produced no finding reads as fixed, while an absent one proves nothing.
#[test]
fn apply_to_report_unmatched_ack_splits_fixed_from_not_run() {
    let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
    enrich_with_signatures(&mut findings);
    let mut report = empty_report(findings);
    report.per_endpoint_io_ops = vec![crate::report::PerEndpointIoOps {
        service: "order-service".to_string(),
        endpoint: "GET /api/orders".to_string(),
        io_ops: 12,
    }];
    let located = |sig: &str, endpoint: &str| Acknowledgment {
        service: Some("order-service".to_string()),
        source_endpoint: Some(endpoint.to_string()),
        ..ack(sig, None)
    };
    let acks = AcknowledgmentsFile {
        acknowledged: vec![
            located("aaaa-exercised", "GET /api/orders"),
            located("bbbb-not-run", "GET /api/legacy/export"),
        ],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let messages: Vec<&str> = report
        .warning_details
        .iter()
        .filter(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .map(|w| w.message.as_str())
        .collect();
    assert_eq!(messages.len(), 2);
    assert!(
        messages[0].contains("aaaa-exercised") && messages[0].contains("looks fixed"),
        "exercised endpoint must read as fixed, got: {}",
        messages[0]
    );
    assert!(
        messages[1].contains("bbbb-not-run") && messages[1].contains("proves nothing"),
        "absent endpoint must prove nothing, got: {}",
        messages[1]
    );
}

/// A template mutation shifts the signature's hash suffix while the
/// detector, service, and endpoint stay put. With exactly one such
/// finding, the warning names it instead of claiming "looks fixed".
#[test]
fn apply_to_report_unmatched_ack_names_the_drifted_successor() {
    let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
    enrich_with_signatures(&mut findings);
    let successor_sig = findings[0].signature.clone();
    let (prefix, _) = successor_sig.rsplit_once(':').expect("4-segment signature");
    let acked_old = format!("{prefix}:{}", "0".repeat(32));

    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack(&acked_old, None)],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let warning = report
        .warning_details
        .iter()
        .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .expect("unmatched warning");
    assert!(
        warning.message.contains(&successor_sig) && warning.message.contains("drifted"),
        "warning must name the successor signature, got: {}",
        warning.message
    );
    assert_eq!(report.findings.len(), 1, "the successor stays unsuppressed");
}

/// A `service.name` that starts resolving differently moves the
/// signature's prefix while the template hash stays put. The warning
/// must name the successor instead of reading as "possibly fixed",
/// which is what would send an operator to delete a live suppression.
#[test]
fn apply_to_report_unmatched_ack_names_the_reattributed_successor() {
    let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
    findings[0].service = "unknown".to_string();
    enrich_with_signatures(&mut findings);
    let successor_sig = findings[0].signature.clone();

    // The same finding as it signed before the service resolved.
    let mut before = findings[0].clone();
    before.service = String::new();
    let acked_old = compute_signature(&before);
    assert_ne!(acked_old, successor_sig);

    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack(&acked_old, None)],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let warning = report
        .warning_details
        .iter()
        .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .expect("unmatched warning");
    assert!(
        warning.message.contains(&successor_sig) && warning.message.contains("attribution"),
        "warning must name the re-attributed successor, got: {}",
        warning.message
    );
    assert_eq!(report.findings.len(), 1, "the successor stays unsuppressed");
}

/// A same-prefix template drift outranks a same-hash finding
/// elsewhere: the second must not turn the named successor back into
/// the generic message.
#[test]
fn apply_to_report_template_drift_outranks_an_attribution_candidate() {
    let acked_old = compute_signature(&make_finding(FindingType::NPlusOneSql, Severity::Warning));
    let mut drifted = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    drifted.pattern.template = "SELECT * FROM t WHERE id = ? AND tenant = ?".to_string();
    let mut moved = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    moved.source_endpoint = "GET /api/other".to_string();
    let mut findings = vec![drifted, moved];
    enrich_with_signatures(&mut findings);
    let successor_sig = findings[0].signature.clone();

    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack(&acked_old, None)],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let warning = report
        .warning_details
        .iter()
        .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .expect("unmatched warning");
    assert!(
        warning.message.contains(&successor_sig) && warning.message.contains("drifted"),
        "the template drift must be named, got: {}",
        warning.message
    );
}

/// A sibling service running the same template on the same endpoint
/// is not a moved attribution while the acked service still emits
/// I/O there: the observed verdict applies.
#[test]
fn apply_to_report_sibling_service_is_not_a_move_while_the_acked_one_emits() {
    let acked_old = compute_signature(&make_finding(FindingType::NPlusOneSql, Severity::Warning));
    let mut sibling = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    sibling.service = "billing-svc".to_string();
    let mut findings = vec![sibling];
    enrich_with_signatures(&mut findings);

    let mut report = empty_report(findings);
    report.per_endpoint_io_ops = vec![crate::report::PerEndpointIoOps {
        service: "order-svc".to_string(),
        endpoint: "POST /api/orders/42/submit".to_string(),
        io_ops: 12,
    }];
    let acks = AcknowledgmentsFile {
        acknowledged: vec![Acknowledgment {
            service: Some("order-svc".to_string()),
            source_endpoint: Some("POST /api/orders/42/submit".to_string()),
            ..ack(&acked_old, None)
        }],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let warning = report
        .warning_details
        .iter()
        .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .expect("unmatched warning");
    assert!(
        warning.message.contains("looks fixed") && !warning.message.contains("attribution"),
        "an exercised acked location is not a move, got: {}",
        warning.message
    );
}

/// Two findings sharing the ack's prefix would make naming either one
/// a guess, so the message stays generic.
#[test]
fn apply_to_report_two_drift_candidates_keep_the_generic_message() {
    let mut second = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    second.pattern.template = "SELECT * FROM t WHERE id = ? AND tenant = ?".to_string();
    let mut findings = vec![
        make_finding(FindingType::NPlusOneSql, Severity::Warning),
        second,
    ];
    enrich_with_signatures(&mut findings);
    let (prefix, _) = findings[0]
        .signature
        .rsplit_once(':')
        .map(|(p, h)| (p.to_string(), h))
        .expect("4-segment signature");
    let acked_old = format!("{prefix}:{}", "0".repeat(32));

    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack(&acked_old, None)],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let warning = report
        .warning_details
        .iter()
        .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .expect("unmatched warning");
    assert!(
        !warning.message.contains("drifted"),
        "two candidates must not be guessed between, got: {}",
        warning.message
    );
}

/// The signature prefix is not injective when service or endpoint
/// contains a colon. An ack carrying its structured fields must not
/// name a prefix-colliding but unrelated finding as its successor.
#[test]
fn drift_hint_rejects_a_prefix_collision_when_fields_are_present() {
    // service "svc:extra" + endpoint "foo" collides with
    // service "svc" + endpoint "extra:foo" on the raw prefix.
    let mut finding = make_finding(FindingType::NPlusOneSql, Severity::Warning);
    finding.service = "svc".to_string();
    finding.source_endpoint = "extra:foo".to_string();
    let mut findings = vec![finding];
    enrich_with_signatures(&mut findings);
    let (prefix, _) = findings[0]
        .signature
        .rsplit_once(':')
        .map(|(p, h)| (p.to_string(), h))
        .expect("4-segment signature");
    let acked_old = format!("{prefix}:{}", "0".repeat(32));

    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![Acknowledgment {
            service: Some("svc:extra".to_string()),
            source_endpoint: Some("foo".to_string()),
            ..ack(&acked_old, None)
        }],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    let warning = report
        .warning_details
        .iter()
        .find(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
        .expect("unmatched warning");
    assert!(
        !warning.message.contains("drifted"),
        "a prefix collision must not be sold as a drift, got: {}",
        warning.message
    );
}

/// An ack doing its job is not noise, and an expired one is inactive,
/// so neither may be reported as removable.
#[test]
fn apply_to_report_does_not_report_matched_or_expired_acks() {
    let mut findings = vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)];
    enrich_with_signatures(&mut findings);
    let target_sig = findings[0].signature.clone();
    let mut report = empty_report(findings);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![
            ack(&target_sig, None),
            ack("expired-and-unmatched", Some("2020-01-01")),
        ],
    };
    apply_to_report(
        &mut report,
        &acks,
        &Config::default(),
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    assert_eq!(report.acknowledged_findings.len(), 1);
    assert!(
        !report
            .warning_details
            .iter()
            .any(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT),
        "got: {:?}",
        report.warning_details
    );
}

/// Re-applying over a baseline that already carries the warnings must
/// not stack them, for the same reason ack pairs are cleared on entry.
#[test]
fn apply_to_report_does_not_accumulate_unmatched_warnings() {
    let mut report = empty_report(vec![]);
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack("deadbeef", None)],
    };
    let config = Config::default();
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );

    assert_eq!(
        report
            .warning_details
            .iter()
            .filter(|w| w.kind == warnings::UNMATCHED_ACKNOWLEDGMENT)
            .count(),
        1
    );
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
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
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
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
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
    let pre_gate = quality_gate::evaluate(
        &findings,
        &GreenSummary::disabled(0),
        &config.thresholds,
        None,
    );
    assert!(!pre_gate.passed, "baseline gate must fail before ack");

    let mut report = empty_report(findings);
    report.quality_gate = pre_gate;
    let acks = AcknowledgmentsFile {
        acknowledged: vec![ack(&target_sig, None)],
    };
    apply_to_report(
        &mut report,
        &acks,
        &config,
        now_2026_05_02(),
        ReportOrigin::FreshAnalysis,
    );
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

#[test]
fn signature_ignores_scopes_code_location_and_suggested_fix() {
    let bare = make_finding(FindingType::PoolSaturation, Severity::Warning);
    let mut enriched = bare.clone();
    enriched.instrumentation_scopes = vec!["io.opentelemetry.jdbc".to_string()];
    enriched.code_location = Some(crate::event::CodeLocation {
        function: None,
        filepath: Some("OrderRepository.java".to_string()),
        lineno: Some(42),
        namespace: Some("com.example.OrderRepository".to_string()),
    });
    crate::detect::suggestions::enrich(std::slice::from_mut(&mut enriched));
    assert!(enriched.suggested_fix.is_some());
    assert_eq!(compute_signature(&bare), compute_signature(&enriched));
}
