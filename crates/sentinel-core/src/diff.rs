//! Diff stage: compare two analysis reports and produce a delta.
//!
//! Primary use case: PR CI integration. Run `analyze` on the base
//! branch's traces, run it again on the PR branch's traces, then compare
//! the two reports to surface regressions (new findings, severity
//! escalations, per-endpoint I/O op increases) and improvements
//! (resolved findings, severity de-escalations, per-endpoint I/O op
//! decreases).
//!
//! Finding identity for stable comparison is the tuple
//! `(finding_type, service, source_endpoint, pattern.template)`. The
//! template is already normalized at detection time so direct equality
//! suffices, no re-normalization at diff time.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::detect::{Finding, FindingType, Severity};
use crate::report::{PerEndpointIoOps, Report};

/// Stable identity tuple for matching findings between two runs.
///
/// Two findings with the same `IdentityKey` are considered "the same
/// anti-pattern" across runs. If multiple findings in one run share a
/// key (e.g. the same N+1 template fired in two traces), the diff
/// engine collapses them to one entry by keeping the worst-severity
/// finding for that key.
type IdentityKey = (FindingType, String, String, String);

fn identity_of(finding: &Finding) -> IdentityKey {
    (
        finding.finding_type.clone(),
        finding.service.clone(),
        finding.source_endpoint.clone(),
        finding.pattern.template.clone(),
    )
}

/// Delta between two analysis runs.
///
/// Stable JSON shape. Field names will not be renamed or removed in a
/// minor release; new optional fields may be added.
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    /// Findings present in `after` but absent from `before`.
    pub new_findings: Vec<Finding>,
    /// Findings present in `before` but absent from `after`.
    pub resolved_findings: Vec<Finding>,
    /// Findings present in both runs whose worst severity differs.
    /// Ordered with regressions (worse-after) first, then improvements.
    pub severity_changes: Vec<SeverityChange>,
    /// Per-endpoint I/O op deltas. Ordered with the largest regressions
    /// first (most positive delta), then improvements last.
    pub endpoint_metric_deltas: Vec<EndpointDelta>,
}

/// A finding whose worst severity changed between the two runs.
#[derive(Debug, Clone, Serialize)]
pub struct SeverityChange {
    /// The "after" version of the finding (same identity, latest data).
    pub finding: Finding,
    pub before_severity: Severity,
    pub after_severity: Severity,
}

impl SeverityChange {
    /// `true` when the after severity is worse than the before severity.
    /// Used to sort regressions ahead of improvements in the output and
    /// reused by the CLI text renderer to color regressions red.
    ///
    /// Severity derives `Ord` with declaration order (Critical < Warning
    /// < Info). "Worse" means numerically lower.
    #[must_use]
    pub fn is_regression(&self) -> bool {
        self.after_severity < self.before_severity
    }
}

/// Per-endpoint I/O op count delta between two runs.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointDelta {
    pub service: String,
    pub endpoint: String,
    pub before_io_ops: usize,
    pub after_io_ops: usize,
    /// `after - before`. Positive = regression, negative = improvement.
    pub delta: i64,
}

