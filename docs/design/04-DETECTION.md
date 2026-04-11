# Detection algorithms

Detection is the fourth pipeline stage. It analyzes correlated traces to identify seven types of anti-patterns: N+1 queries, redundant calls, slow operations, excessive fanout, chatty services, connection pool saturation, and serialized-but-parallelizable calls.

## Shared pattern: borrowed HashMap keys

All three detectors group spans by a composite key. A key insight is that the spans live in the `Trace` struct, which outlives the detector function. This means we can **borrow** from the spans instead of cloning:

```rust
// N+1: group by (event_type, template)
HashMap<(&EventType, &str), Vec<usize>>

// Redundant: group by (event_type, template, params)
HashMap<(&EventType, &str, &[String]), Vec<usize>>

// Slow: group by (event_type, template)
HashMap<(&EventType, &str), Vec<usize>>
```

The values are `Vec<usize>`: indices into `trace.spans` rather than cloned spans. This keeps the HashMap small and avoids copying the event data.

For a trace with 50 spans, each having a 40-character template string, borrowed keys save 50 × 40 = 2,000 bytes of String allocations per grouping pass.

## N+1 detection

### Algorithm

1. Group spans by `(&EventType, &str template)`
2. Skip groups with fewer than `threshold` occurrences (default 5)
3. Count **distinct parameter sets** via `HashSet<&[String]>`
4. Skip groups with fewer than `threshold` distinct params (same params = redundant, not N+1)
5. Compute time window between earliest and latest timestamp
6. Skip groups where the window exceeds `window_limit_ms` (default 500ms)
7. Assign severity: Critical if >= 10 occurrences, Warning otherwise

### Distinct params via borrowed slices

```rust
let distinct_params: HashSet<&[String]> = indices
    .iter()
    .map(|&i| trace.spans[i].params.as_slice())
    .collect();
```

Using `&[String]` as a HashSet key is a critical design choice:
- **No allocation:** borrows the existing Vec as a slice reference
- **No collision bug:** directly compares the full Vec content, unlike a `join(",")` approach where `["a,b"]` and `["a", "b"]` would produce the same joined string

Rust's standard library implements `Hash` and `Eq` for `&[T]` when `T: Hash + Eq`, making this zero-cost.

### Iterator-based window computation

```rust
pub fn compute_window_and_bounds_iter<'a>(
    mut iter: impl Iterator<Item = &'a str>,
) -> (u64, &'a str, &'a str) {
    let Some(first) = iter.next() else {
        return (0, "", "");
    };
    let mut min_ts = first;
    let mut max_ts = first;
    let mut has_second = false;
    for ts in iter {
        has_second = true;
        if ts < min_ts { min_ts = ts; }
        if ts > max_ts { max_ts = ts; }
    }
    // ...
}
```

**Why iterator instead of `&[&str]`?** The caller would need to collect timestamps into a Vec first:

```rust
// Old (allocates):
let timestamps: Vec<&str> = indices.iter().map(|&i| ...).collect();
let (w, min, max) = compute_window_and_bounds(&timestamps);

// New (zero allocation):
let (w, min, max) = compute_window_and_bounds_iter(
    indices.iter().map(|&i| trace.spans[i].event.timestamp.as_str())
);
```

The iterator-based version eliminates one `Vec<&str>` allocation per detection group. With 3 detectors × multiple groups per trace × thousands of traces, this adds up.

The `has_second` boolean replaces a `count` variable that was only used to check `count < 2`. This avoids incrementing a counter on every iteration.

### ISO 8601 timestamp parser

```rust
fn parse_timestamp_ms(ts: &str) -> Option<u64> {
    let time_part = ts.split('T').nth(1)?;
    let time_part = time_part.trim_end_matches('Z');
    let mut colon_parts = time_part.split(':');
    let hours: u64 = colon_parts.next()?.parse().ok()?;
    let minutes: u64 = colon_parts.next()?.parse().ok()?;
    let sec_str = colon_parts.next()?;
    // ... parse seconds and fractional part
}
```

