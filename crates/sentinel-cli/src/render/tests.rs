#[test]
fn recurrence_index_groups_by_signature_and_sums_ops() {
    let mut a = sample_finding();
    a.signature = "sig-a".to_string();
    let mut b = sample_finding();
    b.signature = "sig-a".to_string();
    b.trace_id = "trace-2".to_string();
    let mut c = sample_finding();
    c.signature = "sig-c".to_string();
    let index = build_recurrence_index(&[a, b, c]);
    assert_eq!(index.len(), 2);
    let sig_a = &index["sig-a"];
    assert_eq!(sig_a.count, 2);
    assert_eq!(sig_a.total_ops, 10, "2 detections x 5 avoidable ops");
    assert_eq!(index["sig-c"].count, 1);
}

#[test]
fn recurrence_key_falls_back_when_signature_is_empty() {
    let mut f = sample_finding();
    f.signature = String::new();
    let key = recurrence_key(&f);
    assert!(key.contains(&f.service));
    assert!(key.contains(&f.pattern.template));
    let mut same = sample_finding();
    same.signature = String::new();
    same.trace_id = "other-trace".to_string();
    assert_eq!(
        key,
        recurrence_key(&same),
        "trace id must not split the group"
    );
}

#[test]
fn recurrence_key_keeps_groupings_separate() {
    let ns = |value: &str| {
        vec![GroupingAttribute {
            key: "k8s.namespace.name".into(),
            value: value.into(),
        }]
    };
    let mut prod = sample_finding();
    prod.signature = "sig-a".to_string();
    prod.grouping = ns("prod-eu");

    let mut staging = prod.clone();
    staging.grouping = ns("staging");
    assert_ne!(recurrence_key(&prod), recurrence_key(&staging));

    prod.grouping = vec![GroupingAttribute {
        key: "tenant.id".into(),
        value: "prod".into(),
    }];
    staging.grouping = vec![GroupingAttribute {
        key: "k8s.namespace.name".into(),
        value: "prod".into(),
    }];
    assert_ne!(recurrence_key(&prod), recurrence_key(&staging));

    staging.grouping.clear();
    assert_ne!(
        recurrence_key(&prod),
        recurrence_key(&staging),
        "no grouping at all must not collide with a grouped finding"
    );

    // Length-prefixed: a value carrying the separator cannot forge
    // another finding's key.
    prod.grouping = ns("a|b");
    prod.signature = "c".to_string();
    staging.grouping = ns("a");
    staging.signature = "b|c".to_string();
    assert_ne!(recurrence_key(&prod), recurrence_key(&staging));
}

#[test]
fn a_worse_severity_still_prints_in_full_after_a_stub() {
    // Signatures ignore severity, so a critical arriving after a
    // warning on the same signature must not be reduced to a stub.
    let mut warn = sample_finding();
    warn.signature = "sig".to_string();
    warn.severity = Severity::Warning;
    let mut crit = sample_finding();
    crit.signature = "sig".to_string();
    crit.severity = Severity::Critical;
    crit.trace_id = "trace-crit".to_string();
    let findings = vec![warn, crit];

    // The fold prints to stdout, so assert on the decision the loop
    // makes: the critical outranks the warning already printed.
    let mut printed_worst: HashMap<String, Severity> = HashMap::new();
    let mut full_blocks = 0;
    for f in &findings {
        let key = recurrence_key(f);
        let outranks = printed_worst
            .get(&key)
            .is_none_or(|worst| f.severity < *worst);
        if outranks {
            printed_worst.insert(key, f.severity.clone());
            full_blocks += 1;
        }
    }
    assert_eq!(full_blocks, 2, "the critical must get its own full block");
    assert_eq!(printed_worst["sig"], Severity::Critical);
}

#[test]
fn sort_by_impact_puts_the_frequent_info_first() {
    let mut crit = sample_finding();
    crit.signature = "sig-crit".to_string();
    crit.severity = Severity::Critical;
    let mut infos: Vec<Finding> = (0..5)
        .map(|i| {
            let mut f = sample_finding();
            f.signature = "sig-info".to_string();
            f.severity = Severity::Info;
            f.trace_id = format!("t{i}");
            f
        })
        .collect();
    let mut findings = vec![crit];
    findings.append(&mut infos);
    sort_findings(&mut findings, FindingsSort::Impact);
    assert_eq!(
        findings[0].severity,
        Severity::Info,
        "5x5 = 25 aggregate ops must outrank the critical's 5"
    );
    // Assert the FULL order: the sort permutation here is a six-element
    // rotation, and applying its inverse also puts an Info first, so a
    // head-only assert passes over a broken apply_permutation. The
    // critical must land last.
    assert!(
        findings[..5].iter().all(|f| f.severity == Severity::Info),
        "all five infos precede the critical"
    );
    assert_eq!(findings[5].severity, Severity::Critical);
    sort_findings(&mut findings, FindingsSort::Severity);
    assert_eq!(findings[0].severity, Severity::Critical);
}

#[test]
fn sort_applies_the_permutation_not_its_inverse() {
    // Three distinct impacts whose sort permutation is a 3-cycle: the
    // inverse of a 3-cycle is the other 3-cycle, so a convention mix-up
    // in apply_permutation breaks this ordering.
    let mut findings: Vec<Finding> = [(1u64, "a"), (3, "b"), (2, "c")]
        .iter()
        .map(|(ops, sig)| {
            let mut f = sample_finding();
            f.signature = (*sig).to_string();
            f.severity = Severity::Info;
            f.green_impact = Some(GreenImpact {
                estimated_extra_io_ops: *ops as usize,
                io_intensity_score: 1.0,
                io_intensity_band: InterpretationLevel::for_iis(1.0),
            });
            f
        })
        .collect();
    sort_findings(&mut findings, FindingsSort::Impact);
    let order: Vec<&str> = findings.iter().map(|f| f.signature.as_str()).collect();
    assert_eq!(order, ["b", "c", "a"], "descending by aggregate ops");
}

