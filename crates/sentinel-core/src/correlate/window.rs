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
    /// Retained context proved it: a route root, or a chain reaching one.
    /// An endpoint the converter spelled on an event and no route confirmed
    /// (a code frame, a consumer destination) is cached unproven: it names
    /// what nothing proven does, and never outranks a nearer route.
    proven: bool,
}

#[derive(Clone)]
struct AncestryEntry {
    parent_span_id: Option<String>,
    resolution: Option<ResolvedEndpoint>,
}

type SourceEndpointGroups = HashMap<Arc<str>, HashMap<String, String>>;
type SourceEndpointParentGroups = HashMap<Arc<str>, HashMap<String, Option<String>>>;

/// One trace's retained endpoint context, borrowed for a walk. Named fields,
/// so the two same-typed maps cannot swap places at a call site.
#[derive(Clone, Copy)]
struct SourceContext<'a> {
    roots: &'a SourceEndpointGroups,
    parents: &'a SourceEndpointParentGroups,
    consumers: &'a SourceEndpointGroups,
}

/// Buffer for a single trace.
struct TraceBuffer {
    events: VecDeque<NormalizedEvent>,
    source_endpoint_groups: SourceEndpointGroups,
    source_endpoint_parent_groups: SourceEndpointParentGroups,
    /// CONSUMER destinations by service and span id, apart from the roots: an
    /// ancestor walk reads the nearest one only where nothing else answers.
    source_consumer_groups: SourceEndpointGroups,
    /// Services for which a distinct root was observed after one was retained.
    /// Every entry therefore also exists in `source_endpoint_groups`.
    ambiguous_source_endpoint_services: HashSet<Arc<str>>,
    source_endpoint_count: usize,
    source_consumer_count: usize,
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
        // Fast path: trace already exists (get_mut auto-promotes to MRU).
        if let Some(buf) = self.traces.get_mut(event.event.trace_id.as_str()) {
            buf.last_seen_ms = now_ms;
            resolve_and_index_event(
                &mut event,
                SourceContext {
                    roots: &buf.source_endpoint_groups,
                    parents: &buf.source_endpoint_parent_groups,
                    consumers: &buf.source_consumer_groups,
                },
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

        // Slow path: new trace, clone trace_id. `push` evicts LRU if at cap.
        let trace_id = event.event.trace_id.clone();
        let mut buffer = new_trace_buffer(now_ms, self.config.max_events_per_trace);
        resolve_and_index_event(
            &mut event,
            SourceContext {
                roots: &buffer.source_endpoint_groups,
                parents: &buffer.source_endpoint_parent_groups,
                consumers: &buffer.source_consumer_groups,
            },
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

    /// Retain endpoint and intermediate-parent context even when no I/O event
    /// exists yet.
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
        self.retain_source_endpoint_context_groups(
            trace_id,
            service_root_endpoints,
            &HashMap::new(),
            &HashMap::new(),
            now_ms,
        )
    }

    pub(crate) fn retain_source_endpoint_context_groups(
        &mut self,
        trace_id: &str,
        service_root_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
        service_root_parents: &SourceEndpointParentGroups,
        service_consumer_endpoints: &HashMap<Arc<str>, HashMap<String, String>>,
        now_ms: u64,
    ) -> Option<(String, Vec<NormalizedEvent>)> {
        if service_root_endpoints.is_empty()
            && service_root_parents.is_empty()
            && service_consumer_endpoints.is_empty()
        {
            return None;
        }
        let root_cap = self.config.max_events_per_trace;
        self.next_source_endpoint_generation = self.next_source_endpoint_generation.wrapping_add(1);
        let source_endpoint_generation = self.next_source_endpoint_generation;
        if let Some(buf) = self.traces.peek_mut(trace_id) {
            merge_source_endpoint_groups(
                buf,
                service_root_endpoints,
                service_root_parents,
                service_consumer_endpoints,
                root_cap,
            );
            buf.source_endpoint_generation = source_endpoint_generation;
            #[cfg(all(test, feature = "daemon"))]
            {
                self.reconciliation_passes += 1;
            }
            reconcile_trace_buffer(buf);
            return None;
        }

        let mut buf = new_trace_buffer(now_ms, root_cap);
        merge_source_endpoint_groups(
            &mut buf,
            service_root_endpoints,
            service_root_parents,
            service_consumer_endpoints,
            root_cap,
        );
        if buf.source_endpoint_count == 0
            && buf.source_consumer_count == 0
            && buf
                .resolved_ancestry
                .as_ref()
                .is_none_or(LruCache::is_empty)
        {
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

    /// Fill unresolved source endpoints below retained entry spans in one trace.
    ///
    /// Uses `peek_mut` so a context-only update neither extends the trace TTL
    /// nor promotes it in the LRU. An already-evicted trace stays evicted.
    /// Builds one parent index and scans each unresolved event once, with the
    /// same bounded ancestor walk as OTLP conversion. A missing intermediary
    /// stays unknown when multiple roots could match. At the valid minimum
    /// ancestry cap of one, a sole retained root is the only safe fallback,
    /// and none once a consumer destination is retained beside it.
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
        merge_source_endpoint_groups(
            buf,
            service_root_endpoints,
            &HashMap::new(),
            &HashMap::new(),
            root_cap,
        );
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
                SourceContext {
                    roots: &buf.source_endpoint_groups,
                    parents: &buf.source_endpoint_parent_groups,
                    consumers: &buf.source_consumer_groups,
                },
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
        source_endpoint_parent_groups: HashMap::new(),
        source_consumer_groups: HashMap::new(),
        ambiguous_source_endpoint_services: HashSet::new(),
        source_endpoint_count: 0,
        source_consumer_count: 0,
        resolved_ancestry: NonZeroUsize::new(per_trace_cap).map(|_| LruCache::unbounded()),
        resolved_ancestry_cap: per_trace_cap,
        needs_reconciliation: false,
        source_endpoint_generation: 0,
        last_seen_ms: now_ms,
    }
}

/// Carry the batch's parent link for one root, on both the update and the
/// insert path of [`merge_source_endpoint_groups`].
fn carry_incoming_parent(
    buffer: &mut TraceBuffer,
    incoming_parents: &SourceEndpointParentGroups,
    service: &Arc<str>,
    root_span_id: &str,
) {
    if let Some(parent) = incoming_parents
        .get(service)
        .and_then(|service_parents| service_parents.get(root_span_id))
    {
        buffer
            .source_endpoint_parent_groups
            .entry(Arc::clone(service))
            .or_default()
            .insert(root_span_id.to_string(), parent.clone());
    }
}

/// Every `(service, span id, value)` of a batch's groups, in that order, so
/// what the caps admit and the ancestry LRU keeps does not follow the maps'
/// hash order.
fn by_service_and_span<T>(
    groups: &HashMap<Arc<str>, HashMap<String, T>>,
) -> Vec<(&Arc<str>, &String, &T)> {
    let mut entries: Vec<_> = groups
        .iter()
        .flat_map(|(service, items)| items.iter().map(move |(span_id, v)| (service, span_id, v)))
        .collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0).then_with(|| a.1.cmp(b.1)));
    entries
}

fn merge_source_endpoint_groups(
    buffer: &mut TraceBuffer,
    incoming: &HashMap<Arc<str>, HashMap<String, String>>,
    incoming_parents: &SourceEndpointParentGroups,
    incoming_consumers: &SourceEndpointGroups,
    root_cap: usize,
) {
    index_incoming_ancestry(buffer, incoming_parents);
    merge_incoming_roots(buffer, incoming, incoming_parents, root_cap);
    merge_incoming_consumers(buffer, incoming_consumers, incoming_parents, root_cap);
}

/// Fold the batch's consumer destinations in under a cap of their own, so
/// they neither crowd out a route root nor make a service ambiguous. Their
/// parent links are carried like a root's, so they outlive the ancestry LRU.
fn merge_incoming_consumers(
    buffer: &mut TraceBuffer,
    incoming: &SourceEndpointGroups,
    incoming_parents: &SourceEndpointParentGroups,
    root_cap: usize,
) {
    for (service, span_id, endpoint) in by_service_and_span(incoming) {
        if let Some(existing) = buffer
            .source_consumer_groups
            .get_mut(service)
            .and_then(|retained| retained.get_mut(span_id))
        {
            existing.clone_from(endpoint);
            carry_incoming_parent(buffer, incoming_parents, service, span_id);
            continue;
        }
        if buffer.source_consumer_count >= root_cap {
            continue;
        }
        buffer
            .source_consumer_groups
            .entry(Arc::clone(service))
            .or_default()
            .insert(span_id.clone(), endpoint.clone());
        carry_incoming_parent(buffer, incoming_parents, service, span_id);
        buffer.source_consumer_count += 1;
    }
}

/// Record the batch's parent links in the ancestry cache, refreshing an
/// entry already there rather than evicting it.
fn index_incoming_ancestry(
    buffer: &mut TraceBuffer,
    incoming_parents: &SourceEndpointParentGroups,
) {
    for (service, span_id, parent_span_id) in by_service_and_span(incoming_parents) {
        let key = (Arc::clone(service), span_id.clone());
        if let Some(entry) = buffer
            .resolved_ancestry
            .as_mut()
            .and_then(|ancestry| ancestry.get_mut(&key))
        {
            entry.parent_span_id.clone_from(parent_span_id);
        } else {
            cache_ancestry_entry(
                &mut buffer.resolved_ancestry,
                buffer.resolved_ancestry_cap,
                key,
                AncestryEntry {
                    parent_span_id: parent_span_id.clone(),
                    resolution: None,
                },
            );
        }
    }
}

/// Fold the batch's roots into the retained ones, under `root_cap`. A
/// service that already holds a different root becomes ambiguous.
fn merge_incoming_roots(
    buffer: &mut TraceBuffer,
    incoming: &HashMap<Arc<str>, HashMap<String, String>>,
    incoming_parents: &SourceEndpointParentGroups,
    root_cap: usize,
) {
    for (service, root_span_id, endpoint) in by_service_and_span(incoming) {
        if let Some(existing) = buffer
            .source_endpoint_groups
            .get_mut(service)
            .and_then(|service_roots| service_roots.get_mut(root_span_id))
        {
            existing.clone_from(endpoint);
            carry_incoming_parent(buffer, incoming_parents, service, root_span_id);
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
        carry_incoming_parent(buffer, incoming_parents, service, root_span_id);
        buffer.source_endpoint_count += 1;
    }
}

fn resolve_and_index_event(
    event: &mut NormalizedEvent,
    context: SourceContext<'_>,
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
) -> bool {
    let mut updated = false;
    let source = event.event.source.endpoint.trim();
    let source_was_unknown = source.is_empty() || source == "unknown";
    let service_roots = context.roots.get(event.event.service.as_ref());
    let own_root_endpoint = service_roots.and_then(|roots| roots.get(&event.event.span_id));
    let parent_resolution = if own_root_endpoint.is_none() {
        resolve_parent_endpoint(
            &event.event.service,
            event.event.parent_span_id.as_deref(),
            context,
            resolved_ancestry,
            source,
        )
    } else {
        None
    };
    let reconciled_endpoint = own_root_endpoint.or_else(|| {
        parent_resolution
            .as_ref()
            .filter(|(parent, _)| parent.depth < ANCESTOR_WALK_MAX_DEPTH)
            .map(|(parent, _)| &parent.endpoint)
    });
    if (source_was_unknown
        || own_root_endpoint.is_some_and(|endpoint| endpoint == source)
        || parent_resolution
            .as_ref()
            .is_some_and(|(_, matches_source)| *matches_source))
        && let Some(endpoint) = reconciled_endpoint
        && event.event.source.endpoint != *endpoint
    {
        event.event.source.endpoint.clone_from(endpoint);
        updated = true;
    }
    let source = event.event.source.endpoint.trim();
    // The chain resolution the event now carries, if any: its depth counts
    // from there, and a proven one proves the event. An endpoint kept from
    // the converter sits at depth 0 and stays unproven.
    let adopted = parent_resolution
        .as_ref()
        .filter(|(parent, _)| parent.endpoint == source);
    let resolution = (!source.is_empty() && source != "unknown").then(|| ResolvedEndpoint {
        endpoint: event.event.source.endpoint.clone(),
        depth: adopted.map_or(0, |(parent, _)| {
            parent.depth.saturating_add(1).min(ANCESTOR_WALK_MAX_DEPTH)
        }),
        proven: own_root_endpoint.is_some_and(|endpoint| endpoint == source)
            || adopted.is_some_and(|(parent, _)| parent.proven),
    });
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
        source_endpoint_parent_groups,
        source_consumer_groups,
        resolved_ancestry,
        resolved_ancestry_cap,
        ..
    } = buffer;
    let context = SourceContext {
        roots: source_endpoint_groups,
        parents: source_endpoint_parent_groups,
        consumers: source_consumer_groups,
    };
    let mut updated = 0;
    for event in events.iter_mut() {
        updated += usize::from(resolve_and_index_event(
            event,
            context,
            resolved_ancestry,
            *resolved_ancestry_cap,
        ));
    }
    buffer.needs_reconciliation = false;
    updated
}

/// Keeps the first consumer met on the walk. An endpoint equal to the
/// nearest destination came from it, so any route or resolved ancestor
/// further out still replaces it.
fn note_nearest_consumer(
    nearest_consumer: &mut Option<ResolvedEndpoint>,
    matches_source: &mut bool,
    consumers: Option<&HashMap<String, String>>,
    span_id: &str,
    distance: usize,
    source: &str,
) {
    if nearest_consumer.is_none()
        && let Some(endpoint) = consumers.and_then(|entries| entries.get(span_id))
    {
        *matches_source |= endpoint == source;
        *nearest_consumer = Some(ResolvedEndpoint {
            endpoint: endpoint.clone(),
            depth: distance,
            proven: false,
        });
    }
}

fn resolve_parent_endpoint(
    service: &Arc<str>,
    parent_span_id: Option<&str>,
    context: SourceContext<'_>,
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    source: &str,
) -> Option<(ResolvedEndpoint, bool)> {
    let roots = context.roots.get(service.as_ref());
    let root_parents = context.parents.get(service.as_ref());
    let consumers = context.consumers.get(service.as_ref());
    let mut current_span_id = parent_span_id?.to_string();
    let mut traversed = Vec::new();
    let mut outermost = None;
    // Entries walked before `outermost` was found sit below it. Those above
    // it serve their own branches and must not be stamped with its endpoint.
    let mut below_outermost = 0;
    let mut nearest_consumer = None;
    let mut matches_source = false;

    for distance in 0..ANCESTOR_WALK_MAX_DEPTH {
        note_nearest_consumer(
            &mut nearest_consumer,
            &mut matches_source,
            consumers,
            &current_span_id,
            distance,
            source,
        );
        if let Some(endpoint) =
            roots.and_then(|root_endpoints| root_endpoints.get(&current_span_id))
        {
            matches_source |= endpoint == source;
            outermost = Some(ResolvedEndpoint {
                endpoint: endpoint.clone(),
                depth: distance,
                proven: true,
            });
            below_outermost = traversed.len();
            let Some(Some(parent_span_id)) =
                root_parents.and_then(|parents| parents.get(&current_span_id))
            else {
                break;
            };
            current_span_id.clone_from(parent_span_id);
            continue;
        }

        let key = (Arc::clone(service), current_span_id);
        let Some(entry) = resolved_ancestry
            .as_mut()
            .and_then(|ancestry| ancestry.get(&key))
            .cloned()
        else {
            // A retained link (a consumer's) outlives the ancestry LRU.
            let Some(Some(parent_span_id)) = root_parents.and_then(|parents| parents.get(&key.1))
            else {
                break;
            };
            current_span_id = key.1;
            current_span_id.clone_from(parent_span_id);
            continue;
        };
        if let Some(resolution) = entry.resolution {
            matches_source |= resolution.endpoint == source;
            // An unproven resolution answers only where nothing else has.
            if resolution.proven || outermost.is_none() {
                outermost = Some(ResolvedEndpoint {
                    depth: resolution.depth.saturating_add(distance),
                    ..resolution
                });
                below_outermost = traversed.len();
            }
        }
        traversed.push(key);
        let Some(parent_span_id) = entry.parent_span_id else {
            break;
        };
        current_span_id = parent_span_id;
    }
    if let Some(resolution) = &outermost {
        compress_ancestry_path(resolved_ancestry, &traversed, below_outermost, resolution);
    }
    outermost
        .or(nearest_consumer)
        .map(|resolution| (resolution, matches_source))
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
        proven: true,
    })
}

