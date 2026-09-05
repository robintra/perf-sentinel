//! Ring-buffer store for recent findings, queryable by the daemon API.
//!
//! Detection is per trace, so a recurring pattern re-emits an identical
//! finding for every trace that exhibits it. The buffer keeps those
//! instances: they carry the per-trace severity, they let
//! `by_trace_id` answer for a trace whose spans have aged out, and FIFO
//! pressure is what expires a fixed problem. Listing them raw is what
//! reads as duplicate rows, so [`coalesce_by_signature`] folds them at
//! READ time by effective grouping identity and signature, leaving the stored
//! history intact.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use tokio::sync::RwLock;

use crate::detect::Finding;

type FoldKey<'a> = (Option<(&'a str, &'a str)>, &'a str);

/// A finding with daemon-side metadata.
///
/// A raw buffer entry describes one detection: `seen_count` is 1 and
/// `first_seen_ms` equals `stored_at_ms`. After
/// [`coalesce_by_signature`] the entry stands for every folded
/// detection of that signature, see that function for which fields it
/// merges.
/// `#[non_exhaustive]` so a future field stays a minor bump rather than
/// a breaking change: external crates cannot construct it with a
/// struct literal, only read it or deserialize into it.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StoredFinding {
    /// The detected finding. On a coalesced entry, the most recent
    /// instance, carrying the worst severity seen.
    pub finding: Finding,
    /// Monotonic timestamp (ms) of this detection, or of the most
    /// recent one on a coalesced entry.
    pub stored_at_ms: u64,
    /// Monotonic timestamp (ms) of the oldest detection this entry
    /// stands for. `0` on payloads predating the field.
    #[serde(default)]
    pub first_seen_ms: u64,
    /// How many per-trace detections this entry stands for, `1` for a
    /// raw buffer entry. Defaults to 1 on payloads predating the field.
    #[serde(default = "default_seen_count")]
    pub seen_count: u64,
}

fn default_seen_count() -> u64 {
    1
}

/// Fold per-trace detections into one entry per effective grouping and
/// signature, preserving the input order (newest first, what
/// [`FindingsStore::query`] yields).
///
/// Two rows split here share one acknowledgment signature, by design: the
/// signature is grouping-blind (see `acknowledgments::compute_signature`).
/// Grouping answers who is affected and where, acknowledgment answers
/// whether the code is accepted debt, and the second does not vary by
/// deployment: the same N+1 in five tenants is one decision, not five.
/// Environments that genuinely need separate triage run separate daemons
/// with separate stores. Keeping the signature grouping-blind also means
/// reordering `grouping_attributes` never invalidates an existing ack.
///
/// The representative is the WORST-severity detection of the group, not
/// the newest. Severity is derived per trace (12 repeats is critical, 6
/// is a warning), so a row must not claim a severity its own
/// `pattern.occurrences` and `trace_id` contradict: grafting the worst
/// severity onto the newest instance produces a critical row pointing at
/// the quiet trace, which cannot be triaged from what it shows. Ties
/// keep the newest. `seen_count` counts the fold and `first_seen_ms` is
/// the oldest detection retained, both group metadata rather than
/// properties of the representative.
#[must_use]
pub fn coalesce_by_signature(entries: &[StoredFinding]) -> Vec<StoredFinding> {
    fold_entries(entries.iter())
}

/// [`coalesce_by_signature`] over any iterator, so the store can fold
/// straight off the buffer under the read lock and clone once per
/// distinct signature instead of once per retained detection.
fn fold_entries<'a>(entries: impl Iterator<Item = &'a StoredFinding>) -> Vec<StoredFinding> {
    let mut out: Vec<StoredFinding> = Vec::new();
    let mut index: HashMap<FoldKey<'_>, usize> = HashMap::new();
    for entry in entries {
        // An unsigned finding cannot be keyed, so it stays its own row
        // rather than folding every unsigned finding into one.
        if entry.finding.signature.is_empty() {
            out.push(entry.clone());
            continue;
        }
        let grouping = entry.finding.grouping_identity();
        let key = (grouping, entry.finding.signature.as_str());
        if let Some(&i) = index.get(&key) {
            let kept: &mut StoredFinding = &mut out[i];
            let seen = kept.seen_count + entry.seen_count;
            let first = kept.first_seen_ms.min(entry.first_seen_ms);
            let last = kept.stored_at_ms.max(entry.stored_at_ms);
            // Severity is ordered Critical < Warning < Info, so a
            // strictly smaller severity is a worse one. Take the whole
            // instance with it, evidence included.
            if entry.finding.severity < kept.finding.severity {
                kept.finding = entry.finding.clone();
            }
            kept.seen_count = seen;
            kept.first_seen_ms = first;
            kept.stored_at_ms = last;
        } else {
            index.insert(key, out.len());
            out.push(entry.clone());
        }
    }
    out
}