use super::*;
use crate::render;
use core::assert_matches;
use sentinel_core::detect::Confidence;
use sentinel_core::detect::suggestions::SuggestedFix;
use sentinel_core::detect::{Finding, FindingType, GreenImpact, Pattern};
use sentinel_core::diff::DiffReport;
use sentinel_core::event::CodeLocation;
use sentinel_core::event::GroupingAttribute;
use sentinel_core::report::interpret::InterpretationLevel;
use sentinel_core::report::{Analysis, GreenSummary, QualityGate, QualityRule, TopOffender};

/// A gate breach must win over a concurrent write failure so a
/// regression is never masked as the tolerable `EXIT_TOOLING_ERROR`.
#[test]
fn gate_breach_takes_precedence_over_write_failure() {
    assert_eq!(exit_code_after_gate(true, true), Some(1));
    assert_eq!(exit_code_after_gate(true, false), Some(1));
}

#[test]
fn write_failure_without_gate_breach_is_tooling_error() {
    assert_eq!(
        exit_code_after_gate(false, true),
        Some(crate::EXIT_TOOLING_ERROR)
    );
}

#[test]
fn clean_run_has_no_exit_code() {
    assert_eq!(exit_code_after_gate(false, false), None);
}

fn empty_diff() -> DiffReport {
    DiffReport {
        new_findings: vec![],
        resolved_findings: vec![],
        severity_changes: vec![],
        endpoint_metric_deltas: vec![],
        warning_details: vec![],
        mutated_findings: vec![],
    }
}

fn sample_finding() -> Finding {
    Finding {
        finding_type: FindingType::NPlusOneSql,
        severity: Severity::Warning,
        trace_id: "trace-1".to_string(),
        service: "order-svc".to_string(),
        grouping: Vec::new(),
        source_endpoint: "POST /api/orders/42/submit".to_string(),
        pattern: Pattern {
            template: "SELECT * FROM order_item WHERE order_id = ?".to_string(),
            occurrences: 6,
            window_ms: 7_000,
            distinct_params: 6,
            ..Default::default()
        },
        suggestion: "Use WHERE ... IN (?) to batch 5 queries into one".to_string(),
        first_timestamp: "2026-04-20T10:00:01.000Z".to_string(),
        last_timestamp: "2026-04-20T10:00:08.000Z".to_string(),
        green_impact: Some(GreenImpact {
            estimated_extra_io_ops: 5,
            io_intensity_score: 6.0,
            io_intensity_band: InterpretationLevel::for_iis(6.0),
        }),
        confidence: Confidence::CiBatch,
        classification_method: None,
        code_location: Some(CodeLocation {
            function: Some("findItems".to_string()),
            filepath: Some("src/main/java/orders/OrderService.java".to_string()),
            lineno: Some(118),
            namespace: Some("com.foo.orders.OrderService".to_string()),
        }),
        instrumentation_scopes: Vec::new(),
        suggested_fix: Some(SuggestedFix {
            pattern: "n_plus_one_sql".to_string(),
            framework: "java_jpa".to_string(),
            recommendation: "Use @BatchSize on the lazy collection".to_string(),
            reference_url: Some("https://docs.example.com/batch".to_string()),
        }),
        signature: String::new(),
    }
}

/// A baseline JSON is operator input and ack warnings carry
/// user-authored signatures, so this block is a trust boundary.
#[test]
fn diff_warnings_are_sanitized_like_the_report_block() {
    let mut diff = empty_diff();
    diff.warning_details = vec![sentinel_core::report::Warning::new(
        "tuning",
        "wipe\x1b[2J\x1b[H and \u{202e}reversed",
    )];

    let mut buf = Vec::new();
    write_diff_text(&mut buf, &diff, no_colors()).unwrap();

    assert!(
        !buf.contains(&0x1b),
        "ESC byte leaked into the diff warnings block: {}",
        String::from_utf8_lossy(&buf)
    );
    assert!(
        !String::from_utf8_lossy(&buf).contains('\u{202e}'),
        "BiDi override leaked into the diff warnings block"
    );
}

fn diff_with_new(findings: Vec<Finding>) -> DiffReport {
    DiffReport {
        new_findings: findings,
        resolved_findings: vec![],
        severity_changes: vec![],
        endpoint_metric_deltas: vec![],
        warning_details: vec![],
        mutated_findings: vec![],
    }
}

fn render_text(diff: &DiffReport) -> String {
    let mut buf = Vec::new();
    write_diff_text(&mut buf, diff, no_colors()).unwrap();
    String::from_utf8(buf).expect("render output should be valid UTF-8")
}

/// Regression: `write_diff_text` must honor the `colors` argument,
/// not probe stdout's TTY state. When `emit_diff` writes to a file,
/// it passes `no_colors()` and the output must contain zero ESC
/// bytes regardless of whether the process stdout is a terminal.
#[test]
fn diff_block_names_the_attribute_that_split_the_finding() {
    let mut prod = sample_finding();
    prod.grouping = vec![
        GroupingAttribute {
            key: "tenant.id".into(),
            value: "acme".into(),
        },
        GroupingAttribute {
            key: "k8s.namespace.name".into(),
            value: "shared-cluster".into(),
        },
    ];
    let plain = sample_finding();

    let mut buf = Vec::new();
    write_diff_text(&mut buf, &diff_with_new(vec![prod, plain]), no_colors()).unwrap();
    let out = String::from_utf8(buf).unwrap();

    // The attribute name is the label, so a tenant never reads as a
    // namespace, and every captured attribute is shown.
    assert_eq!(out.matches("tenant.id:").count(), 1, "{out}");
    assert_eq!(out.matches("k8s.namespace.name:").count(), 1, "{out}");
    assert!(
        out.contains("acme") && out.contains("shared-cluster"),
        "{out}"
    );
}

