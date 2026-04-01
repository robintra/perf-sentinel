# Detection algorithms

Detection is the fourth pipeline stage. It analyzes correlated traces to identify three types of anti-patterns: N+1 queries, redundant calls, and slow operations.

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

### ISO 8601 Timestamp Parser

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

**Limitation:** the parser computes milliseconds since midnight, not since epoch. Cross-midnight traces may compute incorrect window durations. This is documented in [LIMITATIONS.md](../LIMITATIONS.md).

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

The three detectors run sequentially on each trace. While they could theoretically share a single grouping pass, the key types differ (`(&EventType, &str)` vs `(&EventType, &str, &[String])`), and the separate implementations are clearer and independently testable. With typical trace sizes of 10-50 spans, three O(n) passes are negligible.