**Why not [chrono](https://docs.rs/chrono/)?** chrono adds ~150KB to the binary and parses ~200ns per timestamp. This hand-rolled parser handles the fixed format (`YYYY-MM-DDTHH:MM:SS.mmmZ`) in ~5ns by splitting on known delimiters and using iterator `.next()` calls instead of collecting into Vecs.

The parser uses iterators throughout (`split(':')` -> `.next()`, `split('.')` -> `.next()`) to avoid allocating intermediate `Vec<&str>` collections.

The parser computes milliseconds since Unix epoch by parsing both the date (`YYYY-MM-DD`) and time components. The date-to-days conversion uses the [Howard Hinnant algorithm](http://howardhinnant.github.io/date_algorithms.html) (public domain), which requires no external dependencies.

### Lexicographic timestamp comparison

Min/max timestamps are found via string comparison: `if ts < min_ts { min_ts = ts; }`. This works because ISO 8601 timestamps with fixed-width fields (`2025-07-10T14:32:01.123Z`) sort chronologically when compared lexicographically. This is guaranteed by the [ISO 8601 standard](https://www.iso.org/iso-8601-date-and-time-format.html), Section 5.3.3.

## Redundant detection

### Borrowed slice keys

```rust
HashMap<(&EventType, &str, &[String]), Vec<usize>>
```

The three-part key includes the full params slice, ensuring that two spans with the same template but different params are in different groups. This is the correct behavior: redundant detection flags **exact duplicates** (same template AND same params).

The use of `&[String]` instead of joining params into a single string prevents a subtle collision bug: `["a,b"]` (one param containing a comma) and `["a", "b"]` (two params) would produce the same joined key `"a,b"` but are semantically different parameter sets.

### Severity

- **Info** (< 5 occurrences): common for config lookups, health checks
- **Warning** (>= 5 occurrences): likely a loop bug or missing cache

The threshold of 2 (minimum to flag) catches any exact duplicate. Unlike N+1 which requires 5+ occurrences, even 2 identical queries in one request is suspicious and worth flagging at Info level.

### ORM bind parameters

ORMs that use named bind parameters (Entity Framework Core with `@__param_0`, Hibernate with `?1`) produce SQL spans where actual parameter values are not visible in `db.statement`/`db.query.text`. In this case, N+1 patterns (same query with different values) appear as redundant queries (same template, same visible params), because perf-sentinel cannot distinguish the bound values. Both findings correctly identify the repeated query pattern. ORMs that inline literal values (SeaORM raw statements, JDBC without prepared statements) allow accurate N+1 vs redundant classification.

## Slow detection

### Saturation arithmetic

```rust
let threshold_us = threshold_ms.saturating_mul(1000);
// ...
if max_duration_us > threshold_us.saturating_mul(5) {
    Severity::Critical
}
```

[`saturating_mul`](https://doc.rust-lang.org/std/primitive.u64.html#method.saturating_mul) returns `u64::MAX` on overflow instead of wrapping to zero. This prevents a malicious or misconfigured `threshold_ms = u64::MAX` from disabling severity thresholds.

### Not part of waste ratio

Slow findings have `green_impact.estimated_extra_io_ops = 0`. They are **necessary** operations that happen to be slow: they need optimization (indexing, caching), not elimination. Including them in the waste ratio would conflate "avoidable I/O" (N+1, redundant) with "slow I/O" (needs a different fix).

## Detection orchestration

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../diagrams/svg/detection_dark.svg">
  <img alt="Detection orchestration" src="../diagrams/svg/detection.svg">
</picture>

```rust
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        findings.extend(detect_n_plus_one(trace, ...));
        findings.extend(detect_redundant(trace));
        findings.extend(detect_slow(trace, ...));
    }
    findings
}
```

The detectors run sequentially on each trace. While they could theoretically share a single grouping pass, the key types differ (`(&EventType, &str)` vs `(&EventType, &str, &[String])`) and the separate implementations are clearer and independently testable. With typical trace sizes of 10-50 spans, multiple O(n) passes are negligible.

## Fanout detection

### Algorithm

1. Group spans by `parent_span_id`
2. Skip groups where the parent has `max_fanout` or fewer children (default 20)
3. For each parent exceeding the threshold, emit an `ExcessiveFanout` finding
4. Severity: Warning if > `max_fanout`, Critical if > 3x `max_fanout`

The fanout detector uses a `HashMap<&str, usize>` span index for O(1) parent lookup and `compute_window_and_bounds` to compute the chronological span of child timestamps in a single pass.

### Not part of waste ratio

Like slow findings, fanout findings have `green_impact.estimated_extra_io_ops = 0`. Excessive fanout is a structural concern (too many child operations per parent) that needs architectural optimization, not I/O elimination. Both the dedup loop and the green_impact enrichment use `FindingType::is_avoidable_io()` to make this determination, ensuring a single source of truth.

## Cross-trace slow percentiles

In batch mode, `detect_slow_cross_trace` collects slow spans across all traces and computes p50/p95/p99 percentiles per normalized template. This complements the per-trace slow detection by identifying templates that are consistently slow across multiple requests.

- Only spans exceeding the threshold are collected (pre-filter for performance)
- Only templates appearing in at least 2 distinct traces are reported (single-trace cases are handled by per-trace detection)
- Percentile computation uses the nearest-rank method via `div_ceil`

## Chatty service detection

### Algorithm

1. Filter spans to HTTP outbound only (`type: http_out`)
2. Count total HTTP outbound spans in the trace
3. If count < `chatty_service_min_calls` (default 15), skip
4. Collect the top called normalized endpoints for the suggestion message
5. Assign severity: Warning if > threshold, Critical if > 3x threshold

```
Input:  trace with N spans
Output: 0 or 1 ChattyService finding

filter spans where type == http_out
if count(http_spans) < chatty_service_min_calls:
    return []

group http_spans by normalized template
sort groups by count descending
top_endpoints = first 5 groups

severity = Critical if count >= 3 * threshold else Warning
emit finding with top_endpoints in suggestion
```

**Complexity:** O(n) to filter and count, O(k log k) to sort groups where k is the number of distinct templates. Since k << n in practice, this is effectively O(n).

### Difference from fanout

Excessive fanout detects a **single parent** with too many direct children. Chatty service detects an **entire trace** with too many outbound HTTP calls, independently of the parent-child structure. A trace can trigger both when a single parent generates all the calls, or only chatty service when the calls are spread across multiple parents.

### Not part of waste ratio

Chatty service findings have `green_impact.estimated_extra_io_ops = 0`. The detector flags an architectural concern (too many inter-service calls per request), not a batching opportunity. The calls may all be necessary; the problem is that the service boundary is too fine-grained. `FindingType::is_avoidable_io()` returns `false` for `ChattyService`.

## Connection pool saturation detection

### Algorithm

1. Filter spans to SQL only (`type: sql`)
2. Group SQL spans by service name
3. For each service group, compute peak concurrency via sweep-line
4. If peak concurrency < `pool_saturation_concurrent_threshold` (default 10), skip
5. Assign severity: Warning if > threshold, Critical if > 3x threshold

```
Input:  trace with N spans, grouped by service
Output: 0 or more PoolSaturation findings (one per service)

for each service in sql_spans_by_service:
    events = []
    for span in service_spans:
        start = parse_timestamp(span.timestamp)
        end = start + span.duration_us
        events.push((start, +1))
        events.push((end, -1))

    sort events by timestamp, with -1 before +1 on ties
    current = 0
    peak = 0
    for (ts, delta) in events:
        current += delta
        peak = max(peak, current)

    if peak >= pool_saturation_concurrent_threshold:
        emit finding
```

**Complexity:** O(n log n) for the sort step, O(n) for the sweep. Total: O(n log n) per service group.

### Sweep-line tie-breaking

When a span ends and another begins at the exact same microsecond, the algorithm processes the end event (`-1`) before the start event (`+1`). This avoids inflating peak concurrency when spans are merely adjacent rather than overlapping.

### Not part of waste ratio

Pool saturation findings have `green_impact.estimated_extra_io_ops = 0`. High concurrency is not avoidable I/O. It signals potential contention on the database connection pool, which is a tuning or architectural concern. `FindingType::is_avoidable_io()` returns `false` for `PoolSaturation`.

## Serialized calls detection

### Algorithm

1. Group sibling spans by `parent_span_id`
2. For each parent group, sort children by **end time** (ascending)
3. Find the longest non-overlapping subsequence via dynamic programming (Weighted Interval Scheduling with unit weights)
4. If the optimal sequence has >= `serialized_min_sequential` (default 3) spans with distinct templates, emit a finding
5. Severity: always Info (heuristic, inherent false positive risk)

```
Input:  trace with N spans, grouped by parent_span_id
Output: 0 or more SerializedCalls findings

for each parent_id in spans_by_parent:
    children = spans with this parent_id
    if len(children) < serialized_min_sequential:
        skip

    sort children by end_time ascending
    
    // Predecessor computation: for each span i, binary search for p(i),
    // the rightmost span j (j < i) whose end_time <= span i's start_time.
    // O(log n) per span.
    
    // DP recurrence:
    //   dp[i] = max(dp[i-1], dp[p(i)] + 1)
    // where dp[i] = longest non-overlapping subsequence in children[0..=i]
    
    // Backtrack from dp[n-1] to reconstruct the selected spans.
    // Guard: predecessor must be strictly less than current index
    // to guarantee termination on degenerate input (zero-duration spans).
    
    if len(selected) >= serialized_min_sequential
       AND distinct_templates(selected) > 1:
        emit finding for selected sequence
```

Complexity: O(n log n) for sorting + O(n log n) for all binary searches + O(n) for the DP fill and backtrack = O(n log n) total per parent group. This is the same asymptotic cost as the simpler greedy approach, but the DP guarantees finding the longest possible non-overlapping sequence. For example, given spans A:[0-200ms], B:[100-150ms], C:[160-300ms], D:[310-400ms], a greedy approach sorted by start time would select {A, D} (length 2), while the DP correctly identifies {B, C, D} (length 3).

The binary search uses `partition_point` directly on the sorted slice, avoiding a separate predecessor array allocation.

### Why `info` only

The detector cannot observe data dependencies between calls. Two sequential calls to different services may be intentionally ordered (e.g. create a record, then notify a dependent service). The `info` severity signals an investigation opportunity, not a confirmed defect.

### Template filtering

The detector skips sequences where all spans share the same normalized template. That pattern is N+1 (same operation repeated with different params), not serialization. By requiring different templates, the detector targets the "fetch user, then fetch orders, then fetch preferences" pattern where the calls are independent and could run concurrently.

### Time savings estimate

The finding includes the potential time savings: `total_sequential_duration - max_individual_duration`. If 3 sequential calls each take 100ms, parallelizing them could reduce latency from 300ms to 100ms, saving 200ms. This is a best-case estimate that assumes no shared resource contention.

### Not part of waste ratio

Serialized call findings have `green_impact.estimated_extra_io_ops = 0`. Parallelizing calls does not reduce the total number of I/O operations. It reduces latency, not I/O volume. `FindingType::is_avoidable_io()` returns `false` for `SerializedCalls`.

## Detection orchestration (updated)

```rust
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        findings.append(&mut detect_n_plus_one(trace, ...));
        findings.append(&mut detect_redundant(trace));
        findings.append(&mut detect_slow(trace, ...));
        findings.append(&mut detect_fanout(trace, config.max_fanout));
        findings.append(&mut detect_chatty(trace, config.chatty_service_min_calls));
        findings.append(&mut detect_pool_saturation(trace, config.pool_saturation_concurrent_threshold));
        findings.append(&mut detect_serialized(trace, config.serialized_min_sequential));
    }
    findings
}
```

The seven detectors run sequentially on each trace. `append(&mut ...)` is used instead of `extend()` to move the backing allocation in O(1) without iterator overhead. Cross-trace slow percentile analysis runs separately in `pipeline.rs` after per-trace detection and before scoring.
