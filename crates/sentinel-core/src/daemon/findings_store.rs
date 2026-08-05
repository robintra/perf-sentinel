//! Signature-coalesced store for recent findings, queryable by the
//! daemon API.
//!
//! Detection is per trace, so a recurring pattern re-emits an identical
//! finding for every trace that exhibits it. Stored raw, one hot
//! pattern reads as N duplicate rows on every operator surface and can
//! fill the whole store, evicting rarer findings. Entries are therefore
//! keyed by the canonical acknowledgment signature (the product's "same
//! problem" key): one entry per signature, refreshed in place with an
//! occurrence tally.

use std::collections::HashMap;

use serde::Serialize;
use tokio::sync::RwLock;

use crate::detect::Finding;

/// A finding with daemon-side metadata.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StoredFinding {
    /// The most recent instance of this finding (per signature). Its
    /// `trace_id` and timestamps describe the latest occurrence.
    pub finding: Finding,
    /// Monotonic timestamp (ms) when this signature was last stored.
    pub stored_at_ms: u64,
    /// Monotonic timestamp (ms) when this signature was first stored.
    /// `0` on payloads from versions predating coalescing.
    #[serde(default)]
    pub first_seen_ms: u64,
    /// How many per-trace instances this entry coalesces. Defaults to 1
    /// on payloads from versions predating coalescing.
    #[serde(default = "default_seen_count")]
    pub seen_count: u64,
}

