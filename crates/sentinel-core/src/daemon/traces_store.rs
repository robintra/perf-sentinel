//! Ring-buffer store of masked spans for the traces that produced a
//! finding.
//!
//! The correlation window drops a trace's spans a few seconds after it
//! completes, which is why `/api/explain/{trace_id}` only answers while
//! the trace is still live. A report exported afterwards carried
//! findings with no way to see what happened around them. This buffer
//! keeps the span trees the findings point at, masked through
//! [`EmbeddedTrace`], so `/api/export/report` hands over a report the
//! HTML dashboard can still draw.
//!
//! Bounded by trace count (`[daemon] max_retained_traces`), not by
//! bytes: spans per trace are already capped upstream by
//! `max_events_per_trace`.

use std::collections::{HashSet, VecDeque};

use tokio::sync::RwLock;

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::report::EmbeddedTrace;

/// Bounded FIFO of masked traces, keyed for lookup by trace id.
pub struct TracesStore {
    inner: RwLock<Inner>,
    capacity: usize,
}

#[derive(Default)]
struct Inner {
    traces: VecDeque<EmbeddedTrace>,
    ids: HashSet<String>,
}

impl TracesStore {
    /// A store holding at most `capacity` traces. Zero disables
    /// retention entirely, every operation then becomes a no-op.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            capacity,
        }
    }

    /// Retain the traces that `findings` point at, evicting oldest
    /// first past the capacity. Traces already held are skipped, so a
    /// pattern re-detected on the same trace does not churn the buffer.
    pub async fn retain_for(&self, traces: &[Trace], findings: &[Finding]) {
        if self.capacity == 0 || findings.is_empty() {
            return;
        }
        let wanted: HashSet<&str> = findings.iter().map(|f| f.trace_id.as_str()).collect();
        let mut inner = self.inner.write().await;
        for trace in traces {
            if !wanted.contains(trace.trace_id.as_str()) || inner.ids.contains(&trace.trace_id) {
                continue;
            }
            inner.ids.insert(trace.trace_id.clone());
            inner.traces.push_back(EmbeddedTrace::from_trace(trace));
            while inner.traces.len() > self.capacity {
                if let Some(evicted) = inner.traces.pop_front() {
                    inner.ids.remove(&evicted.trace_id);
                }
            }
        }
    }

    /// The retained traces that `findings` point at. Findings whose
    /// trace aged out are simply absent, which is what the dashboard
    /// reports as a trace missing from the embed.
    pub async fn snapshot_for(&self, findings: &[Finding]) -> Vec<EmbeddedTrace> {
        if self.capacity == 0 {
            return Vec::new();
        }
        let wanted: HashSet<&str> = findings.iter().map(|f| f.trace_id.as_str()).collect();
        let inner = self.inner.read().await;
        inner
            .traces
            .iter()
            .filter(|t| wanted.contains(t.trace_id.as_str()))
            .cloned()
            .collect()
    }

    /// Number of retained traces, for the occupancy gauge and tests.
    pub async fn len(&self) -> usize {
        self.inner.read().await.traces.len()
    }

    /// Whether the store holds no trace.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize::NormalizedEvent;

    fn trace(id: &str) -> Trace {
        Trace {
            trace_id: id.to_string(),
            spans: vec![NormalizedEvent {
                event: SpanEvent {
                    timestamp: "2025-07-10T14:32:01.123Z".to_string(),
                    trace_id: id.to_string(),
                    span_id: "s1".to_string(),
                    parent_span_id: None,
                    link_trace_id: None,
                    service: "svc".into(),
                    cloud_region: None,
                    event_type: EventType::Sql,
                    operation: "SELECT".to_string(),
                    target: "select * from t where id = 42".to_string(),
                    duration_us: 100,
                    source: EventSource {
                        endpoint: "GET /x".to_string(),
                        method: "GET".to_string(),
                    },
                    status_code: None,
                    response_size_bytes: None,
                    code_function: None,
                    code_filepath: None,
                    code_lineno: None,
                    code_namespace: None,
                    instrumentation_scopes: Vec::new(),
                },
                template: "select * from t where id = ?".into(),
                params: vec![],
            }],
        }
    }

    fn finding_on(trace_id: &str) -> Finding {
        let mut f = crate::test_helpers::make_finding(
            crate::detect::FindingType::RedundantSql,
            crate::detect::Severity::Info,
        );
        f.trace_id = trace_id.to_string();
        f
    }

    #[tokio::test]
    async fn keeps_only_traces_a_finding_points_at() {
        let store = TracesStore::new(10);
        store
            .retain_for(&[trace("a"), trace("b")], &[finding_on("a")])
            .await;
        assert_eq!(store.len().await, 1);
        let kept = store.snapshot_for(&[finding_on("a")]).await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].trace_id, "a");
    }

    #[tokio::test]
    async fn evicts_oldest_past_capacity() {
        let store = TracesStore::new(2);
        for id in ["a", "b", "c"] {
            store.retain_for(&[trace(id)], &[finding_on(id)]).await;
        }
        assert_eq!(store.len().await, 2);
        assert!(store.snapshot_for(&[finding_on("a")]).await.is_empty());
        assert_eq!(store.snapshot_for(&[finding_on("c")]).await.len(), 1);
    }

    #[tokio::test]
    async fn same_trace_twice_does_not_churn_the_buffer() {
        let store = TracesStore::new(2);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        store.retain_for(&[trace("b")], &[finding_on("b")]).await;
        assert_eq!(store.len().await, 2);
        assert_eq!(store.snapshot_for(&[finding_on("a")]).await.len(), 1);
    }

    #[tokio::test]
    async fn zero_capacity_retains_nothing() {
        let store = TracesStore::new(0);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        assert!(store.is_empty().await);
        assert!(store.snapshot_for(&[finding_on("a")]).await.is_empty());
    }

    #[tokio::test]
    async fn retained_spans_carry_the_template_not_the_literal() {
        let store = TracesStore::new(2);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        let kept = store.snapshot_for(&[finding_on("a")]).await;
        assert_eq!(kept[0].spans[0].template, "select * from t where id = ?");
    }
}
