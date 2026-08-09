# Correlation and streaming

Correlation groups normalized events by `trace_id` to form `Trace` objects for detection. Two implementations exist: one for batch mode and one for streaming (daemon) mode.

## Batch correlation

### Manual `get_mut` / `insert` pattern

The batch correlator uses a deliberate pattern instead of the `HashMap::entry` API:

```rust
if let Some(vec) = map.get_mut(event.event.trace_id.as_str()) {
    vec.push(event);
} else {
    let key = event.event.trace_id.clone();
    map.insert(key, vec![event]);
}
```

**Why not `entry()`?** The `entry()` API requires an owned key upfront because it must store the key if the entry is vacant. This would mean cloning `trace_id` for **every** event, even when the trace already exists (the common case). The manual pattern only clones on the slow path (new trace). For a trace with 50 events, this saves 49 unnecessary String clones.

This is a well-known Rust optimization pattern documented in the [Rust Performance Book](https://nnethercote.github.io/perf-book/hashing.html).

### Capacity hint

```rust
HashMap::with_capacity(events.len() / 10 + 1)
```

The heuristic assumes ~10 events per trace on average. The `+ 1` prevents a zero-capacity map when `events.len() < 10`. Over-estimating is cheap (a few hundred bytes of unused bucket space), under-estimating triggers rehashing.

## Streaming correlation: TraceWindow

The daemon uses a `TraceWindow` that combines three data structures:

1. **LRU cache**: bounds the total number of active traces
2. **Ring buffer** (VecDeque): bounds events per trace
3. **TTL eviction**: expires inactive traces

### LRU cache

The [`lru`](https://docs.rs/lru/) crate provides an O(1) amortized LRU cache backed by a doubly-linked list + HashMap. Operations:

| Operation          | Complexity | Notes                      |
|--------------------|------------|----------------------------|
| `get_mut(key)`     | O(1)       | Auto-promotes to MRU       |
| `push(key, value)` | O(1)       | Evicts LRU if at capacity  |
| `pop_lru()`        | O(1)       | Removes oldest entry       |
| `peek_lru()`       | O(1)       | Inspects without promoting |

The cache capacity uses `NonZeroUsize` as required by the `lru` crate API. The `Config::validate()` method rejects `max_active_traces = 0`, so the `expect("max_active_traces must be >= 1")` in `TraceWindow::new()` is unreachable for valid configurations.

### Ring buffer per trace

Each trace stores its events in a `VecDeque<NormalizedEvent>`:

```rust
struct TraceBuffer {
    events: VecDeque<NormalizedEvent>,
    source_endpoint_groups: HashMap<Arc<str>, HashMap<String, String>>,
    source_endpoint_count: usize,
    resolved_ancestry: Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
    last_seen_ms: u64,
}
```

When a trace exceeds `max_events_per_trace`, the oldest event is dropped:

```rust
if buf.events.len() > self.config.max_events_per_trace {
    buf.events.pop_front();
}
```

**Why `VecDeque`?** `Vec::remove(0)` is O(n) because it shifts all elements. `VecDeque::pop_front()` is O(1) because it is backed by a circular buffer. For traces with high event counts hitting the cap frequently, this avoids O(n^2) degradation.

The initial capacity is `VecDeque::with_capacity(8)`: a small allocation for short-lived traces that avoids repeated doubling for the common case of 1-10 events.

An OTLP trace can also retain SERVER-root endpoint contexts before its I/O
events arrive in a later batch, plus a span-ancestry LRU that retains each
span's parent link and optional resolved route after its event rotates out.
This also repairs parent-after-child arrival with a bounded eight-hop lookup.
Events, root contexts, and ancestry entries are separate collections, and
**each** is capped at `max_events_per_trace`. The ancestry LRU allocates
progressively rather than reserving the configured cap for every trace.
At the valid minimum cap of one, rotation may replace the sole parent entry;
in that case a missing chain falls back only when the service has exactly one
retained root and no distinct root for that service has been observed. A second
distinct root marks the retained service as ambiguous even when the root cap
drops that context; repeating an update for the same root does not. The
ambiguity set is limited to services with retained roots. Multi-root services
and depth-exhausted chains remain unknown. This single-root fallback is never
persisted in the active event or ancestry state: it is applied only to a
`peek_clone` result or to a detached trace at finalization. A later distinct
root can therefore retract the provisional preview without rewriting active
events. Explicit reconciliation first merges roots into this authoritative,
bounded state before resolving events.
Context-only traces use the same `max_active_traces` LRU and `trace_ttl_ms`
eviction as event-bearing traces, so early roots and span ancestry cannot create
unbounded state.

During one ingest batch, each retained root group receives a monotonic window
generation. Events compare that generation in O(1) and reapply the group only
after a real context eviction or replacement. A group larger than the cap is
therefore attempted once rather than rescanned for every event.

### TTL eviction

Traces that have not received events within `trace_ttl_ms` are expired:

```rust
pub fn evict_expired(&mut self, now_ms: u64) -> Vec<(String, Vec<NormalizedEvent>)> {
    let expired_keys: Vec<String> = self.traces.iter()
        .filter(|(_, buf)| now_ms.saturating_sub(buf.last_seen_ms) > ttl)
        .map(|(id, _)| id.clone())
        .collect();
    for key in expired_keys {
        self.traces.pop_entry(&key);
        // ... collect evicted trace
    }
}
```

**Full scan instead of early stop:** clock adjustments (NTP) can cause `last_seen_ms` and LRU position to diverge, leaving expired traces behind non-expired ones. A full scan of the cache ensures all expired traces are evicted regardless of ordering. The cache is bounded by `max_active_traces` (default 10k, max 1M), so the scan cost is negligible compared to detection and scoring.

**`saturating_sub`** prevents underflow if `now_ms < last_seen_ms` (possible with clock skew or NTP adjustments).

### Two eviction methods

- **`evict()`**: silently drops expired traces (used if the caller doesn't need the data)
- **`evict_expired()`**: returns expired traces so the daemon can run detection before discarding

The daemon always uses `evict_expired()` to ensure no trace data is lost without analysis.

### `Vec::from(VecDeque)` for eviction

When converting evicted trace events from `VecDeque` to `Vec`:

```rust
.map(|(id, buf)| (id, Vec::from(buf.events)))
```

`Vec::from(VecDeque)` is specialized in the standard library to reuse the contiguous portion of the ring buffer when possible, avoiding element-by-element moves. This is more efficient than `.into_iter().collect()` which always allocates a new Vec.

### Memory budget

The maximum memory consumption of the TraceWindow can be estimated:

```
max_memory = max_active_traces × max_events_per_trace
             × (avg_event_size + avg_root_context_size + avg_ancestry_entry_size)
```

The three per-trace collections can each reach their cap. With the defaults,
the event portion alone is about 5 GB at the theoretical maximum
(10,000 × 1,000 × ~500 bytes), plus the separately bounded root contexts and
span-ancestry entries.

In practice, most traces have far fewer events than the cap. With typical
traces of 10-50 events, the event portion is approximately:

```
typical_memory = 10,000 × 50 × ~500 bytes = ~250 MB
```

Root contexts and progressively allocated ancestry entries add their actual
occupancy to that event-only estimate; the configured 1,000-entry ancestry cap
is not preallocated for every trace.

The config validation caps `max_active_traces` at 1,000,000 and `max_events_per_trace` at 100,000 to prevent accidental misconfiguration.

The ~500-byte average assumes well-behaved emitters. The adversarial
worst case is bounded per field by `sanitize_span_event` at every
ingest boundary (OTLP, JSON, Jaeger, Zipkin), with `MAX_TARGET_LENGTH`
(64 KiB per `target`) as the dominating term: a hostile or pathological
emitter shipping maximal SQL text in every event can push a single
trace's event collection to roughly 130 MB (1,000 events × ~130 KiB of
capped strings, target plus template). Memory stays bounded, but the
envelope includes the event cap, root-context cap, and span-ancestry cap,
multiplied by the trace cap. Operators who
suspect an oversized-text emitter should lower `max_events_per_trace`
or `max_active_traces` (see the memory-pressure section of
`docs/RUNBOOK.md`).
