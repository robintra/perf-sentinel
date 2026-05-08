# Detection algorithms

Detection is the fourth pipeline stage. It analyzes correlated traces to identify seven types of anti-patterns: N+1 queries, redundant calls, slow operations, excessive fanout, chatty services, connection pool saturation and serialized-but-parallelizable calls.

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

## Sanitizer-aware classification

OpenTelemetry agents collapse SQL literals to `?` by default to keep PII out of trace attributes. The sanitized statement (`SELECT ... WHERE id = ?`) reaches perf-sentinel with the placeholder already in place, and `normalize_sql` leaves it as-is (it only extracts numeric and string literals, not literal `?`). For an ORM-induced N+1 every span ends up with the same `template` and an empty `params` vector. The standard `distinct_params >= threshold` check sees one distinct empty params slice and never fires, the redundant detector then groups all the spans together and misclassifies them as `redundant_sql`.

The heuristic in `crates/sentinel-core/src/detect/sanitizer_aware.rs` recovers the correct classification via three signals, evaluated in order:

1. `looks_sanitized`: every span has a `?` placeholder in its template and an empty `params` vector. Required to activate the heuristic at all.
2. `has_orm_scope`: at least one OpenTelemetry instrumentation scope on the spans matches a known ORM marker (Hibernate, Spring Data, EF Core, SQLAlchemy, ActiveRecord, GORM, Prisma, Diesel, etc.). Markers are matched with a word-boundary check (preceded and followed by a non-alphanumeric byte), so `jpa` only fires on `spring-data-jpa` and friends, never on `myappjpastats`. A positive match is treated as strong evidence of N+1.
3. `timing_variance_suggests_n_plus_one`: when the scope signal is absent, fall back to the coefficient of variation of `duration_us`. True N+1 hits different rows with different cache states, so the spread is wider, cached redundant calls cluster tightly. Threshold `0.5` is empirical.

The four emission modes (`Auto`, `Strict`, `Always`, `Never`) are documented in `docs/CONFIGURATION.md` § "`sanitizer_aware_classification`" with their precision/recall trade-offs.

### Known limit

`looks_sanitized` cannot tell a sanitized literal `?` apart from a PostgreSQL JSONB existence operator (`data ? 'key'`) when the latter happens to appear in a query with no other literals. The harm direction is asymmetric: a misclassified JSONB group flips from `redundant_sql` to `n_plus_one_sql`, both of which contribute equally to GreenOps `avoidable_io_ops`, only the suggestion text differs.

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

### Sanitizer-aware classification (0.5.7+)

The same shape appears whenever the OpenTelemetry agent runs its SQL statement sanitizer (default ON), since literals are collapsed to `?` before the span reaches perf-sentinel. The standard distinct-params rule sees one bucket of empty params and rejects the group, so the redundant detector misclassifies the N+1 as `redundant_sql` and the operator gets the wrong remediation.

The 0.5.7 sanitizer-aware heuristic recovers the correct classification by running a second pass over the same `(event_type, template)` groups that the first pass rejected. It activates only when every span in the group has an empty `params` vector and a `?` placeholder in its template (the on-wire signature of a sanitized N+1, truly literal-free queries like `SELECT NOW()` have no `?` in the template). It then evaluates two independent signals:

1. **Instrumentation scope marker** (high confidence). Per-span `instrumentation_scopes` chains are searched, case-insensitively, for any of the known ORM substrings: `spring-data`, `hibernate`, `jpa`, `micronaut-data`, `jdbi`, `r2dbc`, `entityframeworkcore`, `entity-framework`, `sqlalchemy`, `django.db`, `active-record`/`activerecord`, `gorm`, `sqlx`, `sequelize`, `prisma`, `typeorm`, `mongoose`, `sea-orm`, `diesel`. A match flips the verdict to `LikelyNPlusOne`.
2. **Timing-variance fallback** (medium confidence). When no ORM marker is present, the heuristic computes the coefficient of variation (`std-dev / mean`) of `duration_us`. True N+1 lookups hit different rows with different cache states, so durations spread out (CV typically 0.4 to 1.0), cached redundant calls cluster tightly (CV near 0). The threshold of `0.5` is empirical and is the only knob in the heuristic. At least 3 spans are required for a stable variance estimate.

