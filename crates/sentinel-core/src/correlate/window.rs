//! Sliding window correlator for streaming mode.
//!
//! Accumulates normalized events by `trace_id` with ring buffer, TTL eviction,
//! and LRU eviction when max active traces is exceeded.

use crate::normalize::NormalizedEvent;
use std::collections::{HashMap, VecDeque};

/// Configuration for the trace window.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Maximum events kept per trace (ring buffer).
    pub max_events_per_trace: usize,
    /// Trace time-to-live in milliseconds.
    pub trace_ttl_ms: u64,
    /// Maximum number of active traces before LRU eviction.
    pub max_active_traces: usize,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            max_events_per_trace: 1000,
            trace_ttl_ms: 30_000,
            max_active_traces: 10_000,
        }
    }
}

/// Buffer for a single trace.
#[derive(Debug)]
struct TraceBuffer {
    events: VecDeque<NormalizedEvent>,
    last_seen_ms: u64,
}

/// Sliding window that accumulates events by `trace_id`.
#[derive(Debug)]
pub struct TraceWindow {
    config: WindowConfig,
    traces: HashMap<String, TraceBuffer>,
}

impl TraceWindow {
    #[must_use]
    pub fn new(config: WindowConfig) -> Self {
        Self {
            config,
            traces: HashMap::new(),
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
        let trace_id = event.event.trace_id.clone();

        if let Some(buf) = self.traces.get_mut(&trace_id) {
            buf.last_seen_ms = now_ms;
            buf.events.push_back(event);
            // Ring buffer: drop oldest if over capacity
            if buf.events.len() > self.config.max_events_per_trace {
                buf.events.pop_front();
            }
            None
        } else {
            // Evict LRU trace before inserting if at capacity
            let evicted = if self.traces.len() >= self.config.max_active_traces {
                self.traces
                    .iter()
                    .min_by_key(|(_, buf)| buf.last_seen_ms)
                    .map(|(id, _)| id.clone())
                    .and_then(|oldest_id| {
                        self.traces
                            .remove(&oldest_id)
                            .map(|buf| (oldest_id, buf.events.into_iter().collect()))
                    })
            } else {
                None
            };

            let mut events = VecDeque::with_capacity(8);
            events.push_back(event);
            self.traces.insert(
                trace_id,
                TraceBuffer {
                    events,
                    last_seen_ms: now_ms,
                },
            );
            evicted
        }
    }

    /// Evict traces that have not been updated within the TTL.
    pub fn evict(&mut self, now_ms: u64) {
        let ttl = self.config.trace_ttl_ms;
        self.traces
            .retain(|_, buf| now_ms.saturating_sub(buf.last_seen_ms) <= ttl);
    }

    /// Evict expired traces and return them for processing.
    ///
    /// Unlike `evict()` which silently drops expired traces, this method
    /// returns them so the daemon can run detection before discarding.
    pub fn evict_expired(&mut self, now_ms: u64) -> Vec<(String, Vec<NormalizedEvent>)> {
        let ttl = self.config.trace_ttl_ms;
        let mut expired = Vec::new();
        self.traces.retain(|id, buf| {
            if now_ms.saturating_sub(buf.last_seen_ms) > ttl {
                expired.push((id.clone(), buf.events.drain(..).collect()));
                false
            } else {
                true
            }
        });
        expired
    }

    /// Drain all traces, returning their events grouped by `trace_id`.
    pub fn drain_all(&mut self) -> Vec<(String, Vec<NormalizedEvent>)> {
        self.traces
            .drain()
            .map(|(id, buf)| (id, buf.events.into_iter().collect()))
            .collect()
    }

    /// Number of active traces.
    #[must_use]
    pub fn active_traces(&self) -> usize {
        self.traces.len()
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
            service: "test".to_string(),
            event_type: EventType::Sql,
            operation: "SELECT".to_string(),
            target: target.to_string(),
            duration_us: 100,
            source: EventSource {
                endpoint: "GET /test".to_string(),
                method: "Test::test".to_string(),
            },
            status_code: None,
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
            max_active_traces: 2,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 10);
        // This should evict t1 (oldest last_seen_ms)
        w.push(make_event("t3", "SELECT 3"), 20);

        assert_eq!(w.active_traces(), 2);
        assert!(w.traces.contains_key("t2"));
        assert!(w.traces.contains_key("t3"));
        assert!(!w.traces.contains_key("t1"));
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
            max_active_traces: 2,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 10);
        // Touch t1 so it becomes more recent than t2
        w.push(make_event("t1", "SELECT 1b"), 20);
        // Insert t3 — should evict t2 (oldest last_seen_ms=10), not t1 (last_seen_ms=20)
        w.push(make_event("t3", "SELECT 3"), 30);

        assert_eq!(w.active_traces(), 2);
        assert!(w.traces.contains_key("t1"));
        assert!(w.traces.contains_key("t3"));
        assert!(!w.traces.contains_key("t2"));
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
            max_active_traces: 1,
            ..Default::default()
        };
        let mut w = TraceWindow::new(config);
        w.push(make_event("t1", "SELECT 1"), 0);
        w.push(make_event("t2", "SELECT 2"), 10);
        // t1 evicted, only t2 remains
        assert_eq!(w.active_traces(), 1);
        assert!(w.traces.contains_key("t2"));

        w.push(make_event("t3", "SELECT 3"), 20);
        // t2 evicted, only t3 remains
        assert_eq!(w.active_traces(), 1);
        assert!(w.traces.contains_key("t3"));
    }
}
