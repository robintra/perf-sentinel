# Ingestion and daemon mode

## OTLP conversion

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/otlp-conversion_dark.svg">
  <img alt="OTLP two-pass conversion" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/otlp-conversion.svg">
</picture>

### Two-pass design

`convert_otlp_request()` processes each `resource_spans` block in two passes:

**Pass 1: Build span index:**
```rust
let span_index: HashMap<&[u8], &Span> = scope_spans.iter()
    .flat_map(|ss| &ss.spans)
    .map(|span| (span.span_id.as_slice(), span))
    .collect();
```

**Pass 2: Convert I/O spans:**
```rust
for span in &scope.spans {
    if let Some(event) = convert_span(span, service_name, &span_index) {
        events.push(event);
    }
}
```

**Why two passes?** In OTLP, a parent span may appear after its child in the protobuf message. The first pass builds a lookup table so that the second pass can resolve `source.endpoint` from the parent span's `http.route` attribute. A single-pass approach would miss parent spans defined later in the message.

The index uses `&[u8]` keys (raw span_id bytes), avoiding hex encoding just for lookup. The span index is capped at 100,000 spans per resource to prevent memory exhaustion from pathological OTLP payloads. A `tracing::warn!` is emitted when the cap is reached to help operators diagnose degraded parent resolution.

### `bytes_to_hex` lookup table

```rust
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        buf.push(HEX[(b >> 4) as usize]);
        buf.push(HEX[(b & 0x0f) as usize]);
    }
    // All bytes come from HEX (ASCII 0-9, a-f), always valid UTF-8.
    String::from_utf8(buf).expect("hex table is ASCII")
}
```

This is a well-known optimization for hex encoding. Instead of using `write!(hex, "{b:02x}")` (which invokes the formatting machinery per byte at ~30ns), the lookup table converts each byte to two hex characters via bit shifting at ~5ns per byte. The `Vec<u8>` is pre-allocated and the `from_utf8` call is infallible since only ASCII hex digits are pushed. No `unsafe` is needed: the `expect` is a zero-cost assertion on a condition that cannot fail.

For a 16-byte trace_id + 8-byte span_id, this saves ~600ns per span conversion. At 100,000 events/sec, that is 60ms/sec of avoided overhead.

### `nanos_to_iso8601`: Howard Hinnant's Algorithm

> **Note:** This function now lives in `time.rs` (shared module) and is reused by Jaeger and Zipkin ingestion via `micros_to_iso8601`.

