//! Ring-buffer store for recent findings, queryable by the daemon API.
//!
//! Detection is per trace, so a recurring pattern re-emits an identical
//! finding for every trace that exhibits it. The buffer keeps those
//! instances: they carry the per-trace severity, they let
//! `by_trace_id` answer for a trace whose spans have aged out, and FIFO
//! pressure is what expires a fixed problem. Listing them raw is what
//! reads as duplicate rows, so [`coalesce_by_signature`] folds them at
//! READ time, per surface, leaving the stored history intact.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use tokio::sync::RwLock;

use crate::detect::Finding;

/// A finding with daemon-side metadata.
///
/// A raw buffer entry describes one detection: `seen_count` is 1 and
/// `first_seen_ms` equals `stored_at_ms`. After
/// [`coalesce_by_signature`] the entry stands for every folded
/// detection of that signature, see that function for which fields it
/// merges.
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

/// Fold per-trace detections into one entry per signature, newest
/// first, preserving the input order.
///
/// `entries` is expected newest-first (what [`FindingsStore::query`]
/// yields). The kept instance is the most recent one, but its severity
/// is the WORST across the fold: severity is derived per trace (12
/// repeats is critical, 6 is a warning), so keeping the latest alone
/// would let a quiet trace silently downgrade a critical finding out of
/// a `?severity=critical` filter. `seen_count` counts the fold,
/// `first_seen_ms` is the oldest detection retained.
#[must_use]
pub fn coalesce_by_signature(entries: &[StoredFinding]) -> Vec<StoredFinding> {
    let mut out: Vec<StoredFinding> = Vec::with_capacity(entries.len());
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(entries.len());
    for entry in entries {
        // An unsigned finding cannot be keyed, so it stays its own row
        // rather than folding every unsigned finding into one.
        if entry.finding.signature.is_empty() {
            out.push(entry.clone());
            continue;
        }
        if let Some(&i) = index.get(entry.finding.signature.as_str()) {
            let kept: &mut StoredFinding = &mut out[i];
            kept.seen_count += entry.seen_count;
            kept.first_seen_ms = kept.first_seen_ms.min(entry.first_seen_ms);
            // Severity is ordered Critical < Warning < Info, so the
            // worst of two is the min.
            if entry.finding.severity < kept.finding.severity {
                kept.finding.severity = entry.finding.severity.clone();
            }
        } else {
            index.insert(entry.finding.signature.as_str(), out.len());
            out.push(entry.clone());
        }
    }
    out
}

/// Query filter for the findings store.
#[derive(Debug, Default)]
pub struct FindingsFilter {
    /// Optional service name filter. Matches the finding's `service` field.
    pub service: Option<String>,
    /// Optional finding type filter, in `snake_case` (e.g. `n_plus_one_sql`).
    pub finding_type: Option<String>,
    /// Optional severity filter, in `snake_case` (`critical`, `warning`, `info`).
    pub severity: Option<String>,
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
                if let Some(ref sev) = filter.severity
                    && sf.finding.severity.as_str() != sev.as_str()
                {
                    return false;
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Query with the same filters, then fold per-trace detections into
    /// one entry per signature and apply `filter.limit` to the FOLDED
    /// rows.
    ///
    /// The limit lands after the fold on purpose: applied before, a
    /// pattern recurring on 100 traces would consume the whole page and
    /// hide every other problem behind it.
    pub async fn query_coalesced(&self, filter: &FindingsFilter) -> Vec<StoredFinding> {
        let unlimited = FindingsFilter {
            service: filter.service.clone(),
            finding_type: filter.finding_type.clone(),
            severity: filter.severity.clone(),
            limit: usize::MAX,
        };
        let mut folded = coalesce_by_signature(&self.query(&unlimited).await);
        folded.truncate(filter.limit);
        folded
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
    async fn coalescing_keeps_the_worst_severity() {
        // Severity is derived per trace, so a quiet trace must not
        // downgrade a critical finding out of a `?severity=critical`
        // filter. Severity is ordered Critical < Warning < Info.
        let mut critical = make_finding("svc", FindingType::NPlusOneSql);
        critical.severity = Severity::Critical;
        critical.trace_id = "trace-hot".to_string();
        let mut warning = make_finding("svc", FindingType::NPlusOneSql);
        warning.severity = Severity::Warning;
        warning.trace_id = "trace-quiet".to_string();
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
        assert_eq!(
            folded[0].finding.severity,
            Severity::Critical,
            "the later warning must not mask the critical occurrence"
        );
        assert_eq!(
            folded[0].finding.trace_id, "trace-quiet",
            "the latest instance is still the one shown"
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
