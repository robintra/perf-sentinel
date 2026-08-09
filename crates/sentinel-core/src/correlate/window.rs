//! Sliding window correlator for streaming mode.
//!
//! Accumulates normalized events by `trace_id` with ring buffer, TTL eviction,
//! and O(1) LRU eviction when max active traces is exceeded.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;

use crate::ingest::ANCESTOR_WALK_MAX_DEPTH;
use crate::normalize::NormalizedEvent;

/// Configuration for the trace window.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Maximum events kept per trace (ring buffer).
    pub max_events_per_trace: usize,
    /// Trace time-to-live in milliseconds.
    pub trace_ttl_ms: u64,
    /// Maximum number of active traces before LRU eviction. Must be >= 1.
    pub max_active_traces: NonZeroUsize,
}

/// Default LRU cap for the streaming correlator (compile-time non-zero).
const DEFAULT_MAX_ACTIVE_TRACES: NonZeroUsize =
    NonZeroUsize::new(10_000).expect("non-zero literal");

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            max_events_per_trace: 1000,
            trace_ttl_ms: 30_000,
            max_active_traces: DEFAULT_MAX_ACTIVE_TRACES,
        }
    }
}

/// Buffer for a single trace.
struct TraceBuffer {
    events: VecDeque<NormalizedEvent>,
    source_endpoint_groups: HashMap<Arc<str>, HashMap<String, String>>,
    source_endpoint_count: usize,
    /// Absolute timestamp (ms since epoch) of the last event pushed to this trace.
    /// Used for TTL eviction: the LRU cache handles relative access ordering.
    last_seen_ms: u64,
}

/// Sliding window that accumulates events by `trace_id`.
///
/// Uses an LRU cache for O(1) amortized eviction when at capacity.
pub struct TraceWindow {
    config: WindowConfig,
    traces: LruCache<String, TraceBuffer>,
}

impl TraceWindow {
    #[must_use]
    pub fn new(config: WindowConfig) -> Self {
        let cap = config.max_active_traces;
        Self {
            config,
            traces: LruCache::new(cap),
        }
    }

    /// Push a normalized event into the window.
    ///
    /// Returns the LRU-evicted trace (if any) so the caller can run detection
    /// on it before discarding. Returns `None` if no eviction was needed.
    pub fn push(
        &mut self,
        mut event: NormalizedEvent,
        now_ms: u64,
    ) -> Option<(String, Vec<NormalizedEvent>)> {
        // Fast path: trace already exists: get_mut auto-promotes to MRU.
        if let Some(buf) = self.traces.get_mut(event.event.trace_id.as_str()) {
            buf.last_seen_ms = now_ms;
            apply_direct_source_endpoint(&mut event, &buf.source_endpoint_groups);
            buf.events.push_back(event);
            // Ring buffer: drop oldest if over capacity
            if buf.events.len() > self.config.max_events_per_trace {
                buf.events.pop_front();
            }
            return None;
        }

        // Slow path: new trace, clone trace_id; push evicts LRU if at cap.
        let trace_id = event.event.trace_id.clone();
        let mut events = VecDeque::with_capacity(8);
        events.push_back(event);

        self.traces
            .push(
                trace_id,
                TraceBuffer {
                    events,
                    source_endpoint_groups: HashMap::new(),
                    source_endpoint_count: 0,
                    last_seen_ms: now_ms,
                },
            )
            .and_then(finish_trace_buffer)
    }

    /// Retain SERVER-root context even when no I/O event exists yet.
    ///
    /// A new context-only entry participates in the same LRU and TTL bounds as
    /// event-bearing traces. Updating an existing entry uses `peek_mut`, so it
    /// neither refreshes TTL nor promotes the trace.
    pub fn retain_source_endpoint_groups(
        &mut self,
        trace_id: &str,
        service_root_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
        now_ms: u64,
    ) -> Option<(String, Vec<NormalizedEvent>)> {
        if service_root_endpoints.is_empty() {
            return None;
        }
        let root_cap = self.config.max_events_per_trace;
        if let Some(buf) = self.traces.peek_mut(trace_id) {
            merge_source_endpoint_groups(buf, service_root_endpoints, root_cap);
            reconcile_trace_buffer(buf);
            return None;
        }

        let mut buf = TraceBuffer {
            events: VecDeque::new(),
            source_endpoint_groups: HashMap::new(),
            source_endpoint_count: 0,
            last_seen_ms: now_ms,
        };
        merge_source_endpoint_groups(&mut buf, service_root_endpoints, root_cap);
        if buf.source_endpoint_count == 0 {
            return None;
        }
        self.traces
            .push(trace_id.to_string(), buf)
            .and_then(finish_trace_buffer)
    }