#[test]
fn write_diff_text_respects_colors_argument() {
    let diff = empty_diff();

    let forced = AnsiColors {
        bold: "\x1b[1m",
        cyan: "\x1b[36m",
        red: "\x1b[31m",
        yellow: "\x1b[33m",
        green: "\x1b[32m",
        dim: "\x1b[2m",
        reset: "\x1b[0m",
    };
    let mut colored_buf = Vec::new();
    write_diff_text(&mut colored_buf, &diff, forced).unwrap();
    assert!(
        colored_buf.contains(&0x1b),
        "forced palette must emit at least one ESC byte"
    );

    let mut plain_buf = Vec::new();
    write_diff_text(&mut plain_buf, &diff, no_colors()).unwrap();
    assert!(
        !plain_buf.contains(&0x1b),
        "no_colors palette must emit zero ESC bytes, got:\n{}",
        String::from_utf8_lossy(&plain_buf)
    );
}

#[test]
fn new_finding_with_all_fields_renders_every_label() {
    let out = render_text(&diff_with_new(vec![sample_finding()]));
    assert!(out.contains("Template:"), "missing Template, got:\n{out}");
    assert!(
        out.contains("Occurrences:") && out.contains(" 6"),
        "missing Occurrences, got:\n{out}"
    );
    assert!(out.contains("Window:"), "missing Window, got:\n{out}");
    let local = fmt_local_iso("2026-04-20T10:00:01.000Z", "%Y-%m-%d %H:%M");
    assert!(
        out.contains(&format!("{local} -> {local} (7s)")),
        "window line wrong, got:\n{out}"
    );
    assert!(
        out.contains("Suggestion:"),
        "missing Suggestion, got:\n{out}"
    );
    assert!(
        out.contains("Fix [java_jpa]:")
            && out.contains("Use @BatchSize on the lazy collection")
            && out.contains("(https://docs.example.com/batch)"),
        "missing or wrong fix line, got:\n{out}"
    );
    assert!(out.contains("IIS:"), "missing IIS, got:\n{out}");
    assert!(
        out.contains("Extra I/O:") && out.contains("5 avoidable ops"),
        "missing Extra I/O, got:\n{out}"
    );
    assert!(out.contains("Location:"), "missing Location, got:\n{out}");
    assert!(
        out.contains("src/main/java/orders/OrderService.java:118"),
        "location filepath/lineno wrong, got:\n{out}"
    );
}

#[test]
fn new_finding_without_green_impact_omits_iis_and_extra_io() {
    let mut f = sample_finding();
    f.green_impact = None;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(!out.contains("IIS:"), "IIS leaked, got:\n{out}");
    assert!(!out.contains("Extra I/O:"), "Extra I/O leaked, got:\n{out}");
}

#[test]
fn extra_io_is_printed_even_when_zero() {
    let mut f = sample_finding();
    if let Some(ref mut imp) = f.green_impact {
        imp.estimated_extra_io_ops = 0;
    }
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        out.contains("Extra I/O:") && out.contains("0 avoidable ops"),
        "Extra I/O must be printed at zero for parity with analyze, got:\n{out}"
    );
}

#[test]
fn new_finding_without_suggested_fix_omits_fix_line() {
    let mut f = sample_finding();
    f.suggested_fix = None;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(!out.contains("Fix ["), "fix line leaked, got:\n{out}");
}

#[test]
fn new_finding_without_code_location_omits_location_line() {
    let mut f = sample_finding();
    f.code_location = None;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.contains("Location:"),
        "Location line leaked, got:\n{out}"
    );
}

#[test]
fn ci_batch_confidence_is_omitted() {
    let f = sample_finding();
    assert_eq!(f.confidence, Confidence::CiBatch);
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.contains("Confidence:"),
        "ci_batch confidence must not be printed, got:\n{out}"
    );
}

#[test]
fn local_batch_confidence_is_omitted() {
    // Both batch contexts (local + CI) stay quiet in the terminal. Only
    // the stronger daemon signals print a Confidence line.
    let mut f = sample_finding();
    f.confidence = Confidence::LocalBatch;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.contains("Confidence:"),
        "local_batch confidence must not be printed, got:\n{out}"
    );
}

#[test]
fn daemon_production_confidence_is_printed() {
    let mut f = sample_finding();
    f.confidence = Confidence::DaemonProduction;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        out.contains("Confidence:") && out.contains("daemon_production"),
        "daemon_production confidence missing, got:\n{out}"
    );
}

#[test]
fn window_under_one_minute_renders_in_seconds() {
    let mut f = sample_finding();
    f.pattern.window_ms = 5_000;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(out.contains("(5s)"), "expected (5s), got:\n{out}");
}

#[test]
fn window_over_two_hours_renders_with_hours_and_minutes() {
    let mut f = sample_finding();
    f.pattern.window_ms = (2 * 60 * 60 + 12 * 60) * 1_000;
    let out = render_text(&diff_with_new(vec![f]));
    assert!(out.contains("(2h12m)"), "expected (2h12m), got:\n{out}");
}

#[test]
fn resolved_findings_use_same_enriched_format() {
    let diff = DiffReport {
        new_findings: vec![],
        resolved_findings: vec![sample_finding()],
        severity_changes: vec![],
        endpoint_metric_deltas: vec![],
        warning_details: vec![],
        mutated_findings: vec![],
    };
    let out = render_text(&diff);
    assert!(
        out.contains("Resolved findings"),
        "missing resolved header, got:\n{out}"
    );
    assert!(
        out.contains("Occurrences:") && out.contains(" 6"),
        "resolved finding must carry Occurrences, got:\n{out}"
    );
    assert!(
        out.contains("Suggestion:"),
        "resolved finding must carry Suggestion, got:\n{out}"
    );
    assert!(
        out.contains("Fix [java_jpa]:"),
        "resolved finding must carry Fix line, got:\n{out}"
    );
}

