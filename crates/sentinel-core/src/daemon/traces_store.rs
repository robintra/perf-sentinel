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
//! Bounded three ways: trace count (`[daemon] max_retained_traces`),
//! spans per trace (`max_events_per_trace`, so a TTL-split trace whose
//! flushes are merged cannot grow unbounded), and a byte budget applied
//! at snapshot time so the export stays fetchable.

use std::collections::{HashMap, HashSet, VecDeque};

use tokio::sync::RwLock;

use crate::correlate::Trace;
use crate::detect::Finding;
use crate::report::{EmbeddedSpan, EmbeddedTrace};

/// Bounded store of masked traces, keyed by trace id, evicting oldest
/// first where "oldest" means least recently flushed: a re-flushed
/// trace moves to the back, its findings are the freshest.
pub struct TracesStore {
    inner: RwLock<Inner>,
    capacity: usize,
    max_spans_per_trace: usize,
}

#[derive(Default)]
struct Inner {
    traces: HashMap<String, StoredTrace>,
    /// Flush order as `(trace_id, generation)`. A reflush pushes a new
    /// generation to the back and the old entry becomes a tombstone,
    /// skipped on eviction and compacted past twice the capacity, so
    /// refreshing a position never scans the deque.
    order: VecDeque<(String, u64)>,
    next_generation: u64,
}

struct StoredTrace {
    generation: u64,
    trace: EmbeddedTrace,
}