/// Service and finding type are invariant within a signature, so both
/// read paths screen on them during the buffer pass rather than after
/// the fold. Shared so a third caller cannot screen on one and not the
/// other.
fn matches_service_and_type(sf: &StoredFinding, filter: &FindingsFilter) -> bool {
    if let Some(ref svc) = filter.service
        && sf.finding.service != *svc
    {
        return false;
    }
    if let Some(ref ft) = filter.finding_type
        && sf.finding.finding_type.as_str() != ft.as_str()
    {
        return false;
    }
    true
}

/// Merge a later fold of the same window into an earlier one, by the key
/// [`fold_entries`] uses. Rows present in both keep the larger
/// `seen_count` rather than the sum, since both folds counted the same
/// instances, the earliest `first_seen_ms`, the latest `stored_at_ms` and
/// the worse severity. Rows only the later fold has are appended, rows
/// only the earlier one has are kept: the record can grow, never lose.
pub(crate) fn merge_folded(into: &mut Vec<StoredFinding>, later: Vec<StoredFinding>) {
    let mut index: HashMap<(Option<(String, String)>, String), usize> = into
        .iter()
        .enumerate()
        .filter(|(_, sf)| !sf.finding.signature.is_empty())
        .map(|(i, sf)| (owned_fold_key(sf), i))
        .collect();
    for entry in later {
        if entry.finding.signature.is_empty() {
            into.push(entry);
            continue;
        }
        if let Some(&i) = index.get(&owned_fold_key(&entry)) {
            let kept = &mut into[i];
            kept.seen_count = kept.seen_count.max(entry.seen_count);
            kept.first_seen_ms = kept.first_seen_ms.min(entry.first_seen_ms);
            kept.stored_at_ms = kept.stored_at_ms.max(entry.stored_at_ms);
            if entry.finding.severity < kept.finding.severity {
                kept.finding = entry.finding;
            }
        } else {
            index.insert(owned_fold_key(&entry), into.len());
            into.push(entry);
        }
    }
}

/// The fold key as owned strings, for a map that outlives the borrow of
/// the vector it indexes into.
fn owned_fold_key(sf: &StoredFinding) -> (Option<(String, String)>, String) {
    (
        sf.finding
            .grouping_identity()
            .map(|(k, v)| (k.to_string(), v.to_string())),
        sf.finding.signature.clone(),
    )
}

/// Both bounds on one instance's stamp, inclusive. The cheapest and most
/// selective screen, so it runs first on the unfolded path.
fn in_time_bounds(sf: &StoredFinding, filter: &FindingsFilter) -> bool {
    filter.since_ms.is_none_or(|s| sf.stored_at_ms >= s)
        && filter.until_ms.is_none_or(|u| sf.stored_at_ms <= u)
}

/// The fold under an already held read guard, see
/// [`FindingsStore::query_coalesced`] for the two time shapes.
fn coalesce_locked(buf: &VecDeque<StoredFinding>, filter: &FindingsFilter) -> Vec<StoredFinding> {
    // `until_ms` makes it a window: both bounds screen each instance during
    // the pass, cheapest test first. Without it, `since_ms` is a delta poll
    // and lands after the fold so a row keeps its whole history.
    let windowed = filter.until_ms.is_some();
    let mut folded = fold_entries(buf.iter().rev().filter(|sf| {
        (!windowed || in_time_bounds(sf, filter)) && matches_service_and_type(sf, filter)
    }));
    if let Some(ref sev) = filter.severity {
        folded.retain(|sf| sf.finding.severity.as_str() == sev.as_str());
    }
    if !windowed && let Some(since) = filter.since_ms {
        folded.retain(|sf| sf.stored_at_ms >= since);
    }
    folded.truncate(filter.limit);
    folded
}

