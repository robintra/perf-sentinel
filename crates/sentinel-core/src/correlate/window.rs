//! Sliding window correlator for streaming mode.
//!
//! Accumulates normalized events by `trace_id` with ring buffer, TTL eviction,
//! and O(1) LRU eviction when max active traces is exceeded.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use lru::LruCache;

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
        event: NormalizedEvent,
        now_ms: u64,
    ) -> Option<(String, Vec<NormalizedEvent>)> {
        // Fast path: trace already exists: get_mut auto-promotes to MRU.
        if let Some(buf) = self.traces.get_mut(event.event.trace_id.as_str()) {
            buf.last_seen_ms = now_ms;
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
                    last_seen_ms: now_ms,
                },
            )
            .map(|(id, buf)| (id, Vec::from(buf.events)))
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
            if let Some((_id, buf)) = self.traces.pop_entry(&key) {
                expired.push((key, Vec::from(buf.events)));
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
            result.push((id, Vec::from(buf.events)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventSource, EventType, SpanEvent};
    use crate::normalize;

    fn make_event(trace_id: &str, target: &str) -> NormalizedEvent {
        let event = SpanEvent {
            timestamp: "2025-07-10T14:32:01.123Z".to_string(),
            trace_id: trace_id.to_string(),
            span_id: "span-1".to_string(),
            parent_span_id: None,
            service: "test".to_string(),
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
}