#[test]
fn mutated_findings_render_a_before_after_pair() {
    let before = sample_finding();
    let mut after = sample_finding();
    after.pattern.template =
        "SELECT * FROM order_item WHERE order_id = ? AND tenant = ?".to_string();
    let mut diff = empty_diff();
    diff.mutated_findings = vec![sentinel_core::diff::MutatedFinding { before, after }];

    let out = render_text(&diff);
    assert!(
        out.contains("Mutated findings (1):"),
        "missing mutated header, got:\n{out}"
    );
    assert!(
        out.contains("Before: SELECT * FROM order_item WHERE order_id = ?"),
        "missing before template, got:\n{out}"
    );
    assert!(
        out.contains("After:  SELECT * FROM order_item WHERE order_id = ? AND tenant = ?"),
        "missing after template, got:\n{out}"
    );
    assert!(
        !out.contains("No differences detected"),
        "a mutation alone must not read as a clean diff, got:\n{out}"
    );
}

#[test]
fn ansi_escape_in_template_is_stripped_from_text_output() {
    let mut f = sample_finding();
    f.pattern.template = "evil\x1b[2J\x1b[H wipe".to_string();
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.as_bytes().contains(&0x1b),
        "ESC byte from user template leaked into terminal output, got:\n{out}"
    );
    assert!(
        out.contains("evil???[2J???[H wipe") || out.contains("evil?[2J?[H wipe"),
        "control chars must be replaced, got:\n{out}"
    );
}

#[test]
fn osc8_hyperlink_in_suggestion_is_neutralised() {
    let mut f = sample_finding();
    f.suggestion = "click \x1b]8;;https://attacker/\x07here\x1b]8;;\x07".to_string();
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.as_bytes().contains(&0x1b),
        "OSC 8 ESC leaked, got:\n{out}"
    );
    assert!(
        !out.as_bytes().contains(&0x07),
        "BEL terminator leaked, got:\n{out}"
    );
}

/// The suggestion carries backticks so the HTML can chip them. Every
/// terminal surface must strip them, like `suggested_fix`.
#[test]
fn backticks_in_suggestion_never_reach_the_terminal() {
    let mut f = sample_finding();
    f.suggestion = "Use `WHERE ... IN (?)` to batch 5 queries into one".to_string();
    let out = render_text(&diff_with_new(vec![f]));
    assert!(!out.contains('`'), "backtick leaked, got:\n{out}");
    assert!(out.contains("WHERE ... IN (?)"), "got:\n{out}");
}

#[test]
fn non_https_reference_url_is_omitted_from_fix_line() {
    let mut f = sample_finding();
    if let Some(ref mut fix) = f.suggested_fix {
        fix.reference_url = Some("http://insecure.example.com/doc".to_string());
    }
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.contains("http://insecure.example.com/doc"),
        "non-HTTPS URL must not be printed, got:\n{out}"
    );
    assert!(
        out.contains("Fix [java_jpa]:") && out.contains("Use @BatchSize"),
        "fix recommendation must still render, got:\n{out}"
    );
}

#[test]
fn javascript_scheme_reference_url_is_omitted() {
    let mut f = sample_finding();
    if let Some(ref mut fix) = f.suggested_fix {
        fix.reference_url = Some("javascript:alert(1)".to_string());
    }
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.contains("javascript:"),
        "javascript: URL must not be printed, got:\n{out}"
    );
}

#[test]
fn url_with_control_chars_is_omitted() {
    let mut f = sample_finding();
    if let Some(ref mut fix) = f.suggested_fix {
        fix.reference_url = Some("https://docs.example.com/\x1b[31m".to_string());
    }
    let out = render_text(&diff_with_new(vec![f]));
    assert!(
        !out.contains("https://docs.example.com/"),
        "URL with control chars must not be printed, got:\n{out}"
    );
}

fn finding_with(severity: Severity, ftype: FindingType) -> Finding {
    let mut f = sample_finding();
    f.severity = severity;
    f.finding_type = ftype;
    f
}

#[test]
fn severity_breakdown_is_none_for_empty_input() {
    assert!(format_severity_breakdown(&[], no_colors()).is_none());
}

#[test]
fn severity_breakdown_counts_each_bucket() {
    let findings = vec![
        finding_with(Severity::Critical, FindingType::NPlusOneSql),
        finding_with(Severity::Critical, FindingType::SlowSql),
        finding_with(Severity::Warning, FindingType::RedundantSql),
        finding_with(Severity::Info, FindingType::SlowHttp),
    ];
    let line = format_severity_breakdown(&findings, no_colors())
        .expect("non-empty input must yield a line");
    assert!(line.contains("2 critical"), "got: {line}");
    assert!(line.contains("1 warning"), "got: {line}");
    assert!(line.contains("1 info"), "got: {line}");
}

#[test]
fn top_finding_types_skipped_when_five_or_fewer() {
    let findings = vec![
        finding_with(Severity::Warning, FindingType::NPlusOneSql),
        finding_with(Severity::Warning, FindingType::NPlusOneSql),
        finding_with(Severity::Warning, FindingType::SlowSql),
        finding_with(Severity::Warning, FindingType::RedundantSql),
        finding_with(Severity::Warning, FindingType::SlowHttp),
    ];
    assert!(
        format_top_finding_types(&findings).is_none(),
        "5 findings must not surface a `Most common` line"
    );
}