/// Compare two analysis reports.
///
/// Both reports are expected to come from `pipeline::analyze` runs on
/// their respective trace sets, with the same `Config` (otherwise the
/// per-endpoint counts and severity assignments may not be comparable).
///
/// Pure function: takes references and returns owned data. No I/O.
#[must_use]
pub fn diff_runs(before: &Report, after: &Report) -> DiffReport {
    let before_map = build_identity_map(&before.findings);
    let after_map = build_identity_map(&after.findings);

    let mut new_findings: Vec<Finding> = Vec::new();
    let mut resolved_findings: Vec<Finding> = Vec::new();
    let mut severity_changes: Vec<SeverityChange> = Vec::new();

    for (key, after_finding) in &after_map {
        match before_map.get(key) {
            None => new_findings.push(after_finding.clone()),
            Some(before_finding) if before_finding.severity != after_finding.severity => {
                severity_changes.push(SeverityChange {
                    finding: after_finding.clone(),
                    before_severity: before_finding.severity.clone(),
                    after_severity: after_finding.severity.clone(),
                });
            }
            Some(_) => {}
        }
    }
    for (key, before_finding) in &before_map {
        if !after_map.contains_key(key) {
            resolved_findings.push(before_finding.clone());
        }
    }

    // Stable, deterministic ordering for the two finding lists. Reuses
    // the same ordering rule as `analyze` so a reader's mental model
    // is identical between the two outputs.
    crate::detect::sort_findings(&mut new_findings);
    crate::detect::sort_findings(&mut resolved_findings);
    // Severity changes: regressions (worse-after) first, then
    // improvements. Within each group, sort by the same finding-order
    // rule as the regular output for predictability.
    severity_changes.sort_by(|a, b| {
        b.is_regression()
            .cmp(&a.is_regression())
            .then_with(|| a.finding.finding_type.cmp(&b.finding.finding_type))
            .then_with(|| a.finding.service.cmp(&b.finding.service))
            .then_with(|| a.finding.source_endpoint.cmp(&b.finding.source_endpoint))
            .then_with(|| a.finding.pattern.template.cmp(&b.finding.pattern.template))
    });

    let endpoint_metric_deltas =
        diff_per_endpoint_io_ops(&before.per_endpoint_io_ops, &after.per_endpoint_io_ops);

    DiffReport {
        new_findings,
        resolved_findings,
        severity_changes,
        endpoint_metric_deltas,
    }
}

/// Build an identity-keyed map of findings, collapsing duplicates by
/// keeping the worst severity AND summing `pattern.occurrences` across
/// all duplicates. Used as the canonical view of each side before
/// computing the diff.
///
/// Returning owned `Finding` values (not borrowed) lets us aggregate
/// occurrences without mutating the input slice. The aggregated
/// occurrences mean the user sees the total amplitude of the pattern
/// across the trace set, not just one trace's count, so a regression
/// from "6 occurrences in trace A" to "60 occurrences in trace A AND 60
/// in trace B" surfaces as a meaningful endpoint-delta plus (when
/// severity escalates) a `severity_change`.
///
/// Tie-break for the kept Finding template: the first finding inserted
/// at a key wins for `trace_id` / `first_timestamp` / `code_location` /
/// `suggested_fix`. Since `pipeline::analyze` calls `sort_findings`
/// before returning, this is deterministic.
fn build_identity_map(findings: &[Finding]) -> BTreeMap<IdentityKey, Finding> {
    let mut map: BTreeMap<IdentityKey, Finding> = BTreeMap::new();
    for finding in findings {
        let key = identity_of(finding);
        match map.get_mut(&key) {
            None => {
                map.insert(key, finding.clone());
            }
            Some(existing) => {
                // Severity derives Ord with declaration order:
                // Critical < Warning < Info. Worst = min.
                if finding.severity < existing.severity {
                    let summed = existing
                        .pattern
                        .occurrences
                        .saturating_add(finding.pattern.occurrences);
                    *existing = finding.clone();
                    existing.pattern.occurrences = summed;
                } else {
                    existing.pattern.occurrences = existing
                        .pattern
                        .occurrences
                        .saturating_add(finding.pattern.occurrences);
                }
            }
        }
    }
    map
}