The configurable `[detection] sanitizer_aware_classification` mode gates emission across four points on a recall-vs-precision dial: `auto` (default) emits when **either** signal fires, `strict` (0.5.8+) emits only when **both** signals fire conjointly, `always` reclassifies every sanitized group regardless of signal, and `never` disables the second pass entirely. Findings emitted by the heuristic carry `classification_method = SanitizerHeuristic` so consumers can distinguish them from direct classifications. The mode picks where to sit on the trade-off:

- `auto` favors recall: catches all ORM-induced N+1 because the ORM scope alone fires the verdict, at the cost of absorbing legitimate `redundant_sql` findings on Spring Data / EF Core stacks (a `findById(sameId)` called in a loop served from row cache flips to `n_plus_one_sql`).
- `strict` favors precision: preserves `redundant_sql` findings on cached identical queries because the timing-variance signal stays low, at the cost of missing N+1 patterns whose rows happen to be cache-warm. Recommended when actionable `redundant_sql` findings are valuable signal in your environment.

Known limits: a real single-param redundancy whose literal happens to be collapsed by the sanitizer (e.g. `SELECT * FROM config WHERE key = ?` queried 10 times for the same key) cannot be distinguished from an N+1 without scope or variance signal. In `auto` mode it flips to `n_plus_one_sql` whenever an ORM scope is present (harm-reducing direction: batch fetch is a strict superset of "cache one value"). In `strict` mode it stays `redundant_sql` because the timing variance is low. In `always` mode it always flips. In `never` mode the heuristic is bypassed entirely.

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
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/detection_dark.svg">
  <img alt="Detection orchestration" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/detection.svg">
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

Excessive fanout detects a **single parent** with too many direct children. Chatty service detects an **entire trace** with too many outbound HTTP calls, independently of the parent-child structure. A trace can trigger both when a single parent generates all the calls or only chatty service when the calls are spread across multiple parents.

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

## Cross-trace temporal correlation (daemon mode)

In daemon mode (`perf-sentinel watch`), perf-sentinel sees findings from all traces over time. The `CrossTraceCorrelator` detects recurring temporal co-occurrences between findings from different services: "every time the N+1 in order-svc fires, pool saturation appears in payment-svc within 2 seconds."

### Internal state

```rust
pub struct CrossTraceCorrelator {
    occurrences: VecDeque<FindingOccurrence>,
    pair_counts: HashMap<PairKey, PairState>,
    source_totals: HashMap<CorrelationEndpoint, u32>,
    config: CorrelationConfig,
}
```

Three data structures track the correlation state:

- **`occurrences`**: a `VecDeque` of recent finding occurrences, ordered by timestamp. Each entry records a `CorrelationEndpoint` (finding_type, service, template) and a `timestamp_ms`. This is the rolling window.
- **`pair_counts`**: a `HashMap` keyed by `PairKey` (source endpoint, target endpoint). Each value holds the co-occurrence count, a bounded reservoir of observed lag values, a `total_observations` counter, a per-pair `SplitMix64` PRNG state and first/last seen timestamps. This is the correlation accumulator.
- **`source_totals`**: a `HashMap` counting how many times each `CorrelationEndpoint` is currently in the window. Used as the denominator in the confidence calculation. Maintained incrementally (incremented on `push_back`, decremented on `pop_front`); entries are removed when the count reaches zero so the map stays bounded by the number of distinct endpoints, not the number of occurrences.

### The `ingest()` algorithm

`ingest()` is called from `process_traces` after findings are produced and confidence is stamped. It takes a `&[Finding]` batch and a `now_ms` timestamp. The algorithm has five steps:

1. **Evict stale entries.** Walk `occurrences` from front to back, popping entries older than `now_ms - window_ms` (default 10 minutes) and decrement `source_totals` for each evicted endpoint. This is O(k) where k is the number of expired entries.

2. **Prune stale pair counts.** A single `HashMap::retain` pass over `pair_counts` removes pairs whose `last_seen_ms` is outside the window. O(pairs).

3. **Scan for co-occurrences.** For each incoming finding, construct a `CorrelationEndpoint`. Iterate `occurrences` backwards (most recent first). For each recent occurrence from a **different service** within `lag_threshold_ms` (default 5 seconds), increment the pair counter and record the lag via reservoir sampling (see below). The backwards scan breaks early once it reaches entries beyond the lag threshold, keeping this O(l) where l is the number of occurrences within the lag window.

4. **Append to window.** Push the new finding occurrence onto the back of `occurrences` and increment its `source_totals` count.