    /// Fill unresolved source endpoints below active SERVER roots in one trace.
    ///
    /// Uses `peek_mut` so a context-only update neither extends the trace TTL
    /// nor promotes it in the LRU. An already-evicted trace stays evicted.
    /// Builds one parent index and scans each unresolved event once, with the
    /// same bounded ancestor walk as OTLP conversion. If a filtered non-I/O
    /// intermediary is absent, the endpoint stays unknown rather than being
    /// attributed to an unrelated root.
    pub fn reconcile_source_endpoint_groups(
        &mut self,
        trace_id: &str,
        service_root_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
    ) -> usize {
        if service_root_endpoints.is_empty() {
            return 0;
        }
        let Some(buf) = self.traces.peek_mut(trace_id) else {
            return 0;
        };
        reconcile_event_source_endpoint_groups(buf.events.make_contiguous(), service_root_endpoints)
    }

    /// Evict traces that have not been updated within the TTL.
    ///
    /// Scans the full LRU cache rather than stopping at the first non-expired
    /// entry, because clock adjustments (NTP) can cause `last_seen_ms` and LRU
    /// position to diverge, leaving expired traces behind non-expired ones.
    ///
    /// The key cloning into a temporary `Vec<String>` is required because
    /// the `lru` crate does not expose `retain()` or `drain_filter()`.
    /// At `max_active_traces = 10_000` the cost is bounded and runs at
    /// most once per tick (~15s). If the `lru` crate adds in-place removal
    /// in a future release, this can be simplified.
    pub fn evict(&mut self, now_ms: u64) {
        for key in self.collect_expired_keys(now_ms) {
            self.traces.pop(&key);
        }
    }

    /// Evict expired traces and return them for processing.
    ///
    /// Unlike `evict()` which silently drops expired traces, this method
    /// returns them so the daemon can run detection before discarding.
    /// Scans the full cache to handle clock skew (see `evict()`).
    pub fn evict_expired(&mut self, now_ms: u64) -> Vec<(String, Vec<NormalizedEvent>)> {
        let expired_keys = self.collect_expired_keys(now_ms);
        let mut expired = Vec::with_capacity(expired_keys.len());
        for key in expired_keys {
            if let Some(entry) = self.traces.pop_entry(&key).and_then(finish_trace_buffer) {
                expired.push(entry);
            }
        }
        expired
    }