/// Stamp the first `below_outermost` traversed entries with the resolution
/// found above them, and keep the immediate parent warm in the LRU.
fn compress_ancestry_path(
    resolved_ancestry: &mut Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    traversed: &[(Arc<str>, String)],
    below_outermost: usize,
    outermost: &ResolvedEndpoint,
) {
    let Some(ancestry) = resolved_ancestry else {
        return;
    };
    for (offset, key) in traversed[..below_outermost].iter().enumerate() {
        if let Some(entry) = ancestry.get_mut(key) {
            entry.resolution = Some(ResolvedEndpoint {
                endpoint: outermost.endpoint.clone(),
                depth: outermost
                    .depth
                    .saturating_sub(offset)
                    .min(ANCESTOR_WALK_MAX_DEPTH),
                proven: outermost.proven,
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
    context: SourceContext<'_>,
    ambiguous_source_endpoint_services: &HashSet<Arc<str>>,
    resolved_ancestry: Option<&LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
) {
    for event in events.iter_mut() {
        let source = event.event.source.endpoint.trim();
        let source_was_unknown = source.is_empty() || source == "unknown";
        if !source_was_unknown
            && !context.roots.contains_key(event.event.service.as_ref())
            && !context.consumers.contains_key(event.event.service.as_ref())
        {
            continue;
        }
        let Some(parent_span_id) = event.event.parent_span_id.as_ref() else {
            continue;
        };
        if let Some((parent, matches_source)) = peek_parent_endpoint(
            &event.event.service,
            parent_span_id,
            context,
            ambiguous_source_endpoint_services,
            resolved_ancestry,
            resolved_ancestry_cap,
            source,
        ) && parent.depth < ANCESTOR_WALK_MAX_DEPTH
            && (source_was_unknown || matches_source)
        {
            event.event.source.endpoint = parent.endpoint;
        }
    }
    reconcile_event_source_endpoint_groups(events, context);
}

fn peek_parent_endpoint(
    service: &Arc<str>,
    parent_span_id: &str,
    context: SourceContext<'_>,
    ambiguous_source_endpoint_services: &HashSet<Arc<str>>,
    resolved_ancestry: Option<&LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
    source: &str,
) -> Option<(ResolvedEndpoint, bool)> {
    let roots = context.roots.get(service.as_ref());
    let root_parents = context.parents.get(service.as_ref());
    let consumers = context.consumers.get(service.as_ref());
    // A retained consumer is a second entry point, so the guess is off.
    let guess_sole_root = |distance| {
        sole_root_endpoint(
            roots,
            distance,
            resolved_ancestry,
            resolved_ancestry_cap,
            consumers.is_some() || ambiguous_source_endpoint_services.contains(service),
        )
    };
    let mut current_span_id = parent_span_id.to_string();
    let mut outermost = None;
    let mut nearest_consumer = None;
    let mut guess_at = None;
    let mut matches_source = false;
    for distance in 0..ANCESTOR_WALK_MAX_DEPTH {
        note_nearest_consumer(
            &mut nearest_consumer,
            &mut matches_source,
            consumers,
            &current_span_id,
            distance,
            source,
        );
        if let Some(endpoint) =
            roots.and_then(|root_endpoints| root_endpoints.get(&current_span_id))
        {
            matches_source |= endpoint == source;
            outermost = Some(ResolvedEndpoint {
                endpoint: endpoint.clone(),
                depth: distance,
                proven: true,
            });
            let Some(Some(parent_span_id)) =
                root_parents.and_then(|parents| parents.get(&current_span_id))
            else {
                break;
            };
            current_span_id.clone_from(parent_span_id);
            continue;
        }
        let key = (Arc::clone(service), current_span_id);
        let Some(entry) = resolved_ancestry.and_then(|ancestry| ancestry.peek(&key)) else {
            // A retained link (a consumer's) outlives the ancestry LRU.
            let Some(Some(parent_span_id)) = root_parents.and_then(|parents| parents.get(&key.1))
            else {
                guess_at = Some(distance);
                break;
            };
            current_span_id = key.1;
            current_span_id.clone_from(parent_span_id);
            continue;
        };
        if let Some(resolution) = &entry.resolution {
            matches_source |= resolution.endpoint == source;
            // An unproven resolution answers only where nothing else has.
            if resolution.proven || outermost.is_none() {
                outermost = Some(ResolvedEndpoint {
                    endpoint: resolution.endpoint.clone(),
                    depth: resolution.depth.saturating_add(distance),
                    proven: resolution.proven,
                });
            }
        }
        let Some(parent_span_id) = entry.parent_span_id.as_deref() else {
            guess_at = Some(distance);
            break;
        };
        current_span_id = parent_span_id.to_string();
    }
    // A destination proven on the chain outranks a guessed sole root.
    outermost
        .or(nearest_consumer)
        .or_else(|| guess_at.and_then(guess_sole_root))
        .map(|resolution| (resolution, matches_source))
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
            SourceContext {
                roots: &buffer.source_endpoint_groups,
                parents: &buffer.source_endpoint_parent_groups,
                consumers: &buffer.source_consumer_groups,
            },
            &buffer.ambiguous_source_endpoint_services,
            buffer.resolved_ancestry.as_ref(),
            buffer.resolved_ancestry_cap,
        );
    }
    (!events.is_empty()).then_some((trace_id, events))
}

/// Fill unresolved source endpoints in one trace's event slice: the nearest
/// route on the in-slice parent chain, else the nearest consumer destination.
fn reconcile_event_source_endpoint_groups(
    events: &mut [NormalizedEvent],
    context: SourceContext<'_>,
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
            let roots = context.roots.get(service);
            let consumers = context.consumers.get(service);
            if roots.is_none() && consumers.is_none() {
                return None;
            }
            parent_chain_source_endpoint(&parents, service, roots, consumers, event)
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
    roots: Option<&'a HashMap<String, String>>,
    consumers: Option<&'a HashMap<String, String>>,
    event: &NormalizedEvent,
) -> Option<&'a String> {
    let route = |span_id: &str| roots.and_then(|root_endpoints| root_endpoints.get(span_id));
    if let Some(endpoint) = route(&event.event.span_id) {
        return Some(endpoint);
    }
    let mut nearest_consumer = None;
    let mut parent_span_id = event.event.parent_span_id.as_deref();
    for _ in 0..ANCESTOR_WALK_MAX_DEPTH {
        let Some(parent) = parent_span_id else {
            break;
        };
        if let Some(endpoint) = route(parent) {
            return Some(endpoint);
        }
        if nearest_consumer.is_none() {
            nearest_consumer = consumers.and_then(|entries| entries.get(parent));
        }
        parent_span_id = parents.get(&(service, parent)).copied().flatten();
    }
    nearest_consumer
}

#[cfg(test)]
mod tests;