/// Pair up `before` and `after` per-endpoint I/O op counts and emit one
/// `EndpointDelta` per `(service, endpoint)` whose count differs.
/// Endpoints absent from one side are treated as `0` on that side.
/// Sorted regressions-first, then improvements (largest absolute delta
/// inside each group last for the improvements direction).
fn diff_per_endpoint_io_ops(
    before: &[PerEndpointIoOps],
    after: &[PerEndpointIoOps],
) -> Vec<EndpointDelta> {
    let mut before_map: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for entry in before {
        before_map.insert((&entry.service, &entry.endpoint), entry.io_ops);
    }
    let mut after_map: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for entry in after {
        after_map.insert((&entry.service, &entry.endpoint), entry.io_ops);
    }

    let mut keys: BTreeSet<(&str, &str)> = BTreeSet::new();
    keys.extend(before_map.keys().copied());
    keys.extend(after_map.keys().copied());

    let mut deltas: Vec<EndpointDelta> = keys
        .iter()
        .filter_map(|(service, endpoint)| {
            let before_io = before_map.get(&(*service, *endpoint)).copied().unwrap_or(0);
            let after_io = after_map.get(&(*service, *endpoint)).copied().unwrap_or(0);
            if before_io == after_io {
                return None;
            }
            // Cast through i128 to handle the worst case (`usize::MAX`
            // on either side) without panicking. Any plausible counts
            // fit comfortably in i64; if a future workload pushes past
            // `i64::MAX` we clamp and warn rather than overflow silently.
            let delta = i128::from(after_io as u64) - i128::from(before_io as u64);
            let delta_i64 = i64::try_from(delta).unwrap_or_else(|_| {
                tracing::warn!(
                    target: "perf_sentinel::diff",
                    service = %service,
                    endpoint = %endpoint,
                    before_io = before_io,
                    after_io = after_io,
                    "endpoint I/O op delta overflows i64, clamping for output"
                );
                if delta > 0 { i64::MAX } else { i64::MIN }
            });
            Some(EndpointDelta {
                service: (*service).to_string(),
                endpoint: (*endpoint).to_string(),
                before_io_ops: before_io,
                after_io_ops: after_io,
                delta: delta_i64,
            })
        })
        .collect();

    // Regressions first: positive deltas before negative ones, then by
    // descending magnitude inside each group. Tie-break on `(service,
    // endpoint)` so the ordering is stable across runs even if
    // `sort_by`'s internal stability guarantee changes.
    deltas.sort_by(|a, b| {
        b.delta
            .cmp(&a.delta)
            .then_with(|| a.service.cmp(&b.service))
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });
    deltas
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Confidence, Finding, FindingType, Pattern, Severity};
    use crate::report::{Analysis, GreenSummary, PerEndpointIoOps, QualityGate, Report};

    fn make_report(findings: Vec<Finding>, per_endpoint: Vec<PerEndpointIoOps>) -> Report {
        Report {
            analysis: Analysis {
                duration_ms: 0,
                events_processed: 0,
                traces_analyzed: 0,
            },
            findings,
            green_summary: GreenSummary::disabled(0),
            quality_gate: QualityGate {
                passed: true,
                rules: vec![],
            },
            per_endpoint_io_ops: per_endpoint,
            correlations: vec![],
            warnings: vec![],
            acknowledged_findings: vec![],
        }
    }

    fn finding(
        ft: FindingType,
        sev: Severity,
        service: &str,
        endpoint: &str,
        template: &str,
    ) -> Finding {
        Finding {
            finding_type: ft,
            severity: sev,
            trace_id: "trace-1".to_string(),
            service: service.to_string(),
            source_endpoint: endpoint.to_string(),
            pattern: Pattern {
                template: template.to_string(),
                occurrences: 6,
                window_ms: 200,
                distinct_params: 6,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.250Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: String::new(),
        }
    }

    fn endpoint(service: &str, ep: &str, ops: usize) -> PerEndpointIoOps {
        PerEndpointIoOps {
            service: service.to_string(),
            endpoint: ep.to_string(),
            io_ops: ops,
        }
    }

    #[test]
    fn identical_runs_produce_empty_diff() {
        let f = finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "svc",
            "POST /api",
            "SELECT *",
        );
        let before = make_report(vec![f.clone()], vec![endpoint("svc", "POST /api", 6)]);
        let after = make_report(vec![f], vec![endpoint("svc", "POST /api", 6)]);
        let diff = diff_runs(&before, &after);
        assert!(diff.new_findings.is_empty());
        assert!(diff.resolved_findings.is_empty());
        assert!(diff.severity_changes.is_empty());
        assert!(diff.endpoint_metric_deltas.is_empty());
    }

    #[test]
    fn finding_present_only_in_after_is_new() {
        let before = make_report(vec![], vec![]);
        let after = make_report(
            vec![finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "svc",
                "POST /api",
                "SELECT *",
            )],
            vec![],
        );
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.new_findings.len(), 1);
        assert!(diff.resolved_findings.is_empty());
        assert!(diff.severity_changes.is_empty());
    }

    #[test]
    fn finding_present_only_in_before_is_resolved() {
        let before = make_report(
            vec![finding(
                FindingType::NPlusOneSql,
                Severity::Warning,
                "svc",
                "POST /api",
                "SELECT *",
            )],
            vec![],
        );
        let after = make_report(vec![], vec![]);
        let diff = diff_runs(&before, &after);
        assert!(diff.new_findings.is_empty());
        assert_eq!(diff.resolved_findings.len(), 1);
        assert!(diff.severity_changes.is_empty());
    }

    #[test]
    fn same_identity_with_different_severity_is_severity_change() {
        let f_warn = finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "svc",
            "POST /api",
            "SELECT *",
        );
        let mut f_crit = f_warn.clone();
        f_crit.severity = Severity::Critical;
        let before = make_report(vec![f_warn], vec![]);
        let after = make_report(vec![f_crit], vec![]);
        let diff = diff_runs(&before, &after);
        assert!(diff.new_findings.is_empty());
        assert!(diff.resolved_findings.is_empty());
        assert_eq!(diff.severity_changes.len(), 1);
        let change = &diff.severity_changes[0];
        assert_eq!(change.before_severity, Severity::Warning);
        assert_eq!(change.after_severity, Severity::Critical);
        assert!(
            change.is_regression(),
            "warning -> critical is a regression"
        );
    }

    #[test]
    fn severity_changes_sorted_regressions_first() {
        // After: one regression (warning -> critical), one improvement (critical -> warning).
        let before = make_report(
            vec![
                finding(
                    FindingType::NPlusOneSql,
                    Severity::Warning,
                    "svc-a",
                    "POST /a",
                    "SELECT a",
                ),
                finding(
                    FindingType::NPlusOneSql,
                    Severity::Critical,
                    "svc-b",
                    "POST /b",
                    "SELECT b",
                ),
            ],
            vec![],
        );
        let after = make_report(
            vec![
                finding(
                    FindingType::NPlusOneSql,
                    Severity::Critical,
                    "svc-a",
                    "POST /a",
                    "SELECT a",
                ),
                finding(
                    FindingType::NPlusOneSql,
                    Severity::Warning,
                    "svc-b",
                    "POST /b",
                    "SELECT b",
                ),
            ],
            vec![],
        );
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.severity_changes.len(), 2);
        assert!(
            diff.severity_changes[0].is_regression(),
            "regression must come first"
        );
        assert!(
            !diff.severity_changes[1].is_regression(),
            "improvement must come last"
        );
    }

    #[test]
    fn duplicate_identity_in_one_run_is_collapsed_to_worst_severity() {
        // Two findings with the same identity tuple in `after`, one at
        // Warning and one at Critical. They should collapse to a single
        // "after" finding at Critical, and the diff should not interpret
        // the count difference as a severity change.
        let before = make_report(
            vec![finding(
                FindingType::NPlusOneSql,
                Severity::Critical,
                "svc",
                "POST /api",
                "SELECT *",
            )],
            vec![],
        );
        let f_warn = finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "svc",
            "POST /api",
            "SELECT *",
        );
        let mut f_crit = f_warn.clone();
        f_crit.severity = Severity::Critical;
        let after = make_report(vec![f_warn, f_crit], vec![]);
        let diff = diff_runs(&before, &after);
        assert!(
            diff.new_findings.is_empty(),
            "no new findings when identity is shared"
        );
        assert!(
            diff.resolved_findings.is_empty(),
            "no resolved when identity is shared"
        );
        assert!(
            diff.severity_changes.is_empty(),
            "worst-severity dedupe should make this a no-op (Critical == Critical)"
        );
    }

    #[test]
    fn endpoint_io_ops_increase_is_a_positive_delta() {
        let before = make_report(vec![], vec![endpoint("svc", "POST /api/users", 10)]);
        let after = make_report(vec![], vec![endpoint("svc", "POST /api/users", 20)]);
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.endpoint_metric_deltas.len(), 1);
        let d = &diff.endpoint_metric_deltas[0];
        assert_eq!(d.service, "svc");
        assert_eq!(d.endpoint, "POST /api/users");
        assert_eq!(d.before_io_ops, 10);
        assert_eq!(d.after_io_ops, 20);
        assert_eq!(d.delta, 10);
    }

    #[test]
    fn endpoint_absent_from_before_is_a_full_addition() {
        let before = make_report(vec![], vec![]);
        let after = make_report(vec![], vec![endpoint("svc", "POST /api", 7)]);
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.endpoint_metric_deltas.len(), 1);
        let d = &diff.endpoint_metric_deltas[0];
        assert_eq!(d.before_io_ops, 0);
        assert_eq!(d.after_io_ops, 7);
        assert_eq!(d.delta, 7);
    }

    #[test]
    fn endpoint_absent_from_after_is_a_full_removal() {
        let before = make_report(vec![], vec![endpoint("svc", "POST /api", 5)]);
        let after = make_report(vec![], vec![]);
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.endpoint_metric_deltas.len(), 1);
        let d = &diff.endpoint_metric_deltas[0];
        assert_eq!(d.before_io_ops, 5);
        assert_eq!(d.after_io_ops, 0);
        assert_eq!(d.delta, -5);
    }

    #[test]
    fn endpoint_deltas_sorted_regressions_first() {
        let before = make_report(
            vec![],
            vec![
                endpoint("svc", "POST /improve", 10),
                endpoint("svc", "POST /regress", 5),
                endpoint("svc", "POST /steady", 7),
            ],
        );
        let after = make_report(
            vec![],
            vec![
                endpoint("svc", "POST /improve", 2),
                endpoint("svc", "POST /regress", 50),
                endpoint("svc", "POST /steady", 7),
            ],
        );
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.endpoint_metric_deltas.len(), 2);
        assert_eq!(diff.endpoint_metric_deltas[0].endpoint, "POST /regress");
        assert_eq!(diff.endpoint_metric_deltas[0].delta, 45);
        assert_eq!(diff.endpoint_metric_deltas[1].endpoint, "POST /improve");
        assert_eq!(diff.endpoint_metric_deltas[1].delta, -8);
    }

    #[test]
    fn equal_severity_in_both_runs_is_not_a_severity_change() {
        // Guard against a future refactor that compares with `<` or
        // `<=` instead of `!=` and silently classifies equal-severity
        // findings as severity changes.
        let f = finding(
            FindingType::NPlusOneSql,
            Severity::Critical,
            "svc",
            "POST /api",
            "SELECT *",
        );
        let before = make_report(vec![f.clone()], vec![]);
        let after = make_report(vec![f], vec![]);
        let diff = diff_runs(&before, &after);
        assert!(diff.severity_changes.is_empty());
    }

    #[test]
    fn same_identity_different_trace_id_is_treated_as_one_finding() {
        // Two findings with identical (type, service, endpoint, template)
        // but different trace_ids are conceptually "the same anti-pattern"
        // observed twice. The diff collapses them and sums occurrences.
        let mut f_a = finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "svc",
            "POST /api",
            "SELECT *",
        );
        f_a.trace_id = "trace-a".to_string();
        f_a.pattern.occurrences = 6;
        let mut f_b = f_a.clone();
        f_b.trace_id = "trace-b".to_string();
        f_b.pattern.occurrences = 12;

        let before = make_report(vec![], vec![]);
        let after = make_report(vec![f_a, f_b], vec![]);
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.new_findings.len(), 1, "two duplicates collapse to one");
        assert_eq!(
            diff.new_findings[0].pattern.occurrences, 18,
            "occurrences from both findings sum on collapse"
        );
    }

    #[test]
    fn duplicate_identity_collapse_sums_occurrences() {
        // Direct test of `build_identity_map` summing semantic.
        let mut f_a = finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "svc",
            "POST /api",
            "SELECT *",
        );
        f_a.pattern.occurrences = 6;
        let mut f_b = f_a.clone();
        f_b.pattern.occurrences = 60;

        let before = make_report(vec![], vec![]);
        let after = make_report(vec![f_a, f_b], vec![]);
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.new_findings.len(), 1);
        assert_eq!(diff.new_findings[0].pattern.occurrences, 66);
    }

    #[test]
    fn diff_sarif_emits_one_result_per_new_finding() {
        // Smoke test for the public `findings_to_sarif` re-use path
        // exercised by `perf-sentinel diff --format sarif`.
        let f = finding(
            FindingType::NPlusOneSql,
            Severity::Warning,
            "svc",
            "POST /api",
            "SELECT *",
        );
        let before = make_report(vec![], vec![]);
        let after = make_report(vec![f], vec![]);
        let diff = diff_runs(&before, &after);
        assert_eq!(diff.new_findings.len(), 1);
        let sarif = crate::report::sarif::findings_to_sarif(&diff.new_findings);
        assert_eq!(
            sarif.runs[0].results.len(),
            diff.new_findings.len(),
            "SARIF results count must match new_findings count"
        );
    }
}