    /// Collect trace IDs whose `last_seen_ms` is older than `trace_ttl_ms`.
    /// Shared by `evict()` and `evict_expired()`.
    fn collect_expired_keys(&self, now_ms: u64) -> Vec<String> {
        let ttl = self.config.trace_ttl_ms;
        self.traces
            .iter()
            .filter(|(_, buf)| now_ms.saturating_sub(buf.last_seen_ms) > ttl)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Drain all traces, returning their events grouped by `trace_id`.
    pub fn drain_all(&mut self) -> Vec<(String, Vec<NormalizedEvent>)> {
        let mut result = Vec::with_capacity(self.traces.len());
        while let Some((id, buf)) = self.traces.pop_lru() {
            if let Some(entry) = finish_trace_buffer((id, buf)) {
                result.push(entry);
            }
        }
        result
    }

    /// Number of active traces.
    #[must_use]
    pub fn active_traces(&self) -> usize {
        self.traces.len()
    }

    /// Clone a trace's spans without evicting or promoting it in the LRU.
    /// Returns `None` if the trace is not in the window.
    #[must_use]
    pub fn peek_clone(&self, trace_id: &str) -> Option<Vec<NormalizedEvent>> {
        self.traces
            .peek(trace_id)
            .map(|buf| buf.events.iter().cloned().collect())
    }
}

fn merge_source_endpoint_groups(
    buffer: &mut TraceBuffer,
    incoming: &HashMap<Arc<str>, HashMap<String, String>>,
    root_cap: usize,
) {
    for (service, roots) in incoming {
        for (root_span_id, endpoint) in roots {
            if let Some(existing) = buffer
                .source_endpoint_groups
                .get_mut(service)
                .and_then(|service_roots| service_roots.get_mut(root_span_id))
            {
                existing.clone_from(endpoint);
                continue;
            }
            if buffer.source_endpoint_count >= root_cap {
                continue;
            }
            buffer
                .source_endpoint_groups
                .entry(Arc::clone(service))
                .or_default()
                .insert(root_span_id.clone(), endpoint.clone());
            buffer.source_endpoint_count += 1;
        }
    }
}

/// Apply the common direct-child case without scanning the whole trace.
///
fn apply_direct_source_endpoint(
    event: &mut NormalizedEvent,
    source_endpoint_groups: &HashMap<Arc<str>, HashMap<String, String>>,
) {
    let source = event.event.source.endpoint.trim();
    if !source.is_empty() && source != "unknown" {
        return;
    }
    let Some(root_endpoints) = source_endpoint_groups.get(event.event.service.as_ref()) else {
        return;
    };
    let Some(parent_span_id) = event.event.parent_span_id.as_ref() else {
        return;
    };
    if let Some(endpoint) = root_endpoints.get(parent_span_id) {
        event.event.source.endpoint.clone_from(endpoint);
    }
}

fn reconcile_trace_buffer(buffer: &mut TraceBuffer) -> usize {
    reconcile_event_source_endpoint_groups(
        buffer.events.make_contiguous(),
        &buffer.source_endpoint_groups,
    )
}

fn finish_trace_buffer(
    (trace_id, mut buffer): (String, TraceBuffer),
) -> Option<(String, Vec<NormalizedEvent>)> {
    if !buffer.source_endpoint_groups.is_empty() {
        reconcile_trace_buffer(&mut buffer);
    }
    let events = Vec::from(buffer.events);
    (!events.is_empty()).then_some((trace_id, events))
}

/// Fill unresolved source endpoints in one trace's event slice.
pub(crate) fn reconcile_event_source_endpoint_groups(
    events: &mut [NormalizedEvent],
    service_root_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
) -> usize {
    let parents: HashMap<(&str, &str), Option<&str>> = events
        .iter()
        .map(|event| {
            (
                (event.event.service.as_ref(), event.event.span_id.as_str()),
                event.event.parent_span_id.as_deref(),
            )
        })
        .collect();
    let matching_events: Vec<(usize, &String)> = events
        .iter()
        .enumerate()
        .filter_map(|(event_index, event)| {
            let source = event.event.source.endpoint.trim();
            if !source.is_empty() && source != "unknown" {
                return None;
            }
            let service = event.event.service.as_ref();
            let root_endpoints = service_root_endpoints.get(service)?;
            parent_chain_source_endpoint(&parents, service, root_endpoints, event)
                .map(|endpoint| (event_index, endpoint))
        })
        .collect();
    drop(parents);
    let updated = matching_events.len();
    for (event_index, endpoint) in matching_events {
        events[event_index]
            .event
            .source
            .endpoint
            .clone_from(endpoint);
    }
    updated
}

fn parent_chain_source_endpoint<'a>(
    parents: &HashMap<(&str, &str), Option<&str>>,
    service: &str,
    root_endpoints: &'a HashMap<String, String>,
    event: &NormalizedEvent,
) -> Option<&'a String> {
    let mut parent_span_id = event.event.parent_span_id.as_deref();
    for _ in 0..ANCESTOR_WALK_MAX_DEPTH {
        let parent = parent_span_id?;
        if let Some(endpoint) = root_endpoints.get(parent) {
            return Some(endpoint);
        }
        parent_span_id = parents.get(&(service, parent)).copied().flatten();
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize;

    fn make_event(trace_id: &str, target: &str) -> NormalizedEvent {
        let event = SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            link_trace_id: None,
            service: Arc::from("test"),
            grouping: Vec::new(),
            cloud_region: None,
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
            response_size_bytes: None,
            code_function: None,
            code_filepath: None,
            code_lineno: None,
            code_namespace: None,
            instrumentation_scopes: Vec::new(),
        };
        normalize::normalize(event)
    }

    fn make_child(
        trace_id: &str,
        service: &str,
        span_id: &str,
        parent_span_id: &str,
        target: &str,
        endpoint: &str,
    ) -> NormalizedEvent {
        let mut event = make_event(trace_id, target);
        event.event.service = Arc::from(service);
        event.event.span_id = span_id.to_string();
        event.event.parent_span_id = Some(parent_span_id.to_string());
        event.event.source.endpoint = endpoint.to_string();
        event
    }

    fn push_unknown_chain(
        window: &mut TraceWindow,
        prefix: &str,
        root_span_id: &str,
        intermediate_count: usize,
        leaf_target: &str,
    ) {
        let mut parent = root_span_id.to_string();
        for depth in 1..=intermediate_count {
            let span_id = format!("{prefix}-{depth}");
            window.push(
                make_child("t1", "svc-a", &span_id, &parent, &span_id, "unknown"),
                0,
            );
            parent = span_id;
        }
        window.push(
            make_child(
                "t1",
                "svc-a",
                &format!("{prefix}-leaf"),
                &parent,
                leaf_target,
                "unknown",
            ),
            0,
        );
    }

    #[test]
    fn accumulates_events_by_trace() {
        let mut w = TraceWindow::new(WindowConfig::default());
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t1", "SELECT 2"), 10);
        w.push(make_event("t2", "SELECT 3"), 20);

        assert_eq!(w.active_traces(), 2);
        let drained = w.drain_all();
        let t1 = drained.iter().find(|(id, _)| id == "t1").unwrap();
        assert_eq!(t1.1.len(), 2);
    }

    #[test]
    fn ring_buffer_overflow() {
        let config = WindowConfig {
            max_events_per_trace: 3,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        for i in 0..5 {
            w.push(
                make_event("t1", &format!("SELECT {i}")),
                u64::try_from(i).unwrap(),
            );
        }

        let drained = w.drain_all();
        let t1 = drained.iter().find(|(id, _)| id == "t1").unwrap();
        assert_eq!(t1.1.len(), 3);
        // Should have the last 3 events (2, 3, 4)
        assert_eq!(t1.1[0].event.target, "SELECT 2");
        assert_eq!(t1.1[2].event.target, "SELECT 4");
    }

    #[test]
    fn ttl_eviction() {
        let config = WindowConfig {
            trace_ttl_ms: 100,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 50);

        w.evict(150);
        // t1 last_seen=0, now=150, diff=150 > 100 -> evicted
        // t2 last_seen=50, now=150, diff=100 -> NOT evicted (100 <= 100)
        assert_eq!(w.active_traces(), 1);
        let drained = w.drain_all();
        assert_eq!(drained[0].0, "t2");
    }

    #[test]
    fn lru_eviction() {
        let config = WindowConfig {
            max_active_traces: NonZeroUsize::new(2).unwrap(),
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 10);
        // This should evict t1 (LRU: oldest access)
        let evicted = w.push(make_event("t3", "SELECT 3"), 20);

        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().0, "t1");
        assert_eq!(w.active_traces(), 2);
        assert!(w.traces.peek(&"t2".to_string()).is_some());
        assert!(w.traces.peek(&"t3".to_string()).is_some());
        assert!(w.traces.peek(&"t1".to_string()).is_none());
    }

    #[test]
    fn drain_empties_window() {
        let mut w = TraceWindow::new(WindowConfig::default());
        w.push(make_event("t1", "SELECT 1"), 0);
        let drained = w.drain_all();
        assert_eq!(drained.len(), 1);
        assert_eq!(w.active_traces(), 0);
    }

    #[test]
    fn lru_touch_prevents_eviction() {
        let config = WindowConfig {
            max_active_traces: NonZeroUsize::new(2).unwrap(),
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 10);
        // Touch t1 so it becomes more recent than t2 (get_mut promotes to MRU)
        w.push(make_event("t1", "SELECT 1b"), 20);
        // Insert t3: should evict t2 (LRU), not t1 (MRU)
        let evicted = w.push(make_event("t3", "SELECT 3"), 30);

        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().0, "t2");
        assert_eq!(w.active_traces(), 2);
        assert!(w.traces.peek(&"t1".to_string()).is_some());
        assert!(w.traces.peek(&"t3".to_string()).is_some());
        assert!(w.traces.peek(&"t2".to_string()).is_none());
    }

    #[test]
    fn evict_on_empty_window() {
        let mut w = TraceWindow::new(WindowConfig::default());
        w.evict(1000);
        assert_eq!(w.active_traces(), 0);
    }

    #[test]
    fn ttl_evicts_all_expired() {
        let config = WindowConfig {
            trace_ttl_ms: 50,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 10);
        // Both expired at now=200
        w.evict(200);
        assert_eq!(w.active_traces(), 0);
    }

    #[test]
    fn drain_empty_window() {
        let mut w = TraceWindow::new(WindowConfig::default());
        let drained = w.drain_all();
        assert!(drained.is_empty());
    }

    #[test]
    fn lru_eviction_chain() {
        let config = WindowConfig {
            max_active_traces: NonZeroUsize::new(1).unwrap(),
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);

        let evicted1 = w.push(make_event("t1", "SELECT 1"), 0);
        assert!(evicted1.is_none()); // first insert, no eviction

        let evicted2 = w.push(make_event("t2", "SELECT 2"), 10);
        // t1 evicted, only t2 remains
        assert!(evicted2.is_some());
        assert_eq!(evicted2.unwrap().0, "t1");
        assert_eq!(w.active_traces(), 1);
        assert!(w.traces.peek(&"t2".to_string()).is_some());

        let evicted3 = w.push(make_event("t3", "SELECT 3"), 20);
        // t2 evicted, only t3 remains
        assert!(evicted3.is_some());
        assert_eq!(evicted3.unwrap().0, "t2");
        assert_eq!(w.active_traces(), 1);
        assert!(w.traces.peek(&"t3".to_string()).is_some());
    }

    #[test]
    fn evict_expired_returns_traces() {
        let config = WindowConfig {
            trace_ttl_ms: 100,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 50);

        // Not yet expired
        let expired = w.evict_expired(50);
        assert!(expired.is_empty());
        assert_eq!(w.active_traces(), 2);

        // t1 expired (150 - 0 = 150 > 100), t2 not (150 - 50 = 100 <= 100)
        let expired = w.evict_expired(150);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "t1");
        assert_eq!(w.active_traces(), 1);
    }

    #[test]
    fn push_returns_evicted_events() {
        let config = WindowConfig {
            max_active_traces: NonZeroUsize::new(1).unwrap(),
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t1", "SELECT 2"), 5);

        let evicted = w.push(make_event("t2", "SELECT 3"), 10);
        assert!(evicted.is_some());
        let (id, events) = evicted.unwrap();
        assert_eq!(id, "t1");
        assert_eq!(events.len(), 2); // both events from t1
    }

    #[test]
    fn reconciles_only_unknown_endpoints_in_the_same_trace_and_service() {
        let mut w = TraceWindow::new(WindowConfig::default());
        for (trace_id, service, endpoint, target) in [
            ("t1", "svc-a", "unknown", "SELECT 1"),
            ("t1", "svc-a", "  ", "SELECT 2"),
            ("t1", "svc-a", "/already-known", "SELECT 3"),
            ("t1", "svc-b", "unknown", "SELECT 4"),
            ("t2", "svc-a", "unknown", "SELECT 5"),
        ] {
            let mut event = make_event(trace_id, target);
            event.event.service = Arc::from(service);
            event.event.source.endpoint = endpoint.to_string();
            event.event.parent_span_id = Some("root-a".to_string());
            w.push(event, 0);
        }

        let root_endpoints = HashMap::from([(
            "root-a".to_string(),
            "/api/fault/slow-messaging".to_string(),
        )]);
        let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);
        assert_eq!(
            w.reconcile_source_endpoint_groups("t1", &service_root_endpoints),
            2
        );
        let t1 = w.peek_clone("t1").expect("trace remains active");
        let endpoint_for = |target: &str| {
            t1.iter()
                .find(|event| event.event.target == target)
                .expect("event present")
                .event
                .source
                .endpoint
                .as_str()
        };
        assert_eq!(endpoint_for("SELECT 1"), "/api/fault/slow-messaging");
        assert_eq!(endpoint_for("SELECT 2"), "/api/fault/slow-messaging");
        assert_eq!(endpoint_for("SELECT 3"), "/already-known");
        assert_eq!(endpoint_for("SELECT 4"), "unknown");
        assert_eq!(
            w.peek_clone("t2").expect("other trace remains")[0]
                .event
                .source
                .endpoint,
            "unknown"
        );
    }

    #[test]
    fn root_aware_reconciliation_is_order_independent_and_requires_a_known_parent_chain() {
        for updates in [
            [("root-a", "/api/a"), ("root-b", "/api/b")],
            [("root-b", "/api/b"), ("root-a", "/api/a")],
        ] {
            let mut w = TraceWindow::new(WindowConfig::default());
            for event in [
                make_child("t1", "svc-a", "a-direct", "root-a", "a-direct", "unknown"),
                make_child("t1", "svc-a", "a-mid", "root-a", "a-mid", "unknown"),
                make_child("t1", "svc-a", "a-leaf", "a-mid", "a-leaf", "unknown"),
                make_child("t1", "svc-a", "b-direct", "root-b", "b-direct", "unknown"),
                make_child(
                    "t1",
                    "svc-a",
                    "missing-chain",
                    "filtered-middle",
                    "missing-chain",
                    "unknown",
                ),
                make_child("t1", "svc-a", "known", "root-a", "known", "/already-known"),
                make_child(
                    "t1",
                    "svc-b",
                    "other-service",
                    "root-a",
                    "other-service",
                    "unknown",
                ),
            ] {
                w.push(event, 0);
            }

            let root_endpoints = updates
                .into_iter()
                .map(|(root_span_id, endpoint)| (root_span_id.to_string(), endpoint.to_string()))
                .collect();
            let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);
            w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

            let trace = w.peek_clone("t1").expect("trace remains active");
            let endpoint_for = |target: &str| {
                trace
                    .iter()
                    .find(|event| event.event.target == target)
                    .expect("event present")
                    .event
                    .source
                    .endpoint
                    .as_str()
            };
            assert_eq!(endpoint_for("a-direct"), "/api/a");
            assert_eq!(endpoint_for("a-mid"), "/api/a");
            assert_eq!(endpoint_for("a-leaf"), "/api/a");
            assert_eq!(endpoint_for("b-direct"), "/api/b");
            assert_eq!(endpoint_for("missing-chain"), "unknown");
            assert_eq!(endpoint_for("known"), "/already-known");
            assert_eq!(endpoint_for("other-service"), "unknown");
        }
    }

    #[test]
    fn grouped_reconciliation_bounds_depth_and_cycles() {
        let mut w = TraceWindow::new(WindowConfig::default());
        push_unknown_chain(&mut w, "within", "root-at-limit", 7, "within-leaf");
        push_unknown_chain(&mut w, "deep", "root-too-deep", 8, "deep-leaf");
        for index in 0..9 {
            let span_id = format!("cycle-{index}");
            let parent_span_id = format!("cycle-{}", (index + 1) % 9);
            w.push(
                make_child(
                    "t1",
                    "svc-a",
                    &span_id,
                    &parent_span_id,
                    &span_id,
                    "unknown",
                ),
                0,
            );
        }

        let root_endpoints = HashMap::from([
            ("root-at-limit".to_string(), "/at-limit".to_string()),
            ("root-too-deep".to_string(), "/too-deep".to_string()),
        ]);
        let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);
        w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

        let trace = w.peek_clone("t1").expect("trace remains active");
        let endpoint_for = |target: &str| {
            trace
                .iter()
                .find(|event| event.event.target == target)
                .expect("event present")
                .event
                .source
                .endpoint
                .as_str()
        };
        assert_eq!(endpoint_for("within-leaf"), "/at-limit");
        assert_eq!(endpoint_for("deep-leaf"), "unknown");
        for index in 0..9 {
            assert_eq!(endpoint_for(&format!("cycle-{index}")), "unknown");
        }
    }

    #[test]
    fn grouped_reconciliation_stays_bounded_under_many_unmatched_roots() {
        const ADVERSARIAL_SIZE: usize = 500;

        let mut w = TraceWindow::new(WindowConfig::default());
        push_unknown_chain(
            &mut w,
            "adversarial",
            "missing-parent",
            ADVERSARIAL_SIZE - 1,
            "adversarial-leaf",
        );
        let root_endpoints = (0..ADVERSARIAL_SIZE)
            .map(|index| (format!("absent-root-{index}"), format!("/root/{index}")))
            .collect();
        let service_root_endpoints = HashMap::from([(Arc::from("svc-a"), root_endpoints)]);

        let started = Instant::now();
        let updated = w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

        assert_eq!(updated, 0);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "group reconciliation exceeded the bounded-work budget"
        );
        assert!(
            w.peek_clone("t1")
                .expect("trace remains active")
                .iter()
                .all(|event| event.event.source.endpoint == "unknown")
        );
    }

    #[test]
    fn trace_group_reconciliation_keys_parents_and_roots_by_service() {
        let mut w = TraceWindow::new(WindowConfig::default());
        for event in [
            make_child(
                "t1",
                "svc-a",
                "shared-child",
                "shared-root",
                "a-child",
                "unknown",
            ),
            make_child(
                "t1",
                "svc-b",
                "shared-child",
                "shared-root",
                "b-child",
                "unknown",
            ),
            make_child(
                "t1",
                "svc-b",
                "known-child",
                "shared-root",
                "known-child",
                "/already-known",
            ),
        ] {
            w.push(event, 0);
        }
        let service_root_endpoints = HashMap::from([
            (
                Arc::from("svc-a"),
                HashMap::from([("shared-root".to_string(), "/api/a".to_string())]),
            ),
            (
                Arc::from("svc-b"),
                HashMap::from([("shared-root".to_string(), "/api/b".to_string())]),
            ),
        ]);

        assert_eq!(
            w.reconcile_source_endpoint_groups("t1", &service_root_endpoints),
            2
        );
        let trace = w.peek_clone("t1").expect("trace remains active");
        let endpoint_for = |target: &str| {
            trace
                .iter()
                .find(|event| event.event.target == target)
                .expect("event present")
                .event
                .source
                .endpoint
                .as_str()
        };
        assert_eq!(endpoint_for("a-child"), "/api/a");
        assert_eq!(endpoint_for("b-child"), "/api/b");
        assert_eq!(endpoint_for("known-child"), "/already-known");
    }

    #[test]
    fn trace_group_reconciliation_stays_bounded_across_many_services() {
        const SERVICE_COUNT: usize = 2_000;
        const EVENTS_PER_SERVICE: usize = 25;
        const EVENT_COUNT: usize = SERVICE_COUNT * EVENTS_PER_SERVICE;

        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: EVENT_COUNT,
            ..WindowConfig::default()
        });
        let mut service_root_endpoints = HashMap::with_capacity(SERVICE_COUNT);
        for service_index in 0..SERVICE_COUNT {
            let service = format!("service-{service_index}");
            service_root_endpoints.insert(
                Arc::from(service.as_str()),
                HashMap::from([(
                    format!("absent-root-{service_index}"),
                    format!("/api/{service_index}"),
                )]),
            );
            for event_index in 0..EVENTS_PER_SERVICE {
                w.push(
                    make_child(
                        "t1",
                        &service,
                        &format!("span-{service_index}-{event_index}"),
                        &format!("missing-parent-{service_index}-{event_index}"),
                        &format!("target-{service_index}-{event_index}"),
                        "unknown",
                    ),
                    0,
                );
            }
        }

        let started = Instant::now();
        let updated = w.reconcile_source_endpoint_groups("t1", &service_root_endpoints);

        assert_eq!(updated, 0);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "per-trace reconciliation exceeded the bounded-work budget"
        );
    }

    #[test]
    fn late_endpoint_does_not_resurrect_an_evicted_trace() {
        let mut w = TraceWindow::new(WindowConfig {
            max_active_traces: NonZeroUsize::new(1).expect("nonzero"),
            ..WindowConfig::default()
        });
        w.push(make_event("evicted", "SELECT 1"), 0);
        w.push(make_event("active", "SELECT 2"), 1);

        let root_endpoints = HashMap::from([("root".to_string(), "/api/late".to_string())]);
        let service_root_endpoints = HashMap::from([(Arc::from("test"), root_endpoints)]);
        assert_eq!(
            w.reconcile_source_endpoint_groups("evicted", &service_root_endpoints),
            0
        );
        assert_eq!(w.active_traces(), 1);
        assert!(w.peek_clone("evicted").is_none());
        assert!(w.peek_clone("active").is_some());
    }

    #[test]
    fn early_root_context_reconciles_later_multi_service_events_and_drains_io() {
        let mut w = TraceWindow::new(WindowConfig::default());
        let source_endpoint_groups = HashMap::from([
            (
                Arc::from("svc-a"),
                HashMap::from([
                    ("root-a".to_string(), "/api/a".to_string()),
                    ("root-a2".to_string(), "/api/a2".to_string()),
                ]),
            ),
            (
                Arc::from("svc-b"),
                HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
            ),
        ]);

        assert!(
            w.retain_source_endpoint_groups("t1", &source_endpoint_groups, 10)
                .is_none()
        );
        assert_eq!(w.active_traces(), 1);
        assert!(w.peek_clone("t1").expect("context retained").is_empty());
        assert!(
            w.push(make_child("t1", "svc-a", "a", "a-mid", "a", "unknown"), 20,)
                .is_none()
        );
        assert!(
            w.push(
                make_child("t1", "svc-a", "a-mid", "root-a", "a-mid", "unknown"),
                20,
            )
            .is_none()
        );
        assert!(
            w.push(
                make_child("t1", "svc-a", "a2", "root-a2", "a2", "unknown"),
                20,
            )
            .is_none()
        );
        assert!(
            w.push(make_child("t1", "svc-b", "b", "root-b", "b", "unknown"), 20,)
                .is_none()
        );

        let mut drained = w.drain_all();
        assert_eq!(drained.len(), 1, "only non-empty traces are drained");
        let (_, trace) = drained.pop().expect("trace finished");
        assert_eq!(trace[0].event.source.endpoint, "/api/a");
        assert_eq!(trace[1].event.source.endpoint, "/api/a");
        assert_eq!(trace[2].event.source.endpoint, "/api/a2");
        assert_eq!(trace[3].event.source.endpoint, "/api/b");
        assert_eq!(w.active_traces(), 0);
    }

    #[test]
    fn early_root_context_obeys_lru_without_resurrection_or_empty_eviction() {
        let mut w = TraceWindow::new(WindowConfig {
            max_active_traces: NonZeroUsize::new(2).expect("nonzero"),
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/root".to_string())]),
        )]);

        assert!(w.retain_source_endpoint_groups("a", &roots, 0).is_none());
        assert!(w.retain_source_endpoint_groups("b", &roots, 1).is_none());
        assert!(
            w.retain_source_endpoint_groups("a", &roots, 2).is_none(),
            "updating context neither evicts nor promotes"
        );
        assert!(
            w.retain_source_endpoint_groups("c", &roots, 3).is_none(),
            "evicting empty context produces no detection batch"
        );
        assert!(w.peek_clone("a").is_none(), "LRU context was evicted");
        assert!(w.peek_clone("b").is_some());
        assert!(w.peek_clone("c").is_some());

        assert!(
            w.push(
                make_child("a", "svc-a", "late", "root", "late", "unknown"),
                4,
            )
            .is_none()
        );
        assert_eq!(
            w.peek_clone("a").expect("new event trace retained")[0]
                .event
                .source
                .endpoint,
            "unknown",
            "evicted context must not resurrect"
        );
    }

    #[test]
    fn early_root_context_expires_without_an_empty_detection_batch() {
        let mut w = TraceWindow::new(WindowConfig {
            trace_ttl_ms: 100,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/root".to_string())]),
        )]);

        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        assert!(w.evict_expired(101).is_empty());
        assert_eq!(w.active_traces(), 0);
        w.push(
            make_child("t1", "svc-a", "late", "root", "late", "unknown"),
            102,
        );
        assert_eq!(
            w.peek_clone("t1").expect("new event trace retained")[0]
                .event
                .source
                .endpoint,
            "unknown"
        );
    }

    #[test]
    fn early_root_storm_is_bounded_by_trace_and_per_trace_caps() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 3,
            max_active_traces: NonZeroUsize::new(2).expect("nonzero"),
            ..WindowConfig::default()
        });
        for index in 0..100 {
            let roots = HashMap::from([(
                Arc::from("svc-a"),
                HashMap::from([(format!("root-{index}"), format!("/api/{index}"))]),
            )]);
            assert!(
                w.retain_source_endpoint_groups(&format!("trace-{index}"), &roots, index)
                    .is_none()
            );
        }
        let many_roots = HashMap::from([(
            Arc::from("svc-a"),
            (0..100)
                .map(|index| (format!("extra-root-{index}"), format!("/extra/{index}")))
                .collect(),
        )]);
        assert!(
            w.retain_source_endpoint_groups("trace-99", &many_roots, 100)
                .is_none()
        );

        assert_eq!(w.active_traces(), 2);
        assert!(
            w.traces
                .iter()
                .all(|(_, buffer)| buffer.source_endpoint_count <= 3)
        );
        assert!(w.drain_all().is_empty(), "context-only drain stays empty");
        assert_eq!(w.active_traces(), 0);
    }
}