fn default_seen_count() -> u64 {
    1
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

/// Thread-safe signature-keyed store for recent findings.
///
/// Shared between `process_traces` (writer, exclusive lock) and the
/// query API handlers (readers, shared lock).
#[derive(Debug)]
pub struct FindingsStore {
    inner: RwLock<HashMap<String, StoredFinding>>,
    max_size: usize,
}

impl FindingsStore {
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            max_size,
        }
    }

    /// Store findings from a detection batch, coalescing by signature.
    ///
    /// A finding whose signature is already stored refreshes that entry
    /// in place: latest instance kept, `seen_count` incremented,
    /// `first_seen_ms` preserved. When the number of distinct signatures
    /// exceeds capacity, the least recently seen entries are evicted.
    ///
    /// The clones and signature keys are built OUTSIDE the write lock so
    /// concurrent query API readers only wait for the map upserts.
    pub async fn push_batch(&self, findings: &[Finding], now_ms: u64) {
        if findings.is_empty() || self.max_size == 0 {
            // `max_size == 0` disables the store entirely (users set this
            // via `[daemon] max_retained_findings = 0` to reclaim memory
            // when the query API is disabled). Short-circuit here to
            // avoid cloning findings we will immediately drop.
            return;
        }
        // Clone and key the new entries OUTSIDE the lock. The daemon
        // enriches signatures before pushing; the compute fallback keeps
        // the coalescing key total for any future caller that does not.
        let keyed: Vec<(String, Finding)> = findings
            .iter()
            .map(|f| {
                let key = if f.signature.is_empty() {
                    crate::acknowledgments::compute_signature(f)
                } else {
                    f.signature.clone()
                };
                (key, f.clone())
            })
            .collect();

        let mut buf = self.inner.write().await;
        for (key, finding) in keyed {
            buf.entry(key)
                .and_modify(|sf| {
                    sf.finding = finding.clone();
                    sf.stored_at_ms = now_ms;
                    sf.seen_count += 1;
                })
                .or_insert(StoredFinding {
                    finding,
                    stored_at_ms: now_ms,
                    first_seen_ms: now_ms,
                    seen_count: 1,
                });
        }
        // Evict the least recently seen signatures past capacity. The
        // scan is O(n) per eviction, acceptable because evictions only
        // happen once max_size DISTINCT problems are live at once, not
        // on every recurrence of a hot pattern like the ring did.
        while buf.len() > self.max_size {
            let oldest = buf
                .iter()
                .min_by(|(ka, a), (kb, b)| {
                    a.stored_at_ms.cmp(&b.stored_at_ms).then_with(|| ka.cmp(kb))
                })
                .map(|(k, _)| k.clone());
            match oldest {
                Some(key) => buf.remove(&key),
                None => break,
            };
        }
    }

    /// Query findings with optional filters, most recently seen first.
    ///
    /// `filter.limit` is used as-is. Callers set the default (the query
    /// API handler in `query_api.rs` caps at `MAX_FINDINGS_LIMIT` and
    /// falls back to 100 when `?limit=` is absent). This function trusts
    /// its caller rather than silently rewriting `0` to a sentinel.
    pub async fn query(&self, filter: &FindingsFilter) -> Vec<StoredFinding> {
        let buf = self.inner.read().await;
        let limit = filter.limit;
        let mut hits: Vec<(&String, &StoredFinding)> = buf
            .iter()
            .filter(|(_, sf)| {
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
            .collect();
        // Newest first; the signature tiebreak keeps equal-timestamp
        // batches in a deterministic order across queries.
        hits.sort_by(|(ka, a), (kb, b)| {
            b.stored_at_ms.cmp(&a.stored_at_ms).then_with(|| ka.cmp(kb))
        });
        hits.into_iter()
            .take(limit)
            .map(|(_, sf)| sf.clone())
            .collect()
    }

    /// Get findings whose LATEST instance belongs to a specific trace.
    ///
    /// Coalescing keeps one instance per signature, so a trace whose
    /// finding has since recurred on a newer trace no longer lists here;
    /// `/api/explain/{trace_id}` stays the exhaustive per-trace view.
    pub async fn by_trace_id(&self, trace_id: &str) -> Vec<StoredFinding> {
        let buf = self.inner.read().await;
        buf.values()
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
        // 5 distinct problems pushed at increasing times into a store of
        // 3: the least recently seen signatures are evicted.
        let store = FindingsStore::new(3);
        for i in 0..5u64 {
            let f =
                make_finding_with_template("svc", FindingType::NPlusOneSql, &format!("SELECT {i}"));
            store.push_batch(&[f], 1000 + i).await;
        }
        assert_eq!(store.len().await, 3);
        let all = store
            .query(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        let templates: Vec<&str> = all
            .iter()
            .map(|sf| sf.finding.pattern.template.as_str())
            .collect();
        assert_eq!(templates, ["SELECT 4", "SELECT 3", "SELECT 2"]);
    }

    #[tokio::test]
    async fn same_signature_coalesces_into_one_entry() {
        // The tester's repro: one recurring pattern re-detected on N
        // traces must stay ONE entry, refreshed in place with a tally.
        let store = FindingsStore::new(100);
        let mut f1 = make_finding("svc", FindingType::RedundantSql);
        f1.trace_id = "trace-a".to_string();
        store.push_batch(&[f1], 1000).await;
        let mut f2 = make_finding("svc", FindingType::RedundantSql);
        f2.trace_id = "trace-b".to_string();
        store.push_batch(&[f2], 2000).await;

        assert_eq!(store.len().await, 1);
        let all = store
            .query(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        let sf = &all[0];
        assert_eq!(sf.seen_count, 2);
        assert_eq!(sf.first_seen_ms, 1000);
        assert_eq!(sf.stored_at_ms, 2000);
        assert_eq!(
            sf.finding.trace_id, "trace-b",
            "the latest instance is the one kept"
        );
    }

    #[tokio::test]
    async fn hot_pattern_does_not_evict_rare_findings() {
        // The recurrence of one hot signature must not consume capacity:
        // eviction only happens when DISTINCT problems exceed the cap.
        let store = FindingsStore::new(2);
        let cold = make_finding_with_template("svc", FindingType::NPlusOneSql, "SELECT cold");
        store.push_batch(&[cold], 1000).await;
        for i in 0..50u64 {
            let hot = make_finding_with_template("svc", FindingType::RedundantSql, "SELECT hot");
            store.push_batch(&[hot], 2000 + i).await;
        }
        assert_eq!(store.len().await, 2, "hot recurrences never grow the store");
        let all = store
            .query(&FindingsFilter {
                limit: 100,
                ..Default::default()
            })
            .await;
        assert_eq!(all[0].finding.pattern.template, "SELECT hot");
        assert_eq!(all[0].seen_count, 50);
        assert_eq!(
            all[1].finding.pattern.template, "SELECT cold",
            "the rare finding survives the hot pattern"
        );
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