#[test]
fn top_finding_types_takes_two_when_total_between_six_and_ten() {
    let mut findings = Vec::new();
    for _ in 0..3 {
        findings.push(finding_with(Severity::Warning, FindingType::NPlusOneSql));
    }
    for _ in 0..2 {
        findings.push(finding_with(Severity::Warning, FindingType::RedundantSql));
    }
    for _ in 0..2 {
        findings.push(finding_with(Severity::Warning, FindingType::SlowHttp));
    }
    let line = format_top_finding_types(&findings).expect("7 findings must surface a line");
    assert!(line.contains("Most common:"), "got: {line}");
    assert!(line.contains("n_plus_one_sql (3)"), "got: {line}");
    assert_eq!(
        line.matches('(').count(),
        2,
        "expected 2 entries, got: {line}"
    );
}

#[test]
fn top_finding_types_takes_three_when_total_over_ten() {
    let mut findings = Vec::new();
    for _ in 0..5 {
        findings.push(finding_with(Severity::Warning, FindingType::NPlusOneSql));
    }
    for _ in 0..4 {
        findings.push(finding_with(Severity::Warning, FindingType::RedundantSql));
    }
    for _ in 0..3 {
        findings.push(finding_with(Severity::Warning, FindingType::SlowHttp));
    }
    for _ in 0..1 {
        findings.push(finding_with(Severity::Warning, FindingType::SlowSql));
    }
    let line = format_top_finding_types(&findings).expect("13 findings must surface a line");
    assert!(line.contains("n_plus_one_sql (5)"), "got: {line}");
    assert!(line.contains("redundant_sql (4)"), "got: {line}");
    assert!(line.contains("slow_http (3)"), "got: {line}");
    assert!(
        !line.contains("slow_sql (1)"),
        "should cap at top 3, got: {line}"
    );
}

#[test]
fn top_finding_types_does_not_panic_on_thousand_entries() {
    let findings: Vec<Finding> = (0..1000)
        .map(|i| {
            let kind = match i % 4 {
                0 => FindingType::NPlusOneSql,
                1 => FindingType::RedundantSql,
                2 => FindingType::SlowHttp,
                _ => FindingType::SlowSql,
            };
            finding_with(Severity::Warning, kind)
        })
        .collect();
    let line = format_top_finding_types(&findings).expect("1000 findings must surface a line");
    assert!(line.contains("Most common:"), "got: {line}");
}

#[test]
fn intensity_source_label_covers_every_variant() {
    assert_eq!(intensity_source_label(IntensitySource::Annual), "annual");
    assert_eq!(intensity_source_label(IntensitySource::Hourly), "hourly");
    assert_eq!(
        intensity_source_label(IntensitySource::MonthlyHourly),
        "monthly_hourly"
    );
    assert_eq!(
        intensity_source_label(IntensitySource::RealTime),
        "real_time"
    );
}

/// The coefficient of variation is a percentage of a value stored
/// scaled by 1000, the same reading the dashboard makes. Getting that
/// scale wrong turns 52.3% into 523%, so the test pins the unit
/// conversion, alongside the terminal duration scale.
#[test]
fn span_timing_line_uses_the_terminal_scale_and_the_dashboard_cv() {
    let mut pattern = Pattern {
        template: String::new(),
        occurrences: 3,
        occurrences_by_service: std::collections::BTreeMap::new(),
        window_ms: 10,
        distinct_params: 3,
        span_duration_us_p50: Some(800),
        span_duration_us_p99: Some(1_500),
        span_duration_cv_x1000: Some(523),
    };
    assert_eq!(
        format_span_timing(&pattern).as_deref(),
        Some("p50 800 µs · p99 1.5 ms · CV 52.3%")
    );

    // Seconds once past a million microseconds.
    pattern.span_duration_us_p99 = Some(2_500_000);
    assert!(format_span_timing(&pattern).unwrap().contains("p99 2.50 s"));

    // A detector that populates none of the three yields no line at
    // all rather than an empty label.
    pattern.span_duration_us_p50 = None;
    pattern.span_duration_us_p99 = None;
    pattern.span_duration_cv_x1000 = None;
    assert_eq!(format_span_timing(&pattern), None);
}

#[test]
fn classification_label_only_applies_to_the_n_plus_one_family() {
    use sentinel_core::detect::ClassificationMethod;
    let mut finding = finding_with(Severity::Warning, FindingType::NPlusOneSql);
    assert_eq!(classification_label(&finding), Some("direct"));
    finding.classification_method = Some(ClassificationMethod::SanitizerHeuristic);
    assert_eq!(classification_label(&finding), Some("sanitizer heuristic"));

    // Outside the family the question does not arise, so nothing is
    // printed rather than a misleading "direct".
    let other = finding_with(Severity::Critical, FindingType::SlowSql);
    assert_eq!(classification_label(&other), None);
}

#[test]
fn format_estimation_suffix_covers_every_combination() {
    assert_eq!(
        format_estimation_suffix(Some(true), Some("TIME_SLICER_AVERAGE")).as_ref(),
        ", estimated/TIME_SLICER_AVERAGE"
    );
    assert_eq!(
        format_estimation_suffix(Some(true), None).as_ref(),
        ", estimated"
    );
    assert_eq!(
        format_estimation_suffix(Some(false), Some("ignored")).as_ref(),
        ", measured"
    );
    assert_eq!(
        format_estimation_suffix(Some(false), None).as_ref(),
        ", measured"
    );
    assert_eq!(format_estimation_suffix(None, None).as_ref(), "");
    assert_eq!(format_estimation_suffix(None, Some("ignored")).as_ref(), "");

    // The three constant arms must stay borrowed (no allocation).
    assert_matches!(format_estimation_suffix(Some(true), None), Cow::Borrowed(_));
    assert_matches!(
        format_estimation_suffix(Some(false), None),
        Cow::Borrowed(_)
    );
    assert_matches!(format_estimation_suffix(None, None), Cow::Borrowed(_));
}

