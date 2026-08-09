//! Sliding window correlator for streaming mode.
//!
//! Accumulates normalized events by `trace_id` with ring buffer, TTL eviction,
//! and O(1) LRU eviction when max active traces is exceeded.

use std::collections::{HashMap, HashSet, VecDeque};
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

#[derive(Clone)]
struct ResolvedEndpoint {
    endpoint: String,
    depth: usize,
}

#[derive(Clone)]
struct AncestryEntry {
    parent_span_id: Option<String>,
    resolution: Option<ResolvedEndpoint>,
}

/// Buffer for a single trace.
struct TraceBuffer {
    events: VecDeque<NormalizedEvent>,
    source_endpoint_groups: HashMap<Arc<str>, HashMap<String, String>>,
    /// Services for which a distinct root was observed after one was retained.
    /// Every entry therefore also exists in `source_endpoint_groups`.
    ambiguous_source_endpoint_services: HashSet<Arc<str>>,
    source_endpoint_count: usize,
    resolved_ancestry: Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
    needs_reconciliation: bool,
    source_endpoint_generation: u64,
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
    next_source_endpoint_generation: u64,
    #[cfg(all(test, feature = "daemon"))]
    reconciliation_passes: usize,
}

impl TraceWindow {
    #[must_use]
    pub fn new(config: WindowConfig) -> Self {
        let cap = config.max_active_traces;
        Self {
            config,
            traces: LruCache::new(cap),
            next_source_endpoint_generation: 0,
            #[cfg(all(test, feature = "daemon"))]
            reconciliation_passes: 0,
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
            resolve_and_index_event(
                &mut event,
                &buf.source_endpoint_groups,
                &mut buf.resolved_ancestry,
                buf.resolved_ancestry_cap,
            );
            buf.events.push_back(event);
            buf.needs_reconciliation = true;
            // Ring buffer: drop oldest if over capacity
            if buf.events.len() > self.config.max_events_per_trace {
                buf.events.pop_front();
            }
            return None;
        }

        // Slow path: new trace, clone trace_id; push evicts LRU if at cap.
        let trace_id = event.event.trace_id.clone();
        let mut buffer = new_trace_buffer(now_ms, self.config.max_events_per_trace);
        resolve_and_index_event(
            &mut event,
            &buffer.source_endpoint_groups,
            &mut buffer.resolved_ancestry,
            buffer.resolved_ancestry_cap,
        );
        buffer.events.push_back(event);
        buffer.needs_reconciliation = true;

        let evicted = self.traces.push(trace_id, buffer);
        #[cfg(all(test, feature = "daemon"))]
        if evicted
            .as_ref()
            .is_some_and(|(_, buffer)| buffer.needs_reconciliation)
        {
            self.reconciliation_passes += 1;
        }
        evicted.and_then(finish_trace_buffer)
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
        self.next_source_endpoint_generation = self.next_source_endpoint_generation.wrapping_add(1);
        let source_endpoint_generation = self.next_source_endpoint_generation;
        if let Some(buf) = self.traces.peek_mut(trace_id) {
            merge_source_endpoint_groups(buf, service_root_endpoints, root_cap);
            buf.source_endpoint_generation = source_endpoint_generation;
            #[cfg(all(test, feature = "daemon"))]
            {
                self.reconciliation_passes += 1;
            }
            reconcile_trace_buffer(buf);
            return None;
        }

        let mut buf = new_trace_buffer(now_ms, root_cap);
        merge_source_endpoint_groups(&mut buf, service_root_endpoints, root_cap);
        if buf.source_endpoint_count == 0 {
            return None;
        }
        buf.source_endpoint_generation = source_endpoint_generation;
        let evicted = self.traces.push(trace_id.to_string(), buf);
        #[cfg(all(test, feature = "daemon"))]
        if evicted
            .as_ref()
            .is_some_and(|(_, buffer)| buffer.needs_reconciliation)
        {
            self.reconciliation_passes += 1;
        }
        evicted.and_then(finish_trace_buffer)
    }