5. **Enforce pair cap.** If `pair_counts.len()` exceeds `max_tracked_pairs` (default 10,000), use `select_nth_unstable_by_key` (O(n) average) to find the lowest-count entries and remove them until the cap is met. This eviction prioritizes retaining the most significant correlations.

### The `active_correlations()` filter

`active_correlations()` iterates over `pair_counts` and applies two thresholds:

- `min_co_occurrences` (default 5): pairs that have co-occurred fewer times are filtered out.
- `min_confidence` (default 0.7): confidence is `co_occurrence_count / source_total_occurrences`. Pairs below this ratio are filtered out.

For each qualifying pair, the function computes `median_lag_ms` and converts `first_seen_ms`/`last_seen_ms` to ISO 8601 via `time::millis_to_iso8601`.

### Reservoir sampling for lag values

A hot pair firing thousands of times within the window would otherwise grow `lags_ms` without bound (megabytes per pair). To keep memory per pair flat, `record_lag` uses Algorithm R reservoir sampling capped at `MAX_LAG_SAMPLES = 256`:

- While the reservoir has space, append unconditionally.
- Once full, draw `r` uniformly in `[0, total_observations)` via `SplitMix64`. If `r < MAX_LAG_SAMPLES`, replace `lags_ms[r]`. Conditional on `r < k`, `r` is itself uniform in `[0, k)`, so the slot pick is unbiased without a second PRNG draw.

The PRNG is a `SplitMix64` state per `PairState`, seeded at construction from `now_ms ^ (hash_endpoint(source) << 17) ^ hash_endpoint(target)`. `hash_endpoint` is a deterministic FNV-1a over the endpoint's `finding_type`, `service` and `template` strings (NOT the `DefaultHasher`, which uses a per-process `RandomState` and would make the correlator non-deterministic across runs). Two daemon runs replaying the same trace file produce identical reservoir samples and therefore identical median lags.

### Median lag calculation

The `median()` helper sorts a clone of the lag values and returns the middle element (odd length) or the midpoint of the two middle elements (even length). Sorting is bounded by `MAX_LAG_SAMPLES` thanks to the reservoir, so the median computation is O(k log k) with k = 256 regardless of how often the pair fires.

### Memory management

Three mechanisms bound memory usage:

- **Rolling window eviction**: the `occurrences` deque is pruned on every `ingest()` call. Entries older than `window_ms` are removed and their `source_totals` count is decremented. Entries reaching count zero are removed from the map.
- **Pair count pruning**: `pair_counts` entries whose `last_seen_ms` falls outside the window are removed.
- **Reservoir cap**: each `PairState.lags_ms` is bounded at `MAX_LAG_SAMPLES = 256` f64 (~2 KB per pair), regardless of how often the pair fires.
- **Pair cap with lowest-count eviction**: when `pair_counts.len()` exceeds `max_tracked_pairs`, the least significant pairs (lowest co-occurrence count) are evicted via `select_nth_unstable_by_key`.

### Integration point

The correlator is created conditionally in the daemon's `run()` function based on `config.correlation_enabled` (default false). It is wrapped in `Arc<Mutex<CrossTraceCorrelator>>` and passed to `process_traces`. After findings are produced and pushed to the `FindingsStore`, the correlator's `ingest()` method is called with the findings and the current timestamp.

### Batch mode exclusion

The correlator is **not** used in batch mode (`perf-sentinel analyze`). Cross-trace correlation requires a stream of findings over time to detect recurring patterns. A single batch run typically processes a fixed set of traces without the temporal dimension needed for meaningful correlation.

## Actionable fixes (framework-aware suggestions)

Starting in v0.4.2, a `suggested_fix: Option<SuggestedFix>` field on `Finding` carries a framework-specific remediation that goes beyond the generic `suggestion` string. This field is populated by `detect::suggestions::enrich` after the per-trace detectors return, inside `detect()`. The first cut shipped Java/JPA only. The current state covers Java (JPA, WebFlux, Quarkus reactive, Quarkus non-reactive, Helidon SE, Helidon MP), C# (.NET 8 to 10 with EF Core / Pomelo MySQL) and Rust (Diesel, SeaORM), with a generic per-language fallback for HTTP fanout and request-scoped caching guidance.

### The `SuggestedFix` struct

```rust
pub struct SuggestedFix {
    pub pattern: String,          // "n_plus_one_sql" mirrors parent finding.type
    pub framework: String,        // "java_jpa" or "java_generic"
    pub recommendation: String,   // short, imperative sentence
    pub reference_url: Option<String>,
}
```

