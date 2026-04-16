//! Ring-buffer store for recent findings, queryable by the daemon API.

use std::collections::VecDeque;

use serde::Serialize;
use tokio::sync::RwLock;

use crate::detect::Finding;

/// A finding with daemon-side metadata.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StoredFinding {
    /// The detected finding emitted by the detection stage.
    pub finding: Finding,
    /// Monotonic timestamp (ms) when this finding was stored.
    pub stored_at_ms: u64,
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
        // With `INITIAL_CAPACITY_CEILING = 4096`, the ring grows at most
        // log2(max_size / 4096) times before stabilizing, e.g. 4 growth
        // events for `max_size = 65_536`. Each growth pays one realloc
        // under the lock but amortizes across thousands of pushes.
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

    /// Query findings with optional filters.
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

    /// Get findings for a specific trace.
    pub async fn by_trace_id(&self, trace_id: &str) -> Vec<StoredFinding> {
        let buf = self.inner.read().await;
        buf.iter()
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
        Finding {
            finding_type,
            severity: Severity::Warning,
            trace_id: "trace-1".to_string(),
            service: service.to_string(),
            source_endpoint: "POST /api/test".to_string(),
            pattern: Pattern {
                template: "SELECT 1".to_string(),
                occurrences: 5,
                window_ms: 200,
                distinct_params: 5,
            },
            suggestion: "batch".to_string(),
            first_timestamp: "2025-07-10T14:32:01.000Z".to_string(),
            last_timestamp: "2025-07-10T14:32:01.200Z".to_string(),
            green_impact: None,
            confidence: Confidence::default(),
            code_location: None,
            suggested_fix: None,
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
        let mut f1 = make_finding("svc", FindingType::NPlusOneSql);
        f1.trace_id = "trace-a".to_string();
        let mut f2 = make_finding("svc", FindingType::NPlusOneSql);
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
            .map(|_| make_finding("svc", FindingType::NPlusOneSql))
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