impl TracesStore {
    /// A store holding at most `capacity` traces of at most
    /// `max_spans_per_trace` spans each. Zero capacity disables
    /// retention entirely, every operation then becomes a no-op.
    #[must_use]
    pub fn new(capacity: usize, max_spans_per_trace: usize) -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            capacity,
            max_spans_per_trace,
        }
    }

    /// Retain the traces that `findings` point at, evicting the least
    /// recently flushed past the capacity. A trace id already held is
    /// merged, not replaced: the window flushes the same id more than
    /// once (TTL split, LRU eviction plus late spans) and each flush
    /// carries only its own spans, so either flush alone can miss the
    /// evidence a finding from the other one points at.
    pub async fn retain_for(&self, traces: &[Trace], findings: &[Finding]) {
        if self.capacity == 0 || findings.is_empty() {
            return;
        }
        let wanted: HashSet<&str> = findings.iter().map(|f| f.trace_id.as_str()).collect();
        let mut inner = self.inner.write().await;
        for trace in traces {
            if !wanted.contains(trace.trace_id.as_str()) {
                continue;
            }
            let embedded = EmbeddedTrace::from_trace(trace);
            inner.next_generation += 1;
            let generation = inner.next_generation;
            match inner.traces.entry(trace.trace_id.clone()) {
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    let stored = slot.get_mut();
                    stored.trace.spans = merge_spans(
                        std::mem::take(&mut stored.trace.spans),
                        embedded.spans,
                        self.max_spans_per_trace,
                    );
                    stored.generation = generation;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(StoredTrace {
                        generation,
                        trace: embedded,
                    });
                }
            }
            inner.order.push_back((trace.trace_id.clone(), generation));
            while inner.traces.len() > self.capacity {
                let Some((id, popped_generation)) = inner.order.pop_front() else {
                    break;
                };
                let live = inner
                    .traces
                    .get(&id)
                    .is_some_and(|s| s.generation == popped_generation);
                if live {
                    inner.traces.remove(&id);
                }
            }
            if inner.order.len() > self.capacity.saturating_mul(2) {
                let Inner { traces, order, .. } = &mut *inner;
                order.retain(|(id, generation)| {
                    traces.get(id).is_some_and(|s| s.generation == *generation)
                });
            }
        }
    }

    /// The retained traces that `findings` point at, newest flushes
    /// winning under `byte_budget` (their findings are the likeliest
    /// opened), returned in flush order. A trace too large for the
    /// remaining budget is skipped, not fatal: one oversized tree must
    /// not empty the whole snapshot. Sizes are measured on the stored
    /// value, only the kept traces pay a clone. Findings whose trace
    /// aged out are simply absent, which the dashboard reports as a
    /// tree missing from the embed.
    pub async fn snapshot_for(
        &self,
        findings: &[Finding],
        byte_budget: usize,
    ) -> Vec<EmbeddedTrace> {
        if self.capacity == 0 {
            return Vec::new();
        }
        let wanted: HashSet<&str> = findings.iter().map(|f| f.trace_id.as_str()).collect();
        let inner = self.inner.read().await;
        let mut spent = 0usize;
        let mut kept: Vec<EmbeddedTrace> = Vec::new();
        for (id, generation) in inner.order.iter().rev() {
            let Some(stored) = inner.traces.get(id) else {
                continue;
            };
            if stored.generation != *generation || !wanted.contains(id.as_str()) {
                continue;
            }
            let size = serde_json::to_string(&stored.trace).map_or(usize::MAX, |s| s.len());
            if spent.saturating_add(size) > byte_budget {
                continue;
            }
            spent += size;
            kept.push(stored.trace.clone());
        }
        kept.reverse();
        kept
    }

    /// Number of retained traces. Test-only: no occupancy gauge is
    /// wired for this store.
    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.inner.read().await.traces.len()
    }

    /// Whether the store holds no trace.
    #[cfg(test)]
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Union of two flushes' spans, the newer flush winning on a span id
/// collision, bounded by `max_spans` dropping the oldest spans first.
fn merge_spans(
    old: Vec<EmbeddedSpan>,
    new: Vec<EmbeddedSpan>,
    max_spans: usize,
) -> Vec<EmbeddedSpan> {
    let mut merged: Vec<EmbeddedSpan>;
    {
        let new_ids: HashSet<&str> = new.iter().map(|s| s.span_id.as_str()).collect();
        merged = old
            .into_iter()
            .filter(|s| !new_ids.contains(s.span_id.as_str()))
            .collect();
    }
    merged.extend(new);
    if merged.len() > max_spans {
        let excess = merged.len() - max_spans;
        merged.drain(..excess);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize::NormalizedEvent;

    /// A generous budget for tests that are not about the byte cap.
    const NO_BUDGET: usize = usize::MAX;

    fn span_event(trace_id: &str, span_id: &str) -> NormalizedEvent {
        NormalizedEvent {
            event: SpanEvent {
                timestamp: "2025-07-10T14:32:01.123Z".to_string(),
                trace_id: trace_id.to_string(),
                span_id: span_id.to_string(),
                parent_span_id: None,
                link_trace_id: None,
                service: "svc".into(),
                grouping: Vec::new(),
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
        }
    }

    fn trace(id: &str) -> Trace {
        Trace {
            trace_id: id.to_string(),
            spans: vec![span_event(id, "s1")],
        }
    }

    fn trace_with_spans(id: &str, span_ids: &[&str]) -> Trace {
        Trace {
            trace_id: id.to_string(),
            spans: span_ids.iter().map(|s| span_event(id, s)).collect(),
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
        let store = TracesStore::new(10, 1_000);
        store
            .retain_for(&[trace("a"), trace("b")], &[finding_on("a")])
            .await;
        assert_eq!(store.len().await, 1);
        let kept = store.snapshot_for(&[finding_on("a")], NO_BUDGET).await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].trace_id, "a");
    }

    #[tokio::test]
    async fn evicts_oldest_past_capacity() {
        let store = TracesStore::new(2, 1_000);
        for id in ["a", "b", "c"] {
            store.retain_for(&[trace(id)], &[finding_on(id)]).await;
        }
        assert_eq!(store.len().await, 2);
        assert!(
            store
                .snapshot_for(&[finding_on("a")], NO_BUDGET)
                .await
                .is_empty()
        );
        assert_eq!(
            store
                .snapshot_for(&[finding_on("c")], NO_BUDGET)
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn same_trace_twice_does_not_churn_the_buffer() {
        let store = TracesStore::new(2, 1_000);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        store.retain_for(&[trace("b")], &[finding_on("b")]).await;
        assert_eq!(store.len().await, 2);
        assert_eq!(
            store
                .snapshot_for(&[finding_on("a")], NO_BUDGET)
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_reflush_merges_spans_from_both_flushes() {
        // TTL split: the first flush carried the bulk of the tree, the
        // second only the stragglers. Findings from either flush point
        // at spans the other one lacks, so the union serves both.
        let store = TracesStore::new(2, 1_000);
        store
            .retain_for(
                &[trace_with_spans("a", &["s1", "s2", "s3"])],
                &[finding_on("a")],
            )
            .await;
        store
            .retain_for(&[trace_with_spans("a", &["s4"])], &[finding_on("a")])
            .await;
        let kept = store.snapshot_for(&[finding_on("a")], NO_BUDGET).await;
        assert_eq!(kept.len(), 1);
        let ids: Vec<&str> = kept[0].spans.iter().map(|s| s.span_id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s2", "s3", "s4"]);
    }

    #[tokio::test]
    async fn merged_spans_stay_bounded_by_the_per_trace_cap() {
        let store = TracesStore::new(2, 3);
        store
            .retain_for(
                &[trace_with_spans("a", &["s1", "s2", "s3"])],
                &[finding_on("a")],
            )
            .await;
        store
            .retain_for(&[trace_with_spans("a", &["s4", "s5"])], &[finding_on("a")])
            .await;
        let kept = store.snapshot_for(&[finding_on("a")], NO_BUDGET).await;
        let ids: Vec<&str> = kept[0].spans.iter().map(|s| s.span_id.as_str()).collect();
        assert_eq!(ids, vec!["s3", "s4", "s5"], "oldest spans drop first");
    }

    #[tokio::test]
    async fn a_reflush_refreshes_the_eviction_position() {
        // A re-flushed trace carries the freshest findings, it must not
        // keep its original slot and be evicted as the oldest.
        let store = TracesStore::new(2, 1_000);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        store.retain_for(&[trace("b")], &[finding_on("b")]).await;
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        store.retain_for(&[trace("c")], &[finding_on("c")]).await;
        assert!(
            store
                .snapshot_for(&[finding_on("b")], NO_BUDGET)
                .await
                .is_empty(),
            "b is now the oldest and must be the one evicted"
        );
        assert_eq!(
            store
                .snapshot_for(&[finding_on("a")], NO_BUDGET)
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn zero_capacity_retains_nothing() {
        let store = TracesStore::new(0, 1_000);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        assert!(store.is_empty().await);
        assert!(
            store
                .snapshot_for(&[finding_on("a")], NO_BUDGET)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn retained_spans_carry_the_template_not_the_literal() {
        let store = TracesStore::new(2, 1_000);
        store.retain_for(&[trace("a")], &[finding_on("a")]).await;
        let kept = store.snapshot_for(&[finding_on("a")], NO_BUDGET).await;
        assert_eq!(kept[0].spans[0].template, "select * from t where id = ?");
    }

    #[tokio::test]
    async fn byte_budget_skips_oversized_traces_instead_of_stopping() {
        // One oversized newest trace must not empty the snapshot: the
        // older traces that fit still ship.
        let store = TracesStore::new(10, 1_000);
        store
            .retain_for(&[trace("small-1")], &[finding_on("small-1")])
            .await;
        store
            .retain_for(&[trace("small-2")], &[finding_on("small-2")])
            .await;
        store
            .retain_for(
                &[trace_with_spans(
                    "huge",
                    &(0..200)
                        .map(|i| format!("s{i}"))
                        .collect::<Vec<_>>()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )],
                &[finding_on("huge")],
            )
            .await;
        let findings = [
            finding_on("small-1"),
            finding_on("small-2"),
            finding_on("huge"),
        ];
        let one_small = {
            let all = store.snapshot_for(&findings, usize::MAX).await;
            serde_json::to_string(&all[0]).unwrap().len()
        };
        let kept = store.snapshot_for(&findings, one_small * 2 + 1).await;
        let ids: Vec<&str> = kept.iter().map(|t| t.trace_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["small-1", "small-2"],
            "the oversized trace is skipped, the fitting ones survive"
        );
    }

    #[tokio::test]
    async fn byte_budget_prefers_the_newest_flushes() {
        let store = TracesStore::new(10, 1_000);
        for id in ["old", "mid", "new"] {
            store.retain_for(&[trace(id)], &[finding_on(id)]).await;
        }
        let findings = [finding_on("old"), finding_on("mid"), finding_on("new")];
        let one = {
            let all = store.snapshot_for(&findings, usize::MAX).await;
            serde_json::to_string(&all[0]).unwrap().len()
        };
        let kept = store.snapshot_for(&findings, one * 2 + 1).await;
        let ids: Vec<&str> = kept.iter().map(|t| t.trace_id.as_str()).collect();
        assert_eq!(ids, vec!["mid", "new"], "newest win, flush order preserved");
    }
}