Serialized in JSON as a nested object under `finding.suggested_fix`, skipped when absent. Emitted in SARIF under `result.fixes[0].description.text` (description-only form of the SARIF 2.1.0 fix object). The CLI renders it as a nested `Suggested fix:` line right after the generic `Suggestion:` line.

### Framework detector

The detector is a pure function that only reads `finding.code_location` (already populated by each detector from the span's OTel `code.*` attributes). No span-level access, no extra allocations. Decision chain:

1. No `code_location` or no `filepath` → return `None`.
2. Map the file extension to a language: `.java` → Java, `.cs` → C#, `.rs` → Rust. Anything else → return `None`.
3. Walk that language's rules in declared order (most specific first). Return the first framework whose namespace hint matches.
4. Fall back to the language's generic framework (`JavaGeneric`, `CsharpGeneric`, `RustGeneric`) when no rule matches.

The namespace match is segment-boundary-aware on **both** sides: the hint must start at the namespace root or immediately after a separator and must end at the namespace end or immediately before another separator. Boundary characters are `.` (Java, C#) and `::` (Rust). Examples:

- `diesel::` matches `diesel::query_dsl::FilterDsl` and `crate::diesel::reexport` but **not** `crate::mydiesel::query` (leading boundary protects user code that embeds the hint).
- `io.helidon` matches `io.helidon.webserver.Routing` but **not** `io.helidongrpc.Foo` (trailing boundary protects against user packages whose first segment merely starts with the hint).
- `Microsoft.EntityFrameworkCore` matches `Microsoft.EntityFrameworkCore.Query` but **not** `Microsoft.EntityFrameworkCoreCache.Provider`.

### Per-language rules

Order matters within a language: the first matching framework wins. JPA hints intentionally trail Quarkus reactive hints because `org.hibernate.reactive` contains `org.hibernate`.

**Java (`JAVA_RULES`):**

| Framework                | Namespace hints                                                                                                                       |
|--------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| `JavaHelidonMp`          | `io.helidon.microprofile`                                                                                                             |
| `JavaHelidonSe`          | `io.helidon`                                                                                                                          |
| `JavaQuarkusReactive`    | `io.quarkus.hibernate.reactive`, `io.quarkus.panache.reactive`, `io.quarkus.reactive`, `org.hibernate.reactive`, `io.smallrye.mutiny` |
| `JavaQuarkus`            | `io.quarkus.hibernate.orm`, `io.quarkus.panache.common`, `io.quarkus`                                                                 |
| `JavaWebFlux`            | `org.springframework.web.reactive`, `reactor.core`                                                                                    |
| `JavaJpa`                | `jakarta.persistence`, `javax.persistence`, `org.hibernate`, `org.springframework.data.jpa`                                           |
| `JavaGeneric` (fallback) | (any `.java` file without the above)                                                                                                  |

`JavaQuarkusReactive` enumerates its reactive sub-packages explicitly. The catch-all `io.quarkus` belongs to `JavaQuarkus` (non-reactive), so any reactive Quarkus namespace must be matched by one of the more-specific reactive hints first. Helidon MP must come before Helidon SE because `io.helidon.microprofile` is a sub-package of `io.helidon`.

**Note on Helidon MP and JPA:** Helidon MP entities are JPA-managed under Hibernate. A typical OTel JDBC span on Helidon MP code carries `code.namespace = jakarta.persistence.*` or `org.hibernate.*`, which routes to `JavaJpa` (not `JavaHelidonMp`). The `JavaHelidonMp` rule fires when the span comes from Helidon MP plumbing itself (REST resources, CDI containers, MicroProfile Rest Client). For database findings on Helidon MP apps, the `JavaJpa` recommendation applies.

**C# (`CSHARP_RULES`):**

| Framework                  | Namespace hints                                               |
|----------------------------|---------------------------------------------------------------|
| `CsharpEfCore`             | `Microsoft.EntityFrameworkCore`, `Pomelo.EntityFrameworkCore` |
| `CsharpGeneric` (fallback) | (any `.cs` file without the above)                            |

**Rust (`RUST_RULES`):**

| Framework                | Namespace hints                    |
|--------------------------|------------------------------------|
| `RustDiesel`             | `diesel::`                         |
| `RustSeaOrm`             | `sea_orm::`                        |
| `RustGeneric` (fallback) | (any `.rs` file without the above) |

### Mapping table

A `LazyLock<HashMap<(FindingType, Framework), SuggestedFix>>` static. Lookups missing from the table leave `suggested_fix` as `None`. Current entries:

| Finding type   | Framework             | Recommendation anchor                                                                                           |
|----------------|-----------------------|-----------------------------------------------------------------------------------------------------------------|
| `NPlusOneSql`  | `JavaJpa`             | `JOIN FETCH` or `@EntityGraph`, Hibernate User Guide                                                            |
| `NPlusOneSql`  | `JavaQuarkusReactive` | Mutiny `Session.fetch()` + `@NamedEntityGraph`, Quarkus Hibernate Reactive guide                                |
| `NPlusOneSql`  | `JavaQuarkus`         | JPQL/Panache `JOIN FETCH`, `@EntityGraph` or `Session.fetchProfile`, Quarkus Hibernate ORM guide                |
| `NPlusOneSql`  | `JavaHelidonSe`       | Helidon `DbClient` named query with JOIN or `:ids` JDBC parameter binding                                       |
| `NPlusOneSql`  | `JavaHelidonMp`       | JPA `@EntityGraph` or JPQL `JOIN FETCH` (MP entities are JPA-managed under Hibernate)                           |
| `NPlusOneHttp` | `JavaWebFlux`         | `Flux.merge()` / `Flux.zip()` for parallelism or batch endpoint                                                 |
| `NPlusOneHttp` | `JavaQuarkusReactive` | `Uni.combine().all().unis(...)` for parallelism, Mutiny combining guide                                         |
| `NPlusOneHttp` | `JavaQuarkus`         | `CompletableFuture.allOf` on `ManagedExecutor`, batch via Quarkus REST Client                                   |
| `NPlusOneHttp` | `JavaHelidonSe`       | Helidon SE `WebClient` + `Single.zip` / `Multi.merge` for parallelism or batch endpoint                         |
| `NPlusOneHttp` | `JavaHelidonMp`       | MicroProfile Rest Client + `CompletableFuture.allOf` on the `@ManagedExecutorConfig` executor or batch endpoint |
| `NPlusOneHttp` | `JavaGeneric`         | Batch endpoint or request-scoped `@Cacheable`                                                                   |
| `RedundantSql` | `JavaQuarkusReactive` | `@CacheResult` or `Uni.memoize().indefinitely()`                                                                |
| `RedundantSql` | `JavaQuarkus`         | `@CacheResult` (Quarkus cache extension) or `@RequestScoped` HashMap deduplication                              |
| `RedundantSql` | `JavaGeneric`         | Service-level cache (Caffeine, Spring Cache)                                                                    |
| `NPlusOneSql`  | `CsharpEfCore`        | `.Include()` / `.ThenInclude()`, `.AsSplitQuery()` for Cartesian explosion                                      |
| `RedundantSql` | `CsharpEfCore`        | `IMemoryCache`, scoped DbContext for per-request short-circuit                                                  |
| `NPlusOneHttp` | `CsharpGeneric`       | `Task.WhenAll` for parallel calls, batch endpoint, response caching on `HttpClient`                             |
| `NPlusOneSql`  | `RustDiesel`          | `belonging_to` + `grouped_by` or `.inner_join` / `.left_join` for single query                                  |
| `NPlusOneSql`  | `RustSeaOrm`          | `find_with_related` / `find_also_related` or `QuerySelect::join`                                                |
| `RedundantSql` | `RustDiesel`          | `moka` cache or request-local `OnceCell`                                                                        |
| `RedundantSql` | `RustSeaOrm`          | `moka` cache or request-local `OnceCell`                                                                        |
| `NPlusOneHttp` | `RustGeneric`         | `tokio::join!` / `futures::future::join_all` for parallelism or batch endpoint                                  |

### Extension path for contributors

To add a new framework:

1. Extend the private `Framework` enum in `detect/suggestions.rs`.
2. Pick a language and append a `(Framework, &[hint])` entry to that language's rule slice. Place more-specific frameworks before less-specific ones.
3. Add entries to the `FIXES` static for each `(FindingType, Framework)` pair you want to map.
4. Add unit tests under the `tests` module in the same file.

To add a new language:

1. Extend the `Language` enum and its `rules()` / `generic()` methods.
2. Add the file extension match in `language_from_filepath`.
3. Define a new `*_RULES` slice and a generic fallback variant on `Framework`.

No wiring changes elsewhere: the `detect()` orchestrator already calls `suggestions::enrich` at the end of the per-trace detection pass and the CLI / JSON / SARIF rendering already handle an optional `suggested_fix`.