/// Query filter for the findings store.
/// `#[non_exhaustive]` so a future field stays a minor bump rather than
/// a breaking change: external crates cannot construct it with a
/// struct literal, only read it or deserialize into it.
#[non_exhaustive]
#[derive(Debug, Default)]
pub struct FindingsFilter {
    /// Optional service name filter. Matches the finding's `service` field.
    pub service: Option<String>,
    /// Optional finding type filter, in `snake_case` (e.g. `n_plus_one_sql`).
    pub finding_type: Option<String>,
    /// Optional severity filter, in `snake_case` (`critical`, `warning`, `info`).
    pub severity: Option<String>,
    /// Optional lower bound on `stored_at_ms`, in Unix epoch milliseconds,
    /// inclusive. Keeps what was detected at or after that instant, so a
    /// poller can ask for a delta instead of re-reading the whole buffer.
    pub since_ms: Option<u64>,
    /// Optional upper bound on `stored_at_ms`, in Unix epoch milliseconds,
    /// inclusive. Present, it makes the query a window from `since_ms` or
    /// the start of the buffer, screened before the fold, see
    /// [`FindingsStore::query_coalesced`].
    pub until_ms: Option<u64>,
    /// Maximum number of results to return.
    pub limit: usize,
}

/// Thread-safe ring buffer for recent findings.
///
/// Shared between `process_traces` (writer, exclusive lock) and the
/// query API handlers (readers, shared lock).
#[derive(Debug)]
pub struct FindingsStore {
    inner: RwLock<VecDeque<StoredFinding>>,
    max_size: usize,
}