    /// Fill unresolved source endpoints below active SERVER roots in one trace.
    ///
    /// Uses `peek_mut` so a context-only update neither extends the trace TTL
    /// nor promotes it in the LRU. An already-evicted trace stays evicted.
    /// Builds one parent index and scans each unresolved event once, with the
    /// same bounded ancestor walk as OTLP conversion. A missing intermediary
    /// stays unknown when multiple roots could match. At the valid minimum
    /// ancestry cap of one, a sole retained root is the only safe fallback.
    pub fn reconcile_source_endpoint_groups(
        &mut self,
        trace_id: &str,
        service_root_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
    ) -> usize {
        if service_root_endpoints.is_empty() {
            return 0;
        }
        let root_cap = self.config.max_events_per_trace;
        let Some(buf) = self.traces.peek_mut(trace_id) else {
            return 0;
        };
        merge_source_endpoint_groups(buf, service_root_endpoints, root_cap);
        #[cfg(all(test, feature = "daemon"))]
        {
            self.reconciliation_passes += 1;
        }
        reconcile_trace_buffer(buf)
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

    #[cfg(feature = "daemon")]
    pub(crate) fn contains_trace(&self, trace_id: &str) -> bool {
        self.traces.peek(trace_id).is_some()
    }

    #[cfg(feature = "daemon")]
    pub(crate) fn source_endpoint_generation(&self, trace_id: &str) -> Option<u64> {
        self.traces
            .peek(trace_id)
            .map(|buffer| buffer.source_endpoint_generation)
    }

    #[cfg(all(test, feature = "daemon"))]
    pub(crate) fn reconciliation_passes(&self) -> usize {
        self.reconciliation_passes
    }

    /// Clone a trace's spans without evicting or promoting it in the LRU.
    /// Returns `None` if the trace is not in the window.
    #[must_use]
    pub fn peek_clone(&self, trace_id: &str) -> Option<Vec<NormalizedEvent>> {
        self.traces.peek(trace_id).map(|buf| {
            let mut events: Vec<_> = buf.events.iter().cloned().collect();
            reconcile_cloned_events(
                &mut events,
                &buf.source_endpoint_groups,
                &buf.ambiguous_source_endpoint_services,
                buf.resolved_ancestry.as_ref(),
                buf.resolved_ancestry_cap,
            );
            events
        })
    }
}

fn new_trace_buffer(now_ms: u64, per_trace_cap: usize) -> TraceBuffer {
    TraceBuffer {
        events: VecDeque::with_capacity(8),
        source_endpoint_groups: HashMap::new(),
        ambiguous_source_endpoint_services: HashSet::new(),
        source_endpoint_count: 0,
        resolved_ancestry: NonZeroUsize::new(per_trace_cap).map(|_| LruCache::unbounded()),
        resolved_ancestry_cap: per_trace_cap,
        needs_reconciliation: false,
        source_endpoint_generation: 0,
        last_seen_ms: now_ms,
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
            if buffer
                .source_endpoint_groups
                .get(service)
                .is_some_and(|retained_roots| !retained_roots.is_empty())
            {
                buffer
                    .ambiguous_source_endpoint_services
                    .insert(Arc::clone(service));
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

fn resolve_and_index_event(
    event: &mut NormalizedEvent,
    source_endpoint_groups: &HashMap<Arc<str>, HashMap<String, String>>,
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
) -> bool {
    let mut updated = false;
    let source_was_unknown = {
        let source = event.event.source.endpoint.trim();
        source.is_empty() || source == "unknown"
    };
    let own_root_endpoint = source_endpoint_groups
        .get(event.event.service.as_ref())
        .and_then(|roots| roots.get(&event.event.span_id));
    let parent_resolution = if own_root_endpoint.is_none() {
        resolve_parent_endpoint(
            &event.event.service,
            event.event.parent_span_id.as_deref(),
            source_endpoint_groups,
            resolved_ancestry,
        )
    } else {
        None
    };
    if source_was_unknown {
        if let Some(endpoint) = own_root_endpoint {
            event.event.source.endpoint.clone_from(endpoint);
            updated = true;
        } else if let Some(parent) = &parent_resolution
            && parent.depth < ANCESTOR_WALK_MAX_DEPTH
        {
            event.event.source.endpoint.clone_from(&parent.endpoint);
            updated = true;
        }
    }
    let source = event.event.source.endpoint.trim();
    let resolution = if !source.is_empty() && source != "unknown" {
        let depth = if own_root_endpoint.is_some() {
            0
        } else {
            parent_resolution.as_ref().map_or(0, |parent| {
                parent.depth.saturating_add(1).min(ANCESTOR_WALK_MAX_DEPTH)
            })
        };
        Some(ResolvedEndpoint {
            endpoint: event.event.source.endpoint.clone(),
            depth,
        })
    } else {
        None
    };
    cache_ancestry_entry(
        resolved_ancestry,
        resolved_ancestry_cap,
        (
            Arc::clone(&event.event.service),
            event.event.span_id.clone(),
        ),
        AncestryEntry {
            parent_span_id: event.event.parent_span_id.clone(),
            resolution,
        },
    );
    updated
}

fn reconcile_trace_buffer(buffer: &mut TraceBuffer) -> usize {
    let TraceBuffer {
        events,
        source_endpoint_groups,
        resolved_ancestry,
        resolved_ancestry_cap,
        ..
    } = buffer;
    let mut updated = 0;
    for event in events.iter_mut() {
        updated += usize::from(resolve_and_index_event(
            event,
            source_endpoint_groups,
            resolved_ancestry,
            *resolved_ancestry_cap,
        ));
    }
    buffer.needs_reconciliation = false;
    updated
}

fn resolve_parent_endpoint(
    service: &Arc<str>,
    parent_span_id: Option<&str>,
    source_endpoint_groups: &HashMap<Arc<str>, HashMap<String, String>>,
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
) -> Option<ResolvedEndpoint> {
    let roots = source_endpoint_groups.get(service.as_ref());
    let mut current_span_id = parent_span_id?.to_string();
    let mut traversed = Vec::new();

    for distance in 0..ANCESTOR_WALK_MAX_DEPTH {
        if let Some(endpoint) =
            roots.and_then(|root_endpoints| root_endpoints.get(&current_span_id))
        {
            compress_ancestry_path(resolved_ancestry, &traversed, endpoint, distance);
            return Some(ResolvedEndpoint {
                endpoint: endpoint.clone(),
                depth: distance,
            });
        }

        let key = (Arc::clone(service), current_span_id);
        let entry = resolved_ancestry
            .as_mut()
            .and_then(|ancestry| ancestry.get(&key))
            .cloned()?;
        if let Some(resolution) = entry.resolution {
            let depth = resolution.depth.saturating_add(distance);
            compress_ancestry_path(resolved_ancestry, &traversed, &resolution.endpoint, depth);
            return Some(ResolvedEndpoint {
                endpoint: resolution.endpoint,
                depth,
            });
        }
        traversed.push(key);
        let parent_span_id = entry.parent_span_id?;
        current_span_id = parent_span_id;
    }
    None
}

fn sole_root_endpoint(
    roots: Option<&HashMap<String, String>>,
    depth: usize,
    resolved_ancestry: Option<&LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
    source_endpoint_ambiguous: bool,
) -> Option<ResolvedEndpoint> {
    if source_endpoint_ambiguous || resolved_ancestry_cap != 1 || resolved_ancestry?.len() != 1 {
        return None;
    }
    let roots = roots?;
    if roots.len() != 1 {
        return None;
    }
    Some(ResolvedEndpoint {
        endpoint: roots.values().next()?.clone(),
        depth,
    })
}

fn compress_ancestry_path(
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    traversed: &[(Arc<str>, String)],
    endpoint: &str,
    immediate_parent_depth: usize,
) {
    let Some(ancestry) = resolved_ancestry else {
        return;
    };
    for (offset, key) in traversed.iter().enumerate() {
        if let Some(entry) = ancestry.get_mut(key) {
            entry.resolution = Some(ResolvedEndpoint {
                endpoint: endpoint.to_string(),
                depth: immediate_parent_depth
                    .saturating_sub(offset)
                    .min(ANCESTOR_WALK_MAX_DEPTH),
            });
        }
    }
    if let Some(parent_key) = traversed.first() {
        ancestry.get(parent_key);
    }
}

fn cache_ancestry_entry(
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    cap: usize,
    key: (Arc<str>, String),
    entry: AncestryEntry,
) {
    if let Some(ancestry) = resolved_ancestry {
        ancestry.put(key, entry);
        if ancestry.len() > cap {
            ancestry.pop_lru();
        }
    }
}

fn reconcile_cloned_events(
    events: &mut [NormalizedEvent],
    source_endpoint_groups: &HashMap<Arc<str>, HashMap<String, String>>,
    ambiguous_source_endpoint_services: &HashSet<Arc<str>>,
    resolved_ancestry: Option<&LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
) {
    for event in events.iter_mut() {
        let source = event.event.source.endpoint.trim();
        if !source.is_empty() && source != "unknown" {
            continue;
        }
        let Some(parent_span_id) = event.event.parent_span_id.as_ref() else {
            continue;
        };
        if let Some(parent) = peek_parent_endpoint(
            &event.event.service,
            parent_span_id,
            source_endpoint_groups,
            ambiguous_source_endpoint_services,
            resolved_ancestry,
            resolved_ancestry_cap,
        ) && parent.depth < ANCESTOR_WALK_MAX_DEPTH
        {
            event.event.source.endpoint = parent.endpoint;
        }
    }
    reconcile_event_source_endpoint_groups(events, source_endpoint_groups);
}

fn peek_parent_endpoint(
    service: &Arc<str>,
    parent_span_id: &str,
    source_endpoint_groups: &HashMap<Arc<str>, HashMap<String, String>>,
    ambiguous_source_endpoint_services: &HashSet<Arc<str>>,
    resolved_ancestry: Option<&LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
) -> Option<ResolvedEndpoint> {
    let roots = source_endpoint_groups.get(service.as_ref());
    let mut current_span_id = parent_span_id;
    for distance in 0..ANCESTOR_WALK_MAX_DEPTH {
        if let Some(endpoint) = roots.and_then(|root_endpoints| root_endpoints.get(current_span_id))
        {
            return Some(ResolvedEndpoint {
                endpoint: endpoint.clone(),
                depth: distance,
            });
        }
        let key = (Arc::clone(service), current_span_id.to_string());
        let Some(entry) = resolved_ancestry.and_then(|ancestry| ancestry.peek(&key)) else {
            return sole_root_endpoint(
                roots,
                distance,
                resolved_ancestry,
                resolved_ancestry_cap,
                ambiguous_source_endpoint_services.contains(service),
            );
        };
        if let Some(resolution) = &entry.resolution {
            return Some(ResolvedEndpoint {
                endpoint: resolution.endpoint.clone(),
                depth: resolution.depth.saturating_add(distance),
            });
        }
        let Some(parent_span_id) = entry.parent_span_id.as_deref() else {
            return sole_root_endpoint(
                roots,
                distance,
                resolved_ancestry,
                resolved_ancestry_cap,
                ambiguous_source_endpoint_services.contains(service),
            );
        };
        current_span_id = parent_span_id;
    }
    None
}

fn finish_trace_buffer(
    (trace_id, mut buffer): (String, TraceBuffer),
) -> Option<(String, Vec<NormalizedEvent>)> {
    if buffer.needs_reconciliation {
        reconcile_trace_buffer(&mut buffer);
    }
    let mut events = Vec::from(buffer.events);
    if buffer.resolved_ancestry_cap == 1 {
        reconcile_cloned_events(
            &mut events,
            &buffer.source_endpoint_groups,
            &buffer.ambiguous_source_endpoint_services,
            buffer.resolved_ancestry.as_ref(),
            buffer.resolved_ancestry_cap,
        );
    }
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
    if let Some(endpoint) = root_endpoints.get(&event.event.span_id) {
        return Some(endpoint);
    }
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
    fn root_first_context_resolves_the_event_with_the_same_span_id() {
        let mut w = TraceWindow::new(WindowConfig::default());
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        let mut root = make_event("t1", "http://orders-svc/api/orders");
        root.event.service = Arc::from("svc-a");
        root.event.span_id = "root".to_string();
        root.event.source.endpoint = "unknown".to_string();
        assert!(w.push(root, 1).is_none());

        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "/api/orders"
        );
        assert_eq!(
            w.drain_all().pop().expect("one finished trace").1[0]
                .event
                .source
                .endpoint,
            "/api/orders"
        );
    }

    #[test]
    fn detached_reconciliation_resolves_the_event_with_the_root_span_id() {
        let mut root = make_event("t1", "http://orders-svc/api/orders");
        root.event.service = Arc::from("svc-a");
        root.event.span_id = "root".to_string();
        root.event.source.endpoint = "unknown".to_string();
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);

        assert_eq!(
            reconcile_event_source_endpoint_groups(std::slice::from_mut(&mut root), &roots),
            1
        );
        assert_eq!(root.event.source.endpoint, "/api/orders");
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
    fn compressed_ancestry_keeps_every_hop_beyond_the_depth_limit_unknown() {
        let mut w = TraceWindow::new(WindowConfig::default());
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());

        let mut parent = "root".to_string();
        for depth in 1..=ANCESTOR_WALK_MAX_DEPTH {
            let span_id = format!("within-{depth}");
            w.push(
                make_child("t1", "svc-a", &span_id, &parent, &span_id, "unknown"),
                depth as u64,
            );
            parent = span_id;
        }
        w.push(make_child("t1", "svc-a", "a", "b", "a", "unknown"), 9);
        w.push(make_child("t1", "svc-a", "b", &parent, "b", "unknown"), 10);
        w.push(make_child("t1", "svc-a", "c", "a", "c", "unknown"), 11);
        w.push(make_child("t1", "svc-a", "d", "b", "d", "unknown"), 12);

        let trace = w.peek_clone("t1").expect("trace remains active");
        for span_id in ["a", "b", "c", "d"] {
            assert_eq!(
                trace
                    .iter()
                    .find(|event| event.event.span_id == span_id)
                    .expect("event retained")
                    .event
                    .source
                    .endpoint,
                "unknown",
                "{span_id} is beyond the ancestor walk limit"
            );
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
    fn resolved_ancestry_survives_ring_rotation_for_shared_children() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 3,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        w.push(
            make_child("t1", "svc-a", "intermediate", "root", "orders", "unknown"),
            1,
        );
        for index in 0..10 {
            w.push(
                make_child(
                    "t1",
                    "svc-a",
                    &format!("child-{index}"),
                    "intermediate",
                    "orders",
                    "unknown",
                ),
                index + 2,
            );
        }

        let preview = w.peek_clone("t1").expect("trace remains active");
        assert_eq!(preview.len(), 3, "ring retains only the newest children");
        assert!(
            preview
                .iter()
                .all(|event| event.event.source.endpoint == "/api/orders")
        );

        let (trace_id, spans) = w.drain_all().pop().expect("one finished trace");
        let findings =
            crate::detect::slow::detect_slow(&crate::correlate::Trace { trace_id, spans }, 0, 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_endpoint, "/api/orders");
    }

    #[test]
    fn capacity_one_ancestry_keeps_the_referenced_parent_for_siblings() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        w.push(
            make_child("t1", "svc-a", "parent", "root", "parent", "unknown"),
            1,
        );
        w.push(
            make_child("t1", "svc-a", "child-1", "parent", "child-1", "unknown"),
            2,
        );
        w.push(
            make_child("t1", "svc-a", "child-2", "parent", "child-2", "unknown"),
            3,
        );

        let preview = w.peek_clone("t1").expect("trace remains active");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].event.span_id, "child-2");
        assert_eq!(preview[0].event.source.endpoint, "/api/orders");

        let (_, events) = w.drain_all().pop().expect("one finished trace");
        assert_eq!(events[0].event.source.endpoint, "/api/orders");
    }