#[test]
fn estimation_suffix_strips_terminal_escapes_when_method_is_hostile() {
    // Defense-in-depth: estimation_method from a --input JSON
    // bypasses the API-side sanitizer, so the terminal sink must
    // still strip control bytes. Every other user-controlled string in
    // print_green_summary already goes through sanitize_for_terminal.
    let hostile = "ATTACK\x1b[2J\x1b[H";
    let raw = format_estimation_suffix(Some(true), Some(hostile));
    let cleaned = sanitize_for_terminal(&raw);
    assert!(
        !cleaned.bytes().any(|b| b < 0x20 || b == 0x7f),
        "sanitized suffix must contain no control bytes, got: {cleaned:?}"
    );
    assert!(cleaned.contains("estimated/ATTACK"));
}

#[test]
fn empty_diff_keeps_no_differences_message() {
    let out = render_text(&empty_diff());
    assert!(
        out.contains("No differences detected between the two trace sets."),
        "no-diff message missing, got:\n{out}"
    );
}

/// Visual snapshot helper for the 0.5.10 terminal estimation suffix.
/// Loads the 3-state Region fixture and prints `print_green_summary`
/// to stdout. Run with
/// `cargo test --release validation_terminal_for_three_estimation_states -- --ignored --nocapture`
/// and paste the output into the release snapshot.
#[test]
#[ignore = "manual visual snapshot helper, not run in CI"]
fn validation_terminal_for_three_estimation_states() {
    let fixture_path = format!(
        "{}/../../tests/fixtures/report_three_estimation_states.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&fixture_path).expect("fixture readable");
    let report: Report = serde_json::from_str(&raw).expect("fixture parses as Report");
    eprintln!("--- print_green_summary on 3-state fixture ---");
    print_green_summary(
        &report.green_summary,
        report.analysis.traces_analyzed,
        false,
    );
    eprintln!("--- end ---");
}

#[test]
fn format_scoring_config_line_borrows_for_default_v4() {
    use sentinel_core::score::carbon::ScoringConfig;
    let line = format_scoring_config_line(&ScoringConfig::default());
    assert_eq!(
        line.as_ref(),
        "  Carbon scoring: Electricity Maps v4, lifecycle, hourly"
    );
    assert_matches!(line, Cow::Borrowed(_));
}

#[test]
fn format_scoring_config_line_borrows_for_legacy_v3_defaults() {
    use sentinel_core::score::carbon::ScoringConfig;
    use sentinel_core::score::electricity_maps::config::ApiVersion;
    let cfg = ScoringConfig {
        api_version: ApiVersion::V3,
        ..ScoringConfig::default()
    };
    let line = format_scoring_config_line(&cfg);
    assert_eq!(
        line.as_ref(),
        "  Carbon scoring: Electricity Maps v3, lifecycle, hourly"
    );
    assert_matches!(line, Cow::Borrowed(_));
}

#[test]
fn format_scoring_config_line_owns_for_optins() {
    use sentinel_core::score::carbon::ScoringConfig;
    use sentinel_core::score::electricity_maps::config::{
        ApiVersion, EmissionFactorType, TemporalGranularity,
    };
    let cfg = ScoringConfig {
        api_version: ApiVersion::V4,
        emission_factor_type: EmissionFactorType::Direct,
        temporal_granularity: TemporalGranularity::FiveMinutes,
        ..ScoringConfig::default()
    };
    let line = format_scoring_config_line(&cfg);
    assert_eq!(
        line.as_ref(),
        "  Carbon scoring: Electricity Maps v4, direct, 5_minutes"
    );
    assert_matches!(line, Cow::Owned(_));
}

#[test]
fn format_scoring_config_line_owns_for_custom_endpoint() {
    use sentinel_core::score::carbon::ScoringConfig;
    use sentinel_core::score::electricity_maps::config::ApiVersion;
    let cfg = ScoringConfig {
        api_version: ApiVersion::Custom,
        ..ScoringConfig::default()
    };
    let line = format_scoring_config_line(&cfg);
    assert_eq!(
        line.as_ref(),
        "  Carbon scoring: Electricity Maps custom, lifecycle, hourly"
    );
    assert_matches!(line, Cow::Owned(_));
}

/// Visual snapshot helper for the 0.5.12 terminal scoring config
/// header. Builds 3 in-memory `GreenSummary`s with different
/// `scoring_config` shapes and prints each through
/// `print_green_summary`. Run with
/// `cargo test --release validation_terminal_for_scoring_config -- --ignored --nocapture`
/// and paste the output into the release snapshot.
#[test]
#[ignore = "manual visual snapshot helper, not run in CI"]
fn validation_terminal_for_scoring_config() {
    use sentinel_core::report::GreenSummary;
    use sentinel_core::score::carbon::ScoringConfig;
    use sentinel_core::score::electricity_maps::config::{
        ApiVersion, EmissionFactorType, TemporalGranularity,
    };
    let cases = [
        ("V4 defaults", ScoringConfig::default()),
        (
            "V3 legacy",
            ScoringConfig {
                api_version: ApiVersion::V3,
                ..ScoringConfig::default()
            },
        ),
        (
            "All opt-ins",
            ScoringConfig {
                api_version: ApiVersion::V4,
                emission_factor_type: EmissionFactorType::Direct,
                temporal_granularity: TemporalGranularity::FifteenMinutes,
                ..ScoringConfig::default()
            },
        ),
    ];
    for (label, cfg) in cases {
        eprintln!("=== {label} ===");
        let mut summary = GreenSummary::disabled(0);
        summary.scoring_config = Some(cfg);
        print_green_summary(&summary, 1, false);
    }
}

#[test]
fn broker_waste_line_sanitizes_and_labels_its_source() {
    use sentinel_core::report::MessagingWaste;
    // A terminal escape in the daemon-sourced region must not reach
    // the sink, same bar as the database line.
    let declared = MessagingWaste {
        energy_kwh: 0.5,
        waste_kwh: 0.2,
        waste_gco2: Some(3e-7),
        energy_gco2: Some(7e-7),
        region: Some("eu\u{1b}[2Jwest".to_string()),
        messaging_waste_ratio: 0.4,
        model: "broker_specpower".to_string(),
    };
    let line = format_messaging_waste_line(&declared);
    assert!(!line.contains('\u{1b}'), "escape reached the sink: {line}");
    assert!(line.contains("Broker waste"), "{line}");
    assert!(line.contains("40% messaging ratio"), "{line}");
    assert!(line.contains("model broker_specpower"), "{line}");
    assert!(line.contains("excluded from totals"), "{line}");

    let estimated = MessagingWaste {
        model: sentinel_core::report::DB_WASTE_MODEL_ESTIMATED.to_string(),
        region: None,
        waste_gco2: None,
        ..declared
    };
    let line = format_messaging_waste_line(&estimated);
    assert!(line.contains("within the report totals"), "{line}");
    assert!(
        !line.contains("region"),
        "absent region must drop out: {line}"
    );
}

#[test]
fn green_summary_prints_sql_share_and_database_waste() {
    use sentinel_core::report::{DatabaseWaste, GreenSummary};
    // Measured path: sanitized region, measured label, scientific gCO2.
    let measured = DatabaseWaste {
        energy_kwh: 0.2,
        waste_kwh: 0.16,
        waste_gco2: Some(4e-7),
        energy_gco2: Some(5e-7),
        region: Some("eu\u{1b}[2Jwest".to_string()),
        sql_waste_ratio: 5.0 / 6.0,
        model: "alumet_rapl".to_string(),
    };
    let line = format_database_waste_line(&measured);
    assert!(!line.contains('\u{1b}'), "escape reached the sink: {line}");
    assert!(line.contains("model alumet_rapl"), "{line}");
    assert!(line.contains("excluded from totals"), "{line}");
    assert!(line.contains("4.000e-7 gCO"), "{line}");
    // Estimated path: within-totals label, optional suffixes drop out.
    let estimated = DatabaseWaste {
        energy_kwh: 5.5e-7,
        waste_kwh: 0.0,
        waste_gco2: None,
        energy_gco2: None,
        region: None,
        sql_waste_ratio: 0.0,
        model: "estimated".to_string(),
    };
    let line = format_database_waste_line(&estimated);
    assert!(line.contains("within the report totals"), "{line}");
    assert!(line.contains("5.500e-7 kWh"), "{line}");
    assert!(!line.contains("region"), "{line}");
    // Empty model (a legacy baseline) renders the dash placeholder and
    // the non-estimated "excluded" scope.
    let legacy = DatabaseWaste {
        model: String::new(),
        ..estimated.clone()
    };
    let line = format_database_waste_line(&legacy);
    assert!(line.contains("model -"), "{line}");
    assert!(line.contains("excluded from totals"), "{line}");
    // Smoke the full print path with the SQL share visible.
    let mut summary = GreenSummary::disabled(10);
    summary.total_sql_io_ops = 6;
    summary.avoidable_sql_io_ops = 5;
    summary.database_waste = Some(estimated);
    print_green_summary(&summary, 1, false);
}

#[test]
fn energy_line_names_the_energy_source() {
    use sentinel_core::report::GreenSummary;
    let mut gs = GreenSummary::disabled(10);
    assert_eq!(
        format_energy_line(&gs, "<d>", "</d>"),
        "  <d>Energy:            not computed (no span resolved to a region)</d>"
    );
    gs.energy_kwh = 0.5;
    gs.energy_model = "electricity_maps_api".to_string();
    gs.per_service_energy_model
        .insert("a".to_string(), "electricity_maps_api".to_string());
    gs.per_service_measured_ratio.insert("a".to_string(), 0.0);
    assert_eq!(
        format_energy_line(&gs, "<d>", "</d>"),
        "  Energy:            0.500000 kWh (modeled from I/O counts)"
    );
}

#[test]
fn duration_format_covers_all_branches() {
    assert_eq!(format_duration_compact(0), "0ms");
    assert_eq!(format_duration_compact(750), "750ms");
    assert_eq!(format_duration_compact(1_000), "1s");
    assert_eq!(format_duration_compact(59_000), "59s");
    assert_eq!(format_duration_compact(60_000), "1m");
    assert_eq!(format_duration_compact(125_000), "2m5s");
    assert_eq!(format_duration_compact(3_600_000), "1h");
    assert_eq!(format_duration_compact(3_660_000), "1h1m");
}

#[test]
fn embedded_zone_only_stands_in_for_a_name_the_system_lacks() {
    let none = |_: &str| false;
    let tokyo = embedded_zone(Some("Asia/Tokyo"), none).expect("embedded Asia/Tokyo");
    let t = chrono::DateTime::parse_from_rfc3339("2025-07-10T14:32:01Z").unwrap();
    assert_eq!(
        t.with_timezone(&tokyo)
            .format(LOCAL_TIME_FORMAT)
            .to_string(),
        "2025-07-10 23:32:01"
    );
    assert!(embedded_zone(Some(":Europe/Paris"), none).is_some());
    // Left to chrono::Local: unset, the system's own copy, a path, a POSIX rule.
    assert!(embedded_zone(None, none).is_none());
    assert!(embedded_zone(Some("Asia/Tokyo"), |_| true).is_none());
    assert!(embedded_zone(Some("/etc/localtime"), none).is_none());
    assert!(embedded_zone(Some("JST-9"), none).is_none());
    assert!(embedded_zone(Some(""), none).is_none());
}

#[test]
fn local_iso_converts_to_local_time_and_falls_back_to_the_input() {
    // The zone is the machine's, so the expectation goes through to_local too.
    let expected =
        to_local(&chrono::DateTime::parse_from_rfc3339("2026-04-20T23:30:01.000Z").unwrap())
            .format(LOCAL_TIME_FORMAT)
            .to_string();
    assert_eq!(
        fmt_local_iso("2026-04-20T23:30:01.000Z", LOCAL_TIME_FORMAT),
        expected
    );
    assert_eq!(
        fmt_local_iso("2026-04-20 23:30:01.000Z", LOCAL_TIME_FORMAT),
        expected
    );
    assert_eq!(fmt_local_iso("short", LOCAL_TIME_FORMAT), "short");
    assert_eq!(fmt_local_iso("bad\x1b[31m", LOCAL_TIME_FORMAT), "bad?[31m");
}

fn make_report(
    findings: Vec<Finding>,
    top_offenders: Vec<TopOffender>,
    gate_passed: bool,
    rules: Vec<QualityRule>,
) -> Report {
    let event_count = if findings.is_empty() { 4 } else { 10 };
    // `Analysis` is `#[non_exhaustive]`, so a sibling crate fills it
    // field by field rather than with a struct literal.
    let mut analysis = Analysis::default();
    analysis.duration_ms = 1;
    analysis.events_processed = event_count;
    analysis.traces_analyzed = 1;
    Report {
        analysis,
        findings,
        green_summary: GreenSummary {
            total_io_ops: event_count,
            top_offenders,
            ..GreenSummary::disabled(0)
        },
        quality_gate: QualityGate {
            passed: gate_passed,
            rules,
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

fn make_finding(finding_type: FindingType, severity: Severity) -> Finding {
    Finding {
        finding_type,
        severity,
        trace_id: "trace-1".to_string(),
        service: "order-svc".to_string(),
        grouping: Vec::new(),
        source_endpoint: "POST /api/orders/42/submit".to_string(),
        pattern: Pattern {
            template: "SELECT * FROM t WHERE id = ?".to_string(),
            occurrences: 6,
            window_ms: 200,
            distinct_params: 6,
            ..Default::default()
        },
        suggestion: "batch".to_string(),
        first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
        last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
        green_impact: Some(GreenImpact {
            estimated_extra_io_ops: 5,
            io_intensity_score: 6.0,
            io_intensity_band: sentinel_core::InterpretationLevel::for_iis(6.0),
        }),
        confidence: Confidence::default(),
        classification_method: None,
        code_location: None,
        instrumentation_scopes: Vec::new(),
        suggested_fix: None,
        signature: String::new(),
    }
}

#[test]
fn report_no_findings() {
    let report = make_report(vec![], vec![], true, vec![]);
    // Should not panic and should print "No performance anti-patterns detected."
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_critical_severity() {
    let report = make_report(
        vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)],
        vec![],
        true,
        vec![],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_info_severity() {
    let report = make_report(
        vec![make_finding(FindingType::RedundantSql, Severity::Info)],
        vec![],
        true,
        vec![],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_redundant_http_type() {
    let report = make_report(
        vec![make_finding(FindingType::RedundantHttp, Severity::Warning)],
        vec![],
        true,
        vec![],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_slow_sql_type() {
    let report = make_report(
        vec![make_finding(FindingType::SlowSql, Severity::Warning)],
        vec![],
        true,
        vec![],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_slow_http_type() {
    let report = make_report(
        vec![make_finding(FindingType::SlowHttp, Severity::Critical)],
        vec![],
        true,
        vec![],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_quality_gate_failed() {
    let report = make_report(
        vec![make_finding(FindingType::NPlusOneSql, Severity::Critical)],
        vec![],
        false,
        vec![QualityRule {
            rule: "n_plus_one_sql_critical_max".to_string(),
            threshold: 0.0,
            actual: 1.0,
            passed: false,
        }],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_with_top_offenders() {
    let report = make_report(
        vec![make_finding(FindingType::NPlusOneSql, Severity::Warning)],
        vec![TopOffender {
            endpoint: "POST /api/orders/{id}/submit".to_string(),
            service: "order-svc".to_string(),
            io_intensity_score: 8.2,
            io_intensity_band: sentinel_core::InterpretationLevel::for_iis(8.2),
            co2_grams: None,
        }],
        true,
        vec![],
    );
    render::format_colored_report(&report, "report", false);
}

#[test]
fn report_with_ansi_colors() {
    // Test the TTY=true branch (force_color=true)
    let report = make_report(
        vec![
            make_finding(FindingType::NPlusOneSql, Severity::Critical),
            make_finding(FindingType::NPlusOneHttp, Severity::Warning),
            make_finding(FindingType::RedundantSql, Severity::Info),
            make_finding(FindingType::RedundantHttp, Severity::Info),
        ],
        vec![TopOffender {
            endpoint: "POST /api/orders/{id}/submit".to_string(),
            service: "order-svc".to_string(),
            io_intensity_score: 8.2,
            io_intensity_band: sentinel_core::InterpretationLevel::for_iis(8.2),
            co2_grams: None,
        }],
        false,
        vec![],
    );
    render::format_colored_report(&report, "report", true);
}

#[test]
fn report_with_co2_data() {
    let mut analysis = Analysis::default();
    analysis.duration_ms = 1;
    analysis.events_processed = 10;
    analysis.traces_analyzed = 1;
    let report = Report {
        analysis,
        findings: vec![],
        green_summary: GreenSummary {
            total_io_ops: 10,
            avoidable_io_ops: 5,
            io_waste_ratio: 0.5,
            io_waste_ratio_band: sentinel_core::InterpretationLevel::for_waste_ratio(0.5),
            top_offenders: vec![TopOffender {
                endpoint: "POST /api/orders/{id}/submit".to_string(),
                service: "order-svc".to_string(),
                io_intensity_score: 8.2,
                io_intensity_band: sentinel_core::InterpretationLevel::for_iis(8.2),
                co2_grams: Some(0.001),
            }],
            ..GreenSummary::disabled(0)
        },
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
    };
    render::format_colored_report(&report, "report", false);
}
