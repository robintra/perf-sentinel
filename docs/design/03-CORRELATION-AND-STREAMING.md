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
max_memory = max_active_traces × max_events_per_trace × avg_event_size
           = 10,000 × 1,000 × ~500 bytes
           = ~5 GB (theoretical maximum)
```

In practice, most traces have far fewer events than the cap. With typical traces of 10-50 events:

```
typical_memory = 10,000 × 50 × ~500 bytes = ~250 MB
```

The config validation caps `max_active_traces` at 1,000,000 and `max_events_per_trace` at 100,000 to prevent accidental misconfiguration.
