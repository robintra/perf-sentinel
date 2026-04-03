# Known Limitations and Trade-offs

## OTLP Capture Reliability

perf-sentinel is a **passive listener**: it receives traces forwarded by OpenTelemetry SDKs or collectors. Unlike an in-process agent (e.g., Hypersistence Utils), it cannot guarantee that every span is captured. Spans may be lost due to:

- Network issues between the application and perf-sentinel
- Sampling configured at the SDK or collector level
- Application crashes before spans are flushed

**Mitigation:** For critical CI pipelines, use batch mode (`perf-sentinel analyze`) with pre-collected trace files instead of relying on live capture.

## SQL Tokenizer

The SQL normalizer uses a homemade regex-based tokenizer rather than a full SQL parser. This is intentional: it keeps the binary small, avoids heavy dependencies, and works across SQL dialects. However, it has limitations:

- **No semantic parsing:** the tokenizer replaces literals and UUIDs positionally. It does not build an AST and cannot reason about query structure.
- **ASCII only:** the tokenizer operates byte-by-byte and assumes ASCII SQL. Non-ASCII characters in identifiers, comments, or string literals (e.g., accented characters) may produce incorrect template or param values. SQL keywords and operators are always ASCII, so this only affects extracted parameter values for non-ASCII string literals.
- **Edge cases:** deeply nested CTEs, non-standard SQL extensions, or unusual quoting styles may not normalize correctly.
- **Stored procedures:** `CALL` statements with complex parameter expressions may not be fully normalized.

If you encounter a query that normalizes incorrectly, please open an issue with the raw SQL (anonymized).

**Complementarity with pg_stat_statements:** perf-sentinel detects per-trace patterns (N+1, redundant calls) that pg_stat_statements cannot see. Conversely, pg_stat_statements provides aggregate server-side statistics (total calls, mean time) that perf-sentinel does not track. They complement each other ; use both for full visibility.

## Cross-midnight Timestamps

The min/max timestamp selection for findings uses lexicographic ISO 8601 comparison, which sorts chronologically and works across midnight. However, the **window duration** (`window_ms`) is computed from time-of-day only (hours, minutes, seconds, milliseconds since midnight). Traces that span midnight will compute an incorrect window of 0ms, which may cause false positive N+1 detections for events that are actually ~24 hours apart.

**Mitigation:** In practice, this is rare because individual traces typically complete in seconds. In daemon mode, the default TTL (30s) prevents traces from accumulating across midnight. In batch mode, ensure traces being analyzed do not span midnight boundaries, or accept the potential for false positives on cross-midnight traces.

## ORM bind parameters and N+1 vs redundant classification

ORMs that use named bind parameters (Entity Framework Core with `@__param_0`, Hibernate with `?1`) produce SQL spans where the parameter values are not visible in the `db.statement`/`db.query.text` attribute. perf-sentinel sees the template with the bind placeholders but not the actual values.

This means that N+1 patterns (same query, different values) may be classified as `redundant_sql` (same query, same visible params) instead of `n_plus_one_sql` (same query, different params). Both findings correctly identify the repeated query pattern and the suggestion to batch or cache remains valid.

ORMs that inline literal values (SeaORM with raw statements, JDBC without prepared statements) produce spans with visible parameter values, enabling accurate N+1 vs redundant classification.

## Slow Findings and Waste Ratio

Slow findings (`slow_sql`, `slow_http`) represent operations that are **necessary but slow** : they are not avoidable I/O. Therefore, slow findings do **not** contribute to the I/O waste ratio or the `avoidable_io_ops` count in the GreenOps summary. They still appear in the findings list with `green_impact.estimated_extra_io_ops: 0`.

This is by design: the waste ratio measures how much I/O could be eliminated (N+1, redundant), while slow findings highlight operations that need optimization (indexing, caching) rather than elimination.

## Fanout detection requires `parent_span_id`

Fanout detection (`excessive_fanout`) relies on the `parent_span_id` field to build parent-child relationships between spans. If the tracing instrumentation does not propagate parent span IDs (some older OTel SDKs or custom instrumentations), fanout detection will not produce findings.

Fanout findings, like slow findings, are **not** counted as avoidable I/O in the waste ratio. They represent a structural concern (too many child operations per parent) rather than eliminable I/O.

## `rss_peak_bytes` on Windows

The `perf-sentinel bench` command reports peak RSS (Resident Set Size) using platform-specific APIs. On Windows, this metric is reported as `null` because the current implementation uses Unix-only `getrusage()`. The throughput and latency metrics work on all platforms.

## Sampling in Daemon Mode

When `sampling_rate` is set below 1.0 in the `[daemon]` configuration, perf-sentinel randomly drops traces to reduce resource usage. This means:

- Some N+1 or redundant patterns may go undetected
- The waste ratio is computed only over sampled traces and may not represent the full traffic
- Prometheus metrics (`perf_sentinel_traces_analyzed_total`) reflect only sampled traces

For accurate detection, use `sampling_rate = 1.0` (the default) or sample at the collector level where you have more control.

## Maximum Events Per Trace

In streaming mode, each trace holds at most `max_events_per_trace` events (default: 1000) in a ring buffer. If a trace generates more events, the oldest are dropped. This can cause:

- Missed N+1 patterns if the repeated operations fall outside the retained window
- Undercounted occurrences in findings

For traces with very high event counts, increase `max_events_per_trace` or investigate why a single trace generates so many operations.

## Binary Size

The release binary targets < 10 MB with `lto = "thin"`, `strip = true`, and `panic = "abort"`. The embedded carbon intensity table and OTLP protobuf support contribute to binary size. If you need a smaller binary and do not use OTLP ingestion, building with feature flags (future work) could reduce size.

## No Authentication or TLS

perf-sentinel does **not** implement authentication or TLS on any of its ingestion endpoints (OTLP gRPC, OTLP HTTP, JSON unix socket, Prometheus `/metrics`). By default, the daemon binds to `127.0.0.1` (loopback only), which is safe for single-machine deployments.

If you expose perf-sentinel to a network:

- Place it behind a reverse proxy that handles TLS and authentication
- Use network policies (Kubernetes `NetworkPolicy`, Docker network isolation, firewall rules) to restrict access
- Route traces through an OpenTelemetry Collector with its own auth extensions, and forward to perf-sentinel on a trusted internal network

Never expose perf-sentinel directly to untrusted networks without a security layer in front.

## gCO2eq Energy Constant

The carbon estimation uses a fixed energy constant (`0.1 uWh per I/O operation`) as a rough order-of-magnitude approximation. This value is **not** a measured quantity : actual energy consumption depends on I/O type, hardware, query complexity, and infrastructure. The constant is intended to provide directional guidance (more I/O = more energy) rather than precise measurement. When comparing gCO2eq values across runs, the relative differences are meaningful even if absolute values are approximate.