    #[test]
    fn capacity_one_never_falls_back_across_truncated_same_service_roots() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let first_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
        )]);
        let second_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &first_root, 0)
                .is_none()
        );
        assert!(
            w.retain_source_endpoint_groups("t1", &second_root, 1)
                .is_none()
        );
        w.push(
            make_child("t1", "svc-a", "parent", "root-b", "parent", "unknown"),
            2,
        );
        w.push(
            make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
            3,
        );

        let preview = w.peek_clone("t1").expect("trace remains active");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].event.source.endpoint, "unknown");

        let (trace_id, spans) = w.drain_all().pop().expect("one finished trace");
        assert_eq!(spans[0].event.source.endpoint, "unknown");
        let findings =
            crate::detect::slow::detect_slow(&crate::correlate::Trace { trace_id, spans }, 0, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_endpoint, "unknown");
    }

    #[test]
    fn late_second_root_retracts_capacity_one_preview_and_finished_fallback() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let first_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &first_root, 0)
                .is_none()
        );
        w.push(
            make_child("t1", "svc-a", "parent", "missing", "parent", "unknown"),
            1,
        );
        w.push(
            make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
            2,
        );
        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "/api/a",
            "one retained root permits a provisional preview fallback"
        );

        let second_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &second_root, 3)
                .is_none()
        );
        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "unknown"
        );
        let (trace_id, spans) = w.drain_all().pop().expect("one finished trace");
        assert_eq!(spans[0].event.source.endpoint, "unknown");
        let findings =
            crate::detect::slow::detect_slow(&crate::correlate::Trace { trace_id, spans }, 0, 1);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_endpoint, "unknown");
    }

    #[test]
    fn explicit_reconciliation_records_new_root_ambiguity_before_resolution() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let first_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
        )]);
        let second_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-b".to_string(), "/api/b".to_string())]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &first_root, 0)
                .is_none()
        );
        assert_eq!(w.reconcile_source_endpoint_groups("t1", &second_root), 0);
        assert!(
            w.traces
                .peek("t1")
                .expect("trace remains active")
                .ambiguous_source_endpoint_services
                .contains("svc-a")
        );

        w.push(
            make_child("t1", "svc-a", "parent", "root-b", "parent", "unknown"),
            1,
        );
        w.push(
            make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
            2,
        );
        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "unknown"
        );
    }

    #[test]
    fn duplicate_root_update_keeps_capacity_one_fallback_unambiguous() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let first_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/old".to_string())]),
        )]);
        let updated_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/current".to_string())]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &first_root, 0)
                .is_none()
        );
        assert!(
            w.retain_source_endpoint_groups("t1", &updated_root, 1)
                .is_none()
        );
        assert!(
            w.traces
                .peek("t1")
                .expect("trace remains active")
                .ambiguous_source_endpoint_services
                .is_empty(),
            "updating the same root must not make its service ambiguous"
        );

        w.push(
            make_child("t1", "svc-a", "parent", "root", "parent", "unknown"),
            2,
        );
        w.push(
            make_child("t1", "svc-a", "child-1", "parent", "child-1", "unknown"),
            3,
        );
        w.push(
            make_child("t1", "svc-a", "child-2", "parent", "child-2", "unknown"),
            4,
        );

        let preview = w.peek_clone("t1").expect("trace remains active");
        assert_eq!(preview[0].event.span_id, "child-2");
        assert_eq!(preview[0].event.source.endpoint, "/api/current");
    }

    #[test]
    fn same_batch_truncated_root_marks_only_its_retained_service_ambiguous() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([
                ("root-a".to_string(), "/api/a".to_string()),
                ("root-b".to_string(), "/api/b".to_string()),
            ]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        let retained_root = w
            .traces
            .peek("t1")
            .expect("trace remains active")
            .source_endpoint_groups["svc-a"]
            .keys()
            .next()
            .expect("one root retained")
            .clone();
        let dropped_root = if retained_root == "root-a" {
            "root-b"
        } else {
            "root-a"
        };
        assert!(
            w.traces
                .peek("t1")
                .expect("trace remains active")
                .ambiguous_source_endpoint_services
                .contains("svc-a")
        );

        w.push(
            make_child("t1", "svc-a", "parent", dropped_root, "parent", "unknown"),
            1,
        );
        w.push(
            make_child("t1", "svc-a", "child", "parent", "child", "unknown"),
            2,
        );
        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "unknown"
        );
    }

    #[test]
    fn rejected_other_service_does_not_consume_ambiguity_state() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 1,
            ..WindowConfig::default()
        });
        let first_root = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root-a".to_string(), "/api/a".to_string())]),
        )]);
        assert!(
            w.retain_source_endpoint_groups("t1", &first_root, 0)
                .is_none()
        );
        for index in 0..100 {
            let rejected_root = HashMap::from([(
                Arc::from(format!("svc-rejected-{index}")),
                HashMap::from([(format!("root-{index}"), format!("/api/{index}"))]),
            )]);
            assert!(
                w.retain_source_endpoint_groups("t1", &rejected_root, index + 1)
                    .is_none()
            );
        }
        let buffer = w.traces.peek("t1").expect("trace remains active");
        assert!(buffer.ambiguous_source_endpoint_services.is_empty());
        assert_eq!(buffer.source_endpoint_groups.len(), 1);

        w.push(
            make_child(
                "t1",
                "svc-rejected-0",
                "rejected-parent",
                "root-0",
                "rejected-parent",
                "unknown",
            ),
            101,
        );
        w.push(
            make_child(
                "t1",
                "svc-rejected-0",
                "rejected-child",
                "rejected-parent",
                "rejected-child",
                "unknown",
            ),
            102,
        );
        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "unknown",
            "a rejected service must not inherit the retained service's root"
        );

        w.push(
            make_child("t1", "svc-a", "parent", "root-a", "parent", "unknown"),
            103,
        );
        w.push(
            make_child("t1", "svc-a", "child-1", "parent", "child-1", "unknown"),
            104,
        );
        w.push(
            make_child("t1", "svc-a", "child-2", "parent", "child-2", "unknown"),
            105,
        );
        assert_eq!(
            w.peek_clone("t1").expect("trace remains active")[0]
                .event
                .source
                .endpoint,
            "/api/a"
        );
    }

    #[test]
    fn resolved_ancestry_survives_out_of_order_parent_rotation() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 2,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        w.push(make_child("t1", "svc-a", "a", "b", "a", "unknown"), 1);
        w.push(make_child("t1", "svc-a", "b", "root", "b", "unknown"), 2);
        w.push(make_child("t1", "svc-a", "c", "a", "c", "unknown"), 3);

        let preview = w.peek_clone("t1").expect("trace remains active");
        let c = preview
            .iter()
            .find(|event| event.event.span_id == "c")
            .expect("newest child remains in the ring");
        assert_eq!(c.event.source.endpoint, "/api/orders");

        let (_, events) = w.drain_all().pop().expect("one finished trace");
        let c = events
            .iter()
            .find(|event| event.event.span_id == "c")
            .expect("newest child is drained");
        assert_eq!(c.event.source.endpoint, "/api/orders");
    }

    #[test]
    fn ancestry_cache_does_not_preallocate_the_per_trace_limit() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 100_000,
            ..WindowConfig::default()
        });
        w.push(make_event("t1", "SELECT 1"), 0);

        let buffer = w.traces.peek("t1").expect("trace remains active");
        let ancestry = buffer
            .resolved_ancestry
            .as_ref()
            .expect("positive cap enables ancestry retention");
        assert_eq!(ancestry.cap(), NonZeroUsize::MAX);
        assert_eq!(ancestry.len(), 1);
    }

    #[test]
    fn resolved_ancestry_isolated_by_service_when_span_ids_collide() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 4,
            ..WindowConfig::default()
        });
        let roots = HashMap::from([
            (
                Arc::from("svc-a"),
                HashMap::from([("root".to_string(), "/api/a".to_string())]),
            ),
            (
                Arc::from("svc-b"),
                HashMap::from([("root".to_string(), "/api/b".to_string())]),
            ),
        ]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 0).is_none());
        w.push(
            make_child("t1", "svc-a", "shared", "root", "a-parent", "unknown"),
            1,
        );
        w.push(
            make_child("t1", "svc-b", "shared", "root", "b-parent", "unknown"),
            1,
        );
        w.push(
            make_child("t1", "svc-a", "a-child", "shared", "a-child", "unknown"),
            2,
        );
        w.push(
            make_child("t1", "svc-b", "b-child", "shared", "b-child", "unknown"),
            2,
        );

        let preview = w.peek_clone("t1").expect("trace remains active");
        let endpoint_for = |target: &str| {
            preview
                .iter()
                .find(|event| event.event.target == target)
                .expect("event retained")
                .event
                .source
                .endpoint
                .as_str()
        };
        assert_eq!(endpoint_for("a-child"), "/api/a");
        assert_eq!(endpoint_for("b-child"), "/api/b");
    }

    #[test]
    fn root_update_feeds_resolved_ancestry_before_parent_rotation() {
        let mut w = TraceWindow::new(WindowConfig {
            max_events_per_trace: 3,
            ..WindowConfig::default()
        });
        w.push(
            make_child("t1", "svc-a", "intermediate", "root", "orders", "unknown"),
            0,
        );
        w.push(
            make_child("t1", "svc-a", "early", "intermediate", "orders", "unknown"),
            1,
        );
        let roots = HashMap::from([(
            Arc::from("svc-a"),
            HashMap::from([("root".to_string(), "/api/orders".to_string())]),
        )]);
        assert!(w.retain_source_endpoint_groups("t1", &roots, 2).is_none());
        for index in 0..10 {
            w.push(
                make_child(
                    "t1",
                    "svc-a",
                    &format!("late-{index}"),
                    "intermediate",
                    "orders",
                    "unknown",
                ),
                index + 3,
            );
        }

        assert!(
            w.peek_clone("t1")
                .expect("trace remains active")
                .iter()
                .all(|event| event.event.source.endpoint == "/api/orders")
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
        assert!(w.traces.iter().all(|(_, buffer)| {
            buffer
                .resolved_ancestry
                .as_ref()
                .is_none_or(|ancestry| ancestry.len() <= 3)
        }));
        assert!(w.traces.iter().all(|(_, buffer)| {
            buffer.ambiguous_source_endpoint_services.len() <= buffer.source_endpoint_groups.len()
                && buffer
                    .ambiguous_source_endpoint_services
                    .iter()
                    .all(|service| buffer.source_endpoint_groups.contains_key(service))
        }));
        assert!(w.drain_all().is_empty(), "context-only drain stays empty");
        assert_eq!(w.active_traces(), 0);
    }
}