impl FindingsStore {
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        // Pre-allocate the ring buffer to reduce the number of reallocations
        // that `extend` in `push_batch` can trigger under the writer lock.
        // Reallocating under the lock briefly blocks query API readers.
        //
        // The ceiling is deliberately low: the default
        // `max_retained_findings = 10_000` is already well under `65k`
        // worth of StoredFinding slots (~12 MB), and users who set a much
        // higher cap typically want to pay the initial-memory cost lazily.
        const INITIAL_CAPACITY_CEILING: usize = 4096;
        let capacity = max_size.min(INITIAL_CAPACITY_CEILING);
        Self {
            inner: RwLock::new(VecDeque::with_capacity(capacity)),
            max_size,
        }
    }

    /// Append findings from a detection batch. Evicts oldest entries
    /// when the buffer exceeds capacity.
    ///
    /// The clones happen outside the write lock so concurrent query API
    /// readers only wait for the short `extend + truncate` critical
    /// section, not for N `Finding::clone()` allocations.
    pub async fn push_batch(&self, findings: &[Finding], now_ms: u64) {
        if findings.is_empty() || self.max_size == 0 {
            // `max_size == 0` disables the store entirely (users set this
            // via `[daemon] max_retained_findings = 0` to reclaim memory
            // when the query API is disabled). Short-circuit here to
            // avoid cloning findings we will immediately drain.
            return;
        }
        // Clone and build the new entries OUTSIDE the lock.
        let new_entries: Vec<StoredFinding> = findings
            .iter()
            .map(|f| StoredFinding {
                finding: f.clone(),
                stored_at_ms: now_ms,
                first_seen_ms: now_ms,
                seen_count: 1,
            })
            .collect();

        let mut buf = self.inner.write().await;
        buf.extend(new_entries);
        // Drop oldest entries if we exceeded capacity. `drain(..n)` on a
        // VecDeque is O(n), which is acceptable since n is typically small
        // (one batch's worth of excess, not the whole buffer).
        if buf.len() > self.max_size {
            let excess = buf.len() - self.max_size;
            buf.drain(..excess);
        }
    }

    /// Query findings with optional filters, newest first.
    ///
    /// Returns raw per-trace detections. Callers that list findings for
    /// a human fold them with [`coalesce_by_signature`]; callers that
    /// count them (the quality gate) must not, a pattern hitting 20
    /// traces is 20 findings against a threshold.
    ///
    /// `filter.limit` is used as-is. Callers set the default (the query
    /// API handler in `query_api.rs` caps at `MAX_FINDINGS_LIMIT` and
    /// falls back to 100 when `?limit=` is absent). This function trusts
    /// its caller rather than silently rewriting `0` to a sentinel.
    pub async fn query(&self, filter: &FindingsFilter) -> Vec<StoredFinding> {
        let buf = self.inner.read().await;
        let limit = filter.limit;
        buf.iter()
            .rev()
            .filter(|sf| {
                in_time_bounds(sf, filter)
                    && matches_service_and_type(sf, filter)
                    && filter
                        .severity
                        .as_ref()
                        .is_none_or(|sev| sf.finding.severity.as_str() == sev.as_str())
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Fold per-trace detections into one entry per effective namespace and
    /// signature, then apply `filter.limit` to the FOLDED rows.
    ///
    /// The limit lands after the fold on purpose: applied before, a
    /// pattern recurring on 100 traces would consume the whole page and
    /// hide every other problem behind it. Severity is filtered after the
    /// fold too, against the group's worst, so `?severity=critical` cannot
    /// report the same problem with a different `seen_count`.
    ///
    /// Two time shapes. `until_ms` makes it a window, `[since_ms, until_ms]`
    /// with `since_ms` defaulting to the start of the buffer, screened
    /// during the buffer pass so `first_seen_ms` and `seen_count` describe
    /// the window: after the fold a group's stamp is its most recent
    /// detection, and an upper bound applied there would keep only the
    /// groups that had gone quiet by then. `since_ms` alone is a delta
    /// poll, applied after the fold against that most recent detection,
    /// so a row's history stays whole however the bound moves.
    pub async fn query_coalesced(&self, filter: &FindingsFilter) -> Vec<StoredFinding> {
        let buf = self.inner.read().await;
        coalesce_locked(&buf, filter)
    }

    /// [`Self::query_coalesced`] plus the ring's oldest stamp, read under
    /// the same guard, so the completeness marker describes the very pass
    /// that produced the rows and an eviction in between cannot make it
    /// vouch for more than was captured.
    pub async fn query_coalesced_with_oldest(
        &self,
        filter: &FindingsFilter,
    ) -> (Vec<StoredFinding>, Option<u64>) {
        let buf = self.inner.read().await;
        let oldest = buf.front().map(|sf| sf.stored_at_ms);
        (coalesce_locked(&buf, filter), oldest)
    }

    /// Detection time of the oldest retained finding, `None` when the
    /// buffer is empty.
    ///
    /// Separates "nothing was firing in that window" from "the ring does
    /// not reach that far back", which are otherwise the same empty answer
    /// to a window query. The buffer is FIFO and one batch shares one
    /// stamp, so the front is the oldest.
    pub async fn oldest_ms(&self) -> Option<u64> {
        self.inner.read().await.front().map(|sf| sf.stored_at_ms)
    }

    /// Get every retained detection for a specific trace, newest first.
    ///
    /// Raw instances, not folded: this is the triage path for a trace
    /// whose spans have already aged out of the window, so it must
    /// answer for any trace still in the buffer, see `docs/RUNBOOK.md`.
    pub async fn by_trace_id(&self, trace_id: &str) -> Vec<StoredFinding> {
        let buf = self.inner.read().await;
        buf.iter()
            .rev()
            .filter(|sf| sf.finding.trace_id == trace_id)
            .cloned()
            .collect()
    }

    /// Current count of stored findings.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Whether the store is empty.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acknowledgments::enrich_with_signatures;
    use crate::detect::{Confidence, FindingType, Pattern, Severity};

    fn make_finding(service: &str, finding_type: FindingType) -> Finding {
        make_finding_with_template(service, finding_type, "SELECT 1")
    }

    /// The signature hashes `(type, service, endpoint, template)`, so a
    /// distinct template is what makes two findings distinct problems.
    fn make_finding_with_template(
        service: &str,
        finding_type: FindingType,
        template: &str,
    ) -> Finding {
        Finding {
            finding_type,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: service.to_string(),
            grouping: Vec::new(),
            source_endpoint: "POST /api/test".to_string(),
            pattern: Pattern {
                template: template.to_string(),
                occurrences: 5,
                window_ms: 200,
                distinct_params: 5,
                ..Default::default()
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.200Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            classification_method: None,
            code_location: None,
            instrumentation_scopes: Vec::new(),
            suggested_fix: None,
            signature: String::new(),
        }
    }

    #[tokio::test]
    async fn max_size_zero_disables_store() {
        // When `max_retained_findings = 0`, push_batch should short-circuit
        // without allocating or mutating the ring buffer. Used by daemon
        // operators who disable the query API and want to reclaim memory.
        let store = FindingsStore::new(0);
        let f = make_finding("svc", FindingType::NPlusOneSql);
        store.push_batch(&[f], 1000).await;
        assert_eq!(store.len().await, 0);
        assert!(store.is_empty().await);
    }

    #[tokio::test]
    async fn push_batch_respects_capacity() {
        let store = FindingsStore::new(3);
        let findings: Vec<Finding> = (0..5)
            .map(|i| {
                let mut f = make_finding("svc", FindingType::NPlusOneSql);
                f.trace_id = format!("trace-{i}");
                f
            })
            .collect();
        store.push_batch(&findings, 1000).await;
        assert_eq!(store.len().await, 3);
        // Oldest entries evicted: only trace-2, trace-3, trace-4 remain.
        let all = store
            .query(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        let trace_ids: Vec<&str> = all.iter().map(|sf| sf.finding.trace_id.as_str()).collect();
        assert!(trace_ids.contains(&"trace-4"));
        assert!(trace_ids.contains(&"trace-3"));
        assert!(trace_ids.contains(&"trace-2"));
        assert!(!trace_ids.contains(&"trace-0"));
    }

    #[tokio::test]
    async fn query_keeps_instances_and_coalesced_folds_them() {
        // The tester's repro: one recurring pattern re-detected on 2
        // traces lists ONCE for a reader, while the raw instances stay
        // available for the gate and for per-trace triage.
        let store = FindingsStore::new(100);
        for (trace, ts) in [("trace-a", 1000u64), ("trace-b", 2000)] {
            let mut f = make_finding("svc", FindingType::RedundantSql);
            f.trace_id = trace.to_string();
            enrich_with_signatures(std::slice::from_mut(&mut f));
            store.push_batch(&[f], ts).await;
        }
        let filter = FindingsFilter {
            limit: 100,
            ..Default::default()
        };
        assert_eq!(store.query(&filter).await.len(), 2, "instances retained");

        let folded = store.query_coalesced(&filter).await;
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].seen_count, 2);
        assert_eq!(folded[0].first_seen_ms, 1000);
        assert_eq!(folded[0].stored_at_ms, 2000);
        assert_eq!(
            folded[0].finding.trace_id, "trace-b",
            "the latest instance is the one kept"
        );
    }

    #[tokio::test]
    async fn coalescing_keeps_identical_signatures_separate_by_grouping_value() {
        let mut prod = make_finding("svc", FindingType::RedundantSql);
        prod.grouping = crate::test_helpers::k8s_grouping("prod-eu");
        prod.grouping = crate::test_helpers::grouping("service.namespace", "payments");
        let mut staging = make_finding("svc", FindingType::RedundantSql);
        staging.grouping = crate::test_helpers::grouping("service.namespace", "staging");
        enrich_with_signatures(std::slice::from_mut(&mut prod));
        enrich_with_signatures(std::slice::from_mut(&mut staging));
        assert_eq!(prod.signature, staging.signature);

        let store = FindingsStore::new(100);
        store.push_batch(&[prod], 1000).await;
        store.push_batch(&[staging], 2000).await;

        let folded = store
            .query_coalesced(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(folded.len(), 2);
    }

    #[tokio::test]
    async fn coalescing_keeps_equal_grouping_values_separate_by_key() {
        let mut tenant = make_finding("svc", FindingType::RedundantSql);
        tenant.grouping = crate::test_helpers::grouping("tenant.id", "prod");
        let mut namespace = make_finding("svc", FindingType::RedundantSql);
        namespace.grouping = crate::test_helpers::grouping("k8s.namespace.name", "prod");
        enrich_with_signatures(std::slice::from_mut(&mut tenant));
        enrich_with_signatures(std::slice::from_mut(&mut namespace));

        let store = FindingsStore::new(100);
        store.push_batch(&[tenant], 1000).await;
        store.push_batch(&[namespace], 2000).await;

        let folded = store
            .query_coalesced(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(folded.len(), 2);
    }

    #[tokio::test]
    async fn folded_row_evidence_matches_its_severity() {
        // The row must not claim a severity its own trace_id and
        // occurrence count contradict: the representative is the
        // detection that EARNED the worst severity, evidence included.
        let mut critical = make_finding("svc", FindingType::NPlusOneSql);
        critical.severity = Severity::Critical;
        critical.trace_id = "trace-hot".to_string();
        critical.pattern.occurrences = 12;
        let mut warning = make_finding("svc", FindingType::NPlusOneSql);
        warning.severity = Severity::Warning;
        warning.trace_id = "trace-quiet".to_string();
        warning.pattern.occurrences = 6;
        enrich_with_signatures(std::slice::from_mut(&mut critical));
        enrich_with_signatures(std::slice::from_mut(&mut warning));

        let store = FindingsStore::new(100);
        store.push_batch(&[critical], 1000).await;
        store.push_batch(&[warning], 2000).await;

        let folded = store
            .query_coalesced(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(folded.len(), 1);
        let row = &folded[0];
        assert_eq!(row.finding.severity, Severity::Critical);
        assert_eq!(
            row.finding.trace_id, "trace-hot",
            "the critical row must point at the trace that earned it"
        );
        assert_eq!(row.finding.pattern.occurrences, 12);
        // The timestamps stay group metadata, spanning both detections.
        assert_eq!(row.first_seen_ms, 1000);
        assert_eq!(row.stored_at_ms, 2000);
        assert_eq!(row.seen_count, 2);
    }

    #[tokio::test]
    async fn severity_filter_applies_after_folding() {
        // Filtering instances first would report the same problem with a
        // different seen_count depending on the filter passed.
        let mut critical = make_finding("svc", FindingType::NPlusOneSql);
        critical.severity = Severity::Critical;
        let mut warning = make_finding("svc", FindingType::NPlusOneSql);
        warning.severity = Severity::Warning;
        enrich_with_signatures(std::slice::from_mut(&mut critical));
        enrich_with_signatures(std::slice::from_mut(&mut warning));

        let store = FindingsStore::new(100);
        store.push_batch(&[critical], 1000).await;
        store.push_batch(&[warning], 2000).await;

        let unfiltered = store
            .query_coalesced(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        let filtered = store
            .query_coalesced(&FindingsFilter {
                severity: Some("critical".to_string()),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(filtered.len(), 1, "the group's worst severity matches");
        assert_eq!(
            filtered[0].seen_count, unfiltered[0].seen_count,
            "the same problem must not report two different counts"
        );
    }

    #[tokio::test]
    async fn since_ms_filters_on_the_most_recent_detection() {
        // A recurring problem must stay visible through a delta query with
        // the history it really has, not the slice inside the window.
        let store = FindingsStore::new(100);
        let mut recurring =
            make_finding_with_template("svc", FindingType::RedundantSql, "SELECT recurring");
        enrich_with_signatures(std::slice::from_mut(&mut recurring));
        store
            .push_batch(std::slice::from_ref(&recurring), 1000)
            .await;
        store.push_batch(&[recurring], 5000).await;
        let mut quiet = make_finding_with_template("svc", FindingType::NPlusOneSql, "SELECT quiet");
        enrich_with_signatures(std::slice::from_mut(&mut quiet));
        store.push_batch(&[quiet], 1000).await;

        let filter = FindingsFilter {
            since_ms: Some(5000),
            limit: 100,
            ..Default::default()
        };
        let folded = store.query_coalesced(&filter).await;
        assert_eq!(
            folded.len(),
            1,
            "the bound is inclusive, and drops the quiet one"
        );
        assert_eq!(
            folded[0].first_seen_ms, 1000,
            "history predating the window survives"
        );
        assert_eq!(folded[0].seen_count, 2, "so does the count");

        let raw = store.query(&filter).await;
        assert_eq!(raw.len(), 1, "the unfolded path honours the same bound");
        assert_eq!(raw[0].stored_at_ms, 5000);

        // Both post-fold filters apply, not whichever runs last.
        let narrowed = store
            .query_coalesced(&FindingsFilter {
                since_ms: Some(5000),
                severity: Some("info".to_string()),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert!(narrowed.is_empty(), "severity still applies under a bound");
    }

    #[tokio::test]
    async fn a_window_scopes_the_history_a_single_bound_leaves_whole() {
        // The test that separates the correct implementation from the one
        // that filters after the fold: there, a chronic pattern whose
        // lifetime envelope straddles the window matches every window ever
        // asked for, and reports counts from outside it.
        let store = FindingsStore::new(100);
        let mut chronic =
            make_finding_with_template("svc", FindingType::RedundantSql, "SELECT chronic");
        enrich_with_signatures(std::slice::from_mut(&mut chronic));
        store.push_batch(std::slice::from_ref(&chronic), 1000).await;
        store.push_batch(&[chronic], 5000).await;

        let windowed = store
            .query_coalesced(&FindingsFilter {
                since_ms: Some(4000),
                until_ms: Some(6000),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(windowed.len(), 1, "the in-window detection is kept");
        assert_eq!(
            windowed[0].seen_count, 1,
            "a window counts only what fired inside it"
        );
        assert_eq!(
            windowed[0].first_seen_ms, 5000,
            "and dates the row from inside it"
        );

        let delta = store
            .query_coalesced(&FindingsFilter {
                since_ms: Some(4000),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(delta[0].seen_count, 2, "one bound keeps the whole history");
        assert_eq!(delta[0].first_seen_ms, 1000);

        let before = store
            .query_coalesced(&FindingsFilter {
                until_ms: Some(2000),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(
            before.len(),
            1,
            "an upper bound alone is a window from the start"
        );
        assert_eq!(
            before[0].seen_count, 1,
            "and it does not report the detection that came after it"
        );

        let raw = store
            .query(&FindingsFilter {
                until_ms: Some(2000),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(raw.len(), 1, "the unfolded path bounds each instance");
        assert_eq!(raw[0].stored_at_ms, 1000);
    }

    #[test]
    fn merge_folded_only_grows_the_earlier_capture() {
        let mut a = make_finding_with_template("svc", FindingType::RedundantSql, "SELECT a");
        let mut b = make_finding_with_template("svc", FindingType::RedundantSql, "SELECT b");
        let mut c = make_finding_with_template("svc", FindingType::NPlusOneSql, "SELECT c");
        for f in [&mut a, &mut b, &mut c] {
            enrich_with_signatures(std::slice::from_mut(f));
        }
        let row = |f: &Finding, first: u64, last: u64, seen: u64| StoredFinding {
            finding: f.clone(),
            stored_at_ms: last,
            first_seen_ms: first,
            seen_count: seen,
        };
        // The earlier fold saw A and B; by the later one the ring evicted A
        // and analysis added C, and B gained one more detection.
        let mut earlier = vec![row(&a, 1000, 1000, 1), row(&b, 2000, 2000, 2)];
        let later = vec![row(&b, 2000, 5000, 3), row(&c, 6000, 6000, 1)];
        merge_folded(&mut earlier, later);
        let by_template: std::collections::HashMap<&str, &StoredFinding> = earlier
            .iter()
            .map(|sf| (sf.finding.pattern.template.as_str(), sf))
            .collect();
        assert_eq!(earlier.len(), 3, "A is kept, C is added");
        assert!(
            by_template.contains_key("SELECT a"),
            "an evicted group is not lost"
        );
        let b_row = by_template["SELECT b"];
        assert_eq!(b_row.seen_count, 3, "the larger count, not the sum");
        assert_eq!(b_row.stored_at_ms, 5000);
        assert_eq!(b_row.first_seen_ms, 2000);
    }

    #[tokio::test]
    async fn oldest_ms_reports_the_front_of_the_ring() {
        let store = FindingsStore::new(2);
        assert_eq!(store.oldest_ms().await, None, "empty ring has no oldest");
        for (i, stamp) in [1000u64, 2000, 3000].iter().enumerate() {
            let mut f = make_finding_with_template(
                "svc",
                FindingType::RedundantSql,
                &format!("SELECT {i}"),
            );
            enrich_with_signatures(std::slice::from_mut(&mut f));
            store.push_batch(&[f], *stamp).await;
        }
        assert_eq!(
            store.oldest_ms().await,
            Some(2000),
            "eviction moves the front, which is what bounds a window query"
        );
    }

    #[tokio::test]
    async fn coalesced_limit_applies_after_folding() {
        // A hot pattern recurring 50 times must not consume the page and
        // hide the other problems behind it.
        let store = FindingsStore::new(1000);
        for i in 0..50u64 {
            let mut hot =
                make_finding_with_template("svc", FindingType::RedundantSql, "SELECT hot");
            enrich_with_signatures(std::slice::from_mut(&mut hot));
            store.push_batch(&[hot], 1000 + i).await;
        }
        let mut cold = make_finding_with_template("svc", FindingType::NPlusOneSql, "SELECT cold");
        enrich_with_signatures(std::slice::from_mut(&mut cold));
        store.push_batch(&[cold], 2000).await;

        let folded = store
            .query_coalesced(&FindingsFilter {
                limit: 2,
                ..Default::default()
            })
            .await;
        let templates: Vec<&str> = folded
            .iter()
            .map(|sf| sf.finding.pattern.template.as_str())
            .collect();
        assert_eq!(templates, ["SELECT cold", "SELECT hot"]);
        assert_eq!(folded[1].seen_count, 50);
    }

    #[tokio::test]
    async fn by_trace_id_answers_for_an_older_recurrence() {
        // RUNBOOK triage: a trace_id read off a log line must still
        // resolve after the pattern recurred on newer traces.
        let store = FindingsStore::new(100);
        for (trace, ts) in [("trace-old", 1000u64), ("trace-new", 2000)] {
            let mut f = make_finding("svc", FindingType::RedundantSql);
            f.trace_id = trace.to_string();
            enrich_with_signatures(std::slice::from_mut(&mut f));
            store.push_batch(&[f], ts).await;
        }
        let hits = store.by_trace_id("trace-old").await;
        assert_eq!(hits.len(), 1, "the older instance is still retrievable");
        assert_eq!(hits[0].finding.trace_id, "trace-old");
    }

    #[tokio::test]
    async fn query_filters_by_service() {
        let store = FindingsStore::new(100);
        let f1 = make_finding("order-svc", FindingType::NPlusOneSql);
        let f2 = make_finding("payment-svc", FindingType::NPlusOneSql);
        store.push_batch(&[f1, f2], 1000).await;

        let results = store
            .query(&FindingsFilter {
                service: Some("order-svc".to_string()),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].finding.service, "order-svc");
    }

    #[tokio::test]
    async fn query_filters_by_type() {
        let store = FindingsStore::new(100);
        let f1 = make_finding("svc", FindingType::NPlusOneSql);
        let f2 = make_finding("svc", FindingType::RedundantSql);
        store.push_batch(&[f1, f2], 1000).await;

        let results = store
            .query(&FindingsFilter {
                finding_type: Some("n_plus_one_sql".to_string()),
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].finding.finding_type, FindingType::NPlusOneSql);
    }

    #[tokio::test]
    async fn by_trace_id_filters_correctly() {
        let store = FindingsStore::new(100);
        let mut f1 = make_finding_with_template("svc", FindingType::NPlusOneSql, "SELECT a");
        f1.trace_id = "trace-a".to_string();
        let mut f2 = make_finding_with_template("svc", FindingType::NPlusOneSql, "SELECT b");
        f2.trace_id = "trace-b".to_string();
        store.push_batch(&[f1, f2], 1000).await;

        let results = store.by_trace_id("trace-a").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].finding.trace_id, "trace-a");
    }

    #[tokio::test]
    async fn query_respects_limit() {
        let store = FindingsStore::new(100);
        let findings: Vec<Finding> = (0..10)
            .map(|i| {
                make_finding_with_template("svc", FindingType::NPlusOneSql, &format!("SELECT {i}"))
            })
            .collect();
        store.push_batch(&findings, 1000).await;

        let results = store
            .query(&FindingsFilter {
                limit: 3,
                ..Default::default()
            })
            .await;
        assert_eq!(results.len(), 3);
    }
}