Converting Unix nanoseconds to `YYYY-MM-DDTHH:MM:SS.mmmZ` uses the civil date algorithm from [Howard Hinnant](https://howardhinnant.github.io/date_algorithms.html). The key steps:

1. Convert nanoseconds to days since epoch + remaining milliseconds
2. Shift the epoch to March 1, year 0 (by adding 719,468 days)
3. Compute the era (400-year cycle) and day-of-era
4. Derive year-of-era, day-of-year, month and day using a lookup-free formula

This avoids the [chrono](https://docs.rs/chrono/) crate (~150KB binary overhead) and its ~200ns parse overhead. The hand-rolled algorithm handles leap years correctly (verified by a test with `2024-02-29`).

### Event type priority

When a span has both a SQL attribute (`db.statement` or `db.query.text`) and an HTTP attribute (`http.url` or `url.full`), SQL takes priority. This is intentional: database instrumentation is more specific than HTTP client instrumentation. The SQL attribute carries the actual query text needed for normalization, while the HTTP attribute might represent the same operation at the transport level.

Both legacy (pre-1.21) and stable (1.21+) [OTel semantic conventions](https://opentelemetry.io/docs/specs/semconv/) are supported: `db.statement` and `db.query.text` for SQL, `http.url` and `url.full` for HTTP, `http.method` and `http.request.method` for the HTTP verb, `http.status_code` and `http.response.status_code` for the status. This ensures compatibility with both older OTel SDKs and modern Java agents (v2.x).

### Clock skew protection

```rust
if end_nanos < start_nanos {
    tracing::trace!("Span has end_time < start_time (clock skew?), duration forced to 0");
}
let duration_us = end_nanos.saturating_sub(start_nanos) / 1000;
```

`saturating_sub` returns 0 for negative durations instead of wrapping around. A trace-level log helps operators diagnose OTLP integration issues without flooding logs.

## JSON ingestion

```rust
pub fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
    if raw.len() > self.max_size {
        return Err(JsonIngestError::PayloadTooLarge { ... });
    }
    serde_json::from_slice(raw)
}
```

The payload size is checked **before** deserialization. This prevents `serde_json` from allocating memory for a multi-gigabyte JSON payload before rejecting it.

### Auto-format detection

`JsonIngest` now auto-detects the input format using lightweight byte-level heuristics. It peeks at the first 1-4 KB of the payload:

- Starts with `{` and contains `"data"` + `"spans"` in the first 4 KB: **Jaeger**
- Starts with `[` and contains `"traceId"` + `"localEndpoint"` in the first 1 KB: **Zipkin**
- Otherwise: **Native** perf-sentinel format

This avoids parsing the full payload into a `serde_json::Value` for detection, eliminating a 2x parse cost. The heuristic operates on raw bytes (`std::str::from_utf8` on a bounded prefix), making it O(1) regardless of payload size.

**Boundary sanitization.** After parsing, the JSON ingest path validates `cloud_region` via `is_valid_region_id` and runs `sanitize_span_event` on every event, applying the same field-length caps and UTF-8 boundary truncation as the OTLP path. This ensures all downstream code sees consistently sanitized data regardless of ingestion format.

### Jaeger JSON ingestion

`ingest/jaeger.rs` parses the Jaeger JSON export format (`{ "data": [{ "traceID": "...", "spans": [...], "processes": {...} }] }`). Key mappings:

- `startTime` (microseconds) is converted via `micros_to_iso8601` from the shared `time.rs` module
- `parent_span_id` is extracted from `references` where `refType = "CHILD_OF"`
- Both legacy and stable OTel semantic conventions are supported in tags

### Zipkin JSON v2 ingestion

`ingest/zipkin.rs` parses the Zipkin JSON v2 format (flat array of span objects). Key differences from Jaeger:

- `parentId` is a direct field (not in a references array)
- Tags are a `HashMap<String, String>` (not an array of key-value objects)
- `localEndpoint.serviceName` provides the service name

## Daemon event loop

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/daemon_dark.svg">
  <img alt="Daemon architecture" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/daemon.svg">
</picture>

### Architecture

```
OTLP gRPC (port 4317)   ─┐
OTLP HTTP (port 4318)   ─┤─→ mpsc::channel(1024) ─→ TraceWindow ─→ eviction ─→ detect ─→ score ─→ NDJSON
JSON unix socket        ─┘
```

The event loop uses `tokio::select!` to multiplex:
- **Receive events** from the channel -> normalize -> push into window
- **Ticker** every TTL/2 ms -> evict expired traces -> detect/score -> emit
- **Ctrl+C** -> drain all traces -> detect/score -> emit -> shutdown

### Normalization outside the lock

```rust
// Normalize OUTSIDE the lock:
let normalized: Vec<_> = events.into_iter().map(normalize::normalize).collect();
// Then acquire the lock and push:
let mut w = window.lock().await;
for event in normalized { w.push(event, now_ms); }
```

Normalization is CPU-bound work (regex, string manipulation). Moving it outside the `Mutex` lock minimizes lock hold time to just the HashMap operations. Under contention (ticker and receive running concurrently), this prevents the eviction ticker from blocking on normalization.

### Trace-level sampling

```rust
fn should_sample(trace_id: &str, rate: f64) -> bool {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for b in trace_id.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3); // FNV-1a prime
    }
    (hash as f64 / u64::MAX as f64) < rate
}
```

The [FNV-1a hash](https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function) is a fast, non-cryptographic hash that produces well-distributed output. The offset basis and prime are the standard 64-bit FNV-1a constants.

**Why FNV-1a?** Simpler and faster (~2ns for a typical trace_id) than `std::hash::DefaultHasher` (SipHash, ~10ns). Cryptographic quality is not needed for sampling, only uniform distribution matters.

**Deterministic:** the same `trace_id` always produces the same sampling decision, ensuring all events from a trace are either kept or dropped together.

**Per-batch caching:** the `apply_sampling()` function filters a batch of events using a `HashMap<String, bool>` cache. Within a single batch, multiple events may share a `trace_id`. The cache uses `get()` before `insert()` so that `trace_id` is only cloned for the first event of each trace, not on every cache hit. Extracting this logic into a standalone function keeps the `tokio::select!` event loop readable.

### Bounded channel

```rust
let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(1024);
```

The [bounded channel](https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.channel.html) provides backpressure: if the event loop falls behind and the buffer fills to 1024 batches, ingestion senders will await until space is available. This prevents unbounded memory growth from fast producers.

### Security hardening

**Unix socket permissions:**
```rust
use std::os::unix::fs::PermissionsExt;
std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
```

The `0o600` mode restricts read/write to the socket owner only, preventing other local users from injecting events. If `set_permissions` fails, the socket file is removed and the listener does not start (fatal error, not a warning).

**Connection semaphore:**
```rust
let semaphore = Arc::new(tokio::sync::Semaphore::new(128));
```

Limits concurrent JSON socket connections to 128. Without this, a local attacker could open thousands of connections, each consuming a tokio task and buffer memory.

**Per-connection byte limit:**
```rust
const CONNECTION_LIMIT_FACTOR: u64 = 16;
let limited = stream.take(max_payload_size as u64 * CONNECTION_LIMIT_FACTOR);
```

Each connection is limited to 16 × max_payload_size bytes total (default 16 MB). This prevents a single connection from consuming unbounded memory with a stream of data that never contains a newline.

**Request timeouts:**
- gRPC: `tonic::transport::Server::builder().timeout(Duration::from_secs(60))`
- HTTP: `tower::timeout::TimeoutLayer::new(Duration::from_secs(60))` via axum's `HandleErrorLayer`

These prevent slow/stalled connections from holding resources indefinitely. The HTTP timeout handler emits a `tracing::debug!` log before returning `408 REQUEST_TIMEOUT`, helping operators diagnose slow or stalled clients.

### NDJSON output

Findings are emitted as newline-delimited JSON to stdout using `serde_json::to_writer` with a locked stdout handle to avoid intermediate String allocations and reduce lock contention:

```rust
let stdout = std::io::stdout();
let mut lock = stdout.lock();
for finding in &findings {
    if serde_json::to_writer(&mut lock, finding).is_ok() {
        let _ = writeln!(lock);
    }
}
```

This format is compatible with log aggregation tools (Loki, ELK) that consume line-delimited JSON. Each line is a complete JSON object that can be parsed independently.

### Cumulative waste ratio

The Prometheus `io_waste_ratio` gauge is computed from cumulative counters:

```rust
let cumulative_total = metrics.total_io_ops.get();
if cumulative_total > 0.0 {
    metrics.io_waste_ratio.set(metrics.avoidable_io_ops.get() / cumulative_total);
}
```

This is an all-time average, not a windowed metric. Users who need a recent rate can use Prometheus `rate()` on the raw counters (`total_io_ops`, `avoidable_io_ops`).

### Grafana exemplars

The `prometheus` crate 0.14.0 does not support OpenMetrics exemplars natively. Instead of adding a dependency, exemplar annotations are injected by post-processing the rendered Prometheus text output.

**Tracking worst-case trace IDs:**

`MetricsState` stores exemplar data in `RwLock`-protected fields:
- `worst_finding_trace: HashMap<(String, String), ExemplarData>`, keyed by (finding_type, severity), updated on each `record_batch()` call
- `worst_waste_trace: Option<ExemplarData>`, the trace_id of the finding with the most avoidable I/O

`RwLock` is used instead of `Mutex` because `render()` (read path) is called frequently by Prometheus scrapes, while `record_batch()` (write path) is called less often. Multiple concurrent scrapes should not block each other. Lock poisoning is handled gracefully via `unwrap_or_else(PoisonError::into_inner)`, so a panic in one thread does not cascade into crashes on subsequent lock acquisitions.

**Exemplar injection:**

`inject_exemplars()` iterates over the rendered text line by line. For `perf_sentinel_findings_total{...}` lines, it parses the `type` and `severity` labels to look up the matching exemplar. For `perf_sentinel_io_waste_ratio` lines, it appends the waste trace exemplar.

The exemplar format follows the OpenMetrics specification: `metric{labels} value # {trace_id="abc123"}`. When exemplars are present, the `Content-Type` header switches from `text/plain; version=0.0.4` (Prometheus) to `application/openmetrics-text; version=1.0.0` (OpenMetrics) so that Grafana's Prometheus data source can recognize and display exemplar links.

**Grafana integration:** with exemplars enabled, users can click from a metric spike in Grafana directly to the worst-case trace in Tempo or Jaeger, provided the Prometheus data source has "Exemplars" enabled and a Tempo/Jaeger data source is configured as the trace backend.

## pg_stat_statements ingestion

`ingest/pg_stat.rs` provides a standalone analysis path for PostgreSQL `pg_stat_statements` exports. Unlike trace-based ingestion, this data has no `trace_id` or `span_id`, it cannot feed the N+1/redundant detection pipeline. Instead, it provides hotspot ranking and cross-referencing with trace findings.

### Design decisions

**Separate from `IngestSource`:** the `IngestSource` trait returns `Vec<SpanEvent>`, but `pg_stat_statements` data does not map to `SpanEvent` (no trace_id, span_id or timestamp). It produces its own `PgStatReport` type with rankings.

**Auto-format detection:** follows the same byte-level heuristic pattern as `json.rs`. If the first non-whitespace byte is `[` or `{`, parse as JSON; otherwise, parse as CSV. No external csv crate, the CSV parser handles RFC 4180 quoting manually (double-quoted fields, escaped `""`).

**SQL normalization reuse:** each query goes through `normalize::sql::normalize_sql()` to produce a template comparable with trace-based findings. PostgreSQL normalizes queries at the server level (e.g., `$1` placeholders), but perf-sentinel re-normalizes for consistency with its own template format.

### Cross-referencing

`cross_reference()` accepts `&mut [PgStatEntry]` and `&[Finding]`. It builds a `HashSet` of finding templates and marks entries whose `normalized_template` matches. This is O(n + m) where n = entries, m = findings. The `seen_in_traces` flag enables the CLI to highlight queries that appear in both data sources, useful for validating OTLP trace capture fidelity against database-native ground truth.

## Automated pg_stat Prometheus scrape

`fetch_from_prometheus(endpoint, top_n)` queries a Prometheus HTTP API for `pg_stat_statements` metrics, removing the need for manual CSV export.

### Query and conversion

The function builds a PromQL `topk(N, pg_stat_statements_seconds_total)` instant query and sends it to the Prometheus `/api/v1/query` endpoint via the shared `http_client::fetch_get` helper. The response is a standard Prometheus JSON envelope:

```json
{
  "data": {
    "result": [
      {
        "metric": { "query": "SELECT ...", "datname": "mydb" },
        "value": [1234567890, "1.234"]
      }
    ]
  }
}
```

`parse_prometheus_response` extracts the `query` (or `queryid`) label as the raw SQL text, the `datname` label as the database name and the value as total execution time in seconds. Each result is converted to a `PgStatEntry` with its SQL normalized through `normalize::sql::normalize_sql()` for consistency with trace-based findings.

### CLI integration

The `--prometheus` flag on `perf-sentinel pg-stat` enables this path:

```
perf-sentinel pg-stat --prometheus http://prometheus:9090 --top 20
```

This flag is gated behind the `daemon` feature because it requires the `hyper` HTTP client stack. The rest of the pg-stat pipeline (ranking, cross-referencing, display) is identical regardless of whether the data came from a file or Prometheus.

## Daemon query API

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/query-api_dark.svg">
  <img alt="Daemon query API architecture" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/query-api.svg">
</picture>

The daemon exposes its internal state via HTTP endpoints alongside the existing `/v1/traces`, `/metrics` and `/health` routes on port 4318.

The `/health` endpoint is a stateless liveness probe for Kubernetes, load balancers and systemd. It returns `200 OK` with `{"status":"ok","version":"<pkg_version>"}`, holds no locks and cannot false-negative under load. It is **always exposed**, independent of `[daemon] api_enabled`, which gates only the richer `/api/*` surface described below.

### `FindingsStore` ring buffer

`FindingsStore` is a thread-safe ring buffer backed by `tokio::sync::RwLock<VecDeque<StoredFinding>>`. Each `StoredFinding` wraps a `Finding` with a `stored_at_ms` monotonic timestamp.

- **`push_batch(findings, now_ms)`**: builds the new `StoredFinding` entries outside the lock, then acquires a brief write lock to `extend` the buffer and `drain` any excess. Evicts the oldest entries when the buffer exceeds `max_size` (default 10,000 from config `max_retained_findings`). The initial capacity is `min(max_size, INITIAL_CAPACITY_CEILING)` with a 4096 ceiling to amortize reallocations without a surprising RSS hit at startup.
- **`max_size == 0` short-circuit**: when set to 0, `push_batch` returns immediately without allocating. This lets operators who disable the query API (`api_enabled = false`) reclaim the store's memory by also setting `max_retained_findings = 0`.
- **`query(filter)`**: acquires a read lock, iterates in reverse (newest first), applies optional `service`, `finding_type` and `severity` filters and returns up to `limit` results (default 100, capped at `MAX_FINDINGS_LIMIT = 1000`).
- **`by_trace_id(trace_id)`**: acquires a read lock and returns all findings for a specific trace.

`RwLock` is chosen over `Mutex` because `process_traces` (writer) runs once per tick, while the API handlers (readers) may serve concurrent requests. Multiple read locks do not block each other. Clones happen outside the write lock so readers are not blocked by `Finding::clone()` allocations.

### `QueryApiState` shared state

```rust
pub struct QueryApiState {
    pub findings_store: Arc<FindingsStore>,
    pub window: Arc<tokio::sync::Mutex<TraceWindow>>,
    pub detect_config: DetectConfig,
    pub start_time: std::time::Instant,
    pub correlator: Option<Arc<tokio::sync::Mutex<CrossTraceCorrelator>>>,
}
```

This struct is wrapped in `Arc` and passed as axum `State` to all route handlers. It provides access to the findings ring buffer, the trace window (for explain), the detection config (for re-running detectors on explain requests) and the optional cross-trace correlator (for `/api/correlations`).

### API endpoints

Five endpoints are mounted via `query_api_router()`. The router is only merged into the HTTP stack when `[daemon] api_enabled = true` (default true). Setting `api_enabled = false` disables all `/api/*` routes while keeping OTLP ingestion, `/metrics` and `/health` active.

| Endpoint                   | Method | Cap                                                                      | Description                                                                                |
|----------------------------|--------|--------------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| `/api/findings`            | GET    | `?limit=` clamped to `MAX_FINDINGS_LIMIT = 1000`                         | Query recent findings with optional `?service=`, `?type=`, `?severity=`, `?limit=` filters |
| `/api/findings/{trace_id}` | GET    | none                                                                     | All findings for a specific trace                                                          |
| `/api/explain/{trace_id}`  | GET    | none                                                                     | Trace tree with findings inline, built from daemon memory                                  |
| `/api/correlations`        | GET    | truncated at `MAX_CORRELATIONS_LIMIT = 1000` (sorted by confidence desc) | Active cross-trace correlations from the correlator. Empty when `correlator` is `None`     |
| `/api/status`              | GET    | none                                                                     | Daemon health: version, uptime, active traces, stored findings count                       |

### Explain without eviction via `peek_clone`

The `/api/explain/{trace_id}` handler needs to read a trace's spans from the `TraceWindow` without promoting it in the LRU cache or evicting it. `TraceWindow::peek_clone(trace_id)` uses the underlying `LruCache::peek()` method (read-only, no promotion) and clones the spans into a fresh `Vec<NormalizedEvent>`. The handler then reconstructs a `Trace`, runs per-trace detectors and builds the explain tree via `explain::build_tree` and `explain::format_tree_json`.

If the trace has already been evicted from the window (TTL expired or LRU displaced), the handler returns `{"error": "trace not found in daemon memory"}`.

## Cross-trace correlator integration

### Conditional creation

In `daemon::run()`, the correlator is created only when `config.correlation_enabled` is true (default false, opt-in via `[daemon.correlation] enabled = true`). When created, it is wrapped in `Arc<Mutex<CrossTraceCorrelator>>`:

```rust
let correlator = if config.correlation_enabled {
    Some(Arc::new(Mutex::new(
        CrossTraceCorrelator::new(config.correlation_config.clone()),
    )))
} else {
    None
};
```

### Invocation in `process_traces`

The correlator reference (`Option<&Mutex<CrossTraceCorrelator>>`) is passed to `process_traces`. After findings are produced, scored and pushed to the `FindingsStore`, the correlator's `ingest()` method is called:

```rust
if let Some(correlator) = correlator {
    correlator.lock().await.ingest(&findings, now_ms);
}
```

This ordering ensures that the `FindingsStore` always has the findings before the correlator processes them.

### NDJSON output

Active correlations are not emitted to NDJSON stdout alongside findings. They are exposed via the `/api/correlations` HTTP endpoint and the `perf-sentinel query correlations` CLI subcommand. This separation avoids mixing findings (per-trace, per-tick) with correlations (aggregated, cross-trace) in the same output stream.
