# Known limitations and trade-offs

## OTLP capture reliability

perf-sentinel is a **passive listener**: it receives traces forwarded by OpenTelemetry SDKs or collectors. Unlike an in-process agent (e.g., Hypersistence Utils), it cannot guarantee that every span is captured. Spans may be lost due to:

- Network issues between the application and perf-sentinel
- Sampling configured at the SDK or collector level
- Application crashes before spans are flushed

**Mitigation:** For critical CI pipelines, use batch mode (`perf-sentinel analyze`) with pre-collected trace files instead of relying on live capture.

## SQL tokenizer

The SQL normalizer uses a homemade regex-based tokenizer rather than a full SQL parser. This is intentional: it keeps the binary small, avoids heavy dependencies and works across SQL dialects. However, it has limitations:

- **No semantic parsing:** the tokenizer replaces literals and UUIDs positionally. It does not build an AST and cannot reason about query structure.
- **Query length limit:** SQL queries exceeding 64 KB are truncated at a character boundary before normalization. This prevents unbounded memory allocation from adversarial or pathological inputs.
- **CTEs:** Common Table Expressions (`WITH ... AS (...)`) are supported -- the tokenizer normalizes literals inside CTEs correctly, including nested CTEs.
- **Double-quoted identifiers:** SQL-standard double-quoted identifiers (`"MyTable"`, `"Column"`) are preserved as-is. Digits inside double quotes are not mistaken for numeric literals.
- **Dollar-quoted strings:** PostgreSQL dollar-quoted strings (`$$body$$`, `$tag$body$tag$`) are replaced with `?` placeholders, including in function bodies.
- **`CALL` statements:** literal parameters in `CALL` are normalized (`CALL process(42, 'rush')` becomes `CALL process(?, ?)`). SQL expressions like `NOW()`, `INTERVAL '...'` are handled (the string inside `INTERVAL` is replaced, the function call is preserved).
- **Backtick identifiers:** MySQL-style backtick identifiers (`` `table` ``) are not specifically handled. They pass through as-is without causing errors, but the backtick characters remain in the template.

If you encounter a query that normalizes incorrectly, please open an issue with the raw SQL (anonymized).

**Complementarity with pg_stat_statements:** perf-sentinel detects per-trace patterns (N+1, redundant calls) that pg_stat_statements cannot see. Conversely, pg_stat_statements provides aggregate server-side statistics (total calls, mean time) that perf-sentinel does not track. They complement each other, use both for full visibility.

## ORM bind parameters and N+1 vs redundant classification

ORMs that use named bind parameters (Entity Framework Core with `@__param_0`, Hibernate with `?1`) produce SQL spans where the parameter values are not visible in the `db.statement`/`db.query.text` attribute. perf-sentinel sees the template with the bind placeholders but not the actual values.

This means that N+1 patterns (same query, different values) may be classified as `redundant_sql` (same query, same visible params) instead of `n_plus_one_sql` (same query, different params). Both findings correctly identify the repeated query pattern and the suggestion to batch or cache remains valid.

ORMs that inline literal values (SeaORM with raw statements, JDBC without prepared statements) produce spans with visible parameter values, enabling accurate N+1 vs redundant classification.

## Slow findings and waste ratio

Slow findings (`slow_sql`, `slow_http`) represent operations that are **necessary but slow**, they are not avoidable I/O. Therefore, slow findings do **not** contribute to the I/O waste ratio or the `avoidable_io_ops` count in the GreenOps summary. They still appear in the findings list with `green_impact.estimated_extra_io_ops: 0`.

This is by design: the waste ratio measures how much I/O could be eliminated (N+1, redundant), while slow findings highlight operations that need optimization (indexing, caching) rather than elimination.

## Fanout detection requires `parent_span_id`

Fanout detection (`excessive_fanout`) relies on the `parent_span_id` field to build parent-child relationships between spans. If the tracing instrumentation does not propagate parent span IDs (some older OTel SDKs or custom instrumentations), fanout detection will not produce findings.

Fanout findings, like slow findings, are **not** counted as avoidable I/O in the waste ratio. They represent a structural concern (too many child operations per parent) rather than eliminable I/O.

## `rss_peak_bytes` on Windows

The `perf-sentinel bench` command reports peak RSS (Resident Set Size) using platform-specific APIs. On Windows, this metric is reported as `null` because the current implementation uses Unix-only `getrusage()`. The throughput and latency metrics work on all platforms.

## Sampling in daemon mode

When `sampling_rate` is set below 1.0 in the `[daemon]` configuration, perf-sentinel randomly drops traces to reduce resource usage. This means:

- Some N+1 or redundant patterns may go undetected
- The waste ratio is computed only over sampled traces and may not represent the full traffic
- Prometheus metrics (`perf_sentinel_traces_analyzed_total`) reflect only sampled traces

For accurate detection, use `sampling_rate = 1.0` (the default) or sample at the collector level where you have more control.

## Maximum events per trace

In streaming mode, each trace holds at most `max_events_per_trace` events (default: 1000) in a ring buffer. If a trace generates more events, the oldest are dropped. This can cause:

- Missed N+1 patterns if the repeated operations fall outside the retained window
- Undercounted occurrences in findings

For traces with very high event counts, increase `max_events_per_trace` or investigate why a single trace generates so many operations.

## Binary size

The release binary targets < 10 MB with `lto = "thin"`, `strip = true` and `panic = "abort"`. The embedded carbon intensity table and OTLP protobuf support contribute to binary size. If you need a smaller binary and do not use OTLP ingestion, building with feature flags (future work) could reduce size.

## No Authentication or TLS

perf-sentinel does **not** implement authentication or TLS on any of its ingestion endpoints (OTLP gRPC, OTLP HTTP, JSON unix socket, Prometheus `/metrics`). By default, the daemon binds to `127.0.0.1` (loopback only), which is safe for single-machine deployments.

If you expose perf-sentinel to a network:

- Place it behind a reverse proxy that handles TLS and authentication
- Use network policies (Kubernetes `NetworkPolicy`, Docker network isolation, firewall rules) to restrict access
- Route traces through an OpenTelemetry Collector with its own auth extensions and forward to perf-sentinel on a trusted internal network

Never expose perf-sentinel directly to untrusted networks without a security layer in front.

## Carbon estimates accuracy

perf-sentinel uses an **I/O → energy → CO₂ proxy model** to estimate the carbon footprint of analyzed workloads. The chain has three steps and an inherent margin of error at each:

1. **I/O operations → energy**: each detected I/O op (SQL query, HTTP call) is multiplied by a fixed `ENERGY_PER_IO_OP_KWH` constant of `0.0000001 kWh` (~0.1 µWh). This is **not measured**, it is an order-of-magnitude approximation.
2. **Energy → CO₂**: energy is multiplied by a per-region grid carbon intensity (gCO₂eq/kWh) sourced from Electricity Maps and Cloud Carbon Footprint annual averages (2023-2024), with a per-provider PUE applied (AWS 1.135, GCP 1.10, Azure 1.185, Generic 1.2).
3. **Embodied carbon (`M` in SCI v1.0)**: hardware manufacturing emissions amortized at a configurable default of `0.001 gCO₂/request`. Region-independent.

### Uncertainty: 2× multiplicative, not ±50%

Every CO₂ estimate is reported as `{ low, mid, high }` where:

```
low  = mid × 0.5   (half the midpoint)
high = mid × 2.0   (twice the midpoint)
```

This is a **log-symmetric multiplicative interval**, not an arithmetic ±50% window. The geometric mean of `low` and `high` equals `mid`; the arithmetic mean does not. The 2× framing is deliberate: the I/O proxy model has order-of-magnitude uncertainty (ENERGY_PER_IO_OP_KWH is rougher than half), so a symmetric ±50% window would understate the real model uncertainty. Read the bounds as "the true value is within a factor of 2 of `mid`, in either direction".

The bounds reflect aggregate model uncertainty, not per-endpoint variance.

**This bracket is a directional indicator of model uncertainty, not a statistical confidence interval.** The true value on unusual I/O workloads (mixed SQL + HTTP, cache-heavy paths, custom storage engines) may fall outside `[low, high]`. Use the range to gauge *order-of-magnitude* plausibility, not as a probabilistic bound.

### SCI v1.0 semantics: numerator vs intensity

The `co2.total` field holds the **SCI v1.0 numerator** `(E × I) + M`, summed over all analyzed traces. This is **not** the per-request intensity score that the SCI specification defines as "SCI". To get the per-request intensity, consumers compute:

```
sci_per_trace = co2.total.mid / analysis.traces_analyzed
```

This distinction matters: perf-sentinel reports a **footprint** (absolute emissions), not an **intensity** (emissions per functional unit). The `methodology` field on each `CarbonEstimate` tags the semantic:

- `co2.total.methodology = "sci_v1_numerator"`: the `(E × I) + M` footprint over analyzed traces.
- `co2.avoidable.methodology = "sci_v1_operational_ratio"`: `operational × (avoidable_io_ops / accounted_io_ops)`, a region-blind global ratio that excludes embodied carbon by design.

### Positioning: directional waste counter

perf-sentinel is a **directional waste counter** designed to:

- **Detect performance anti-patterns** (N+1, redundant queries, fanout) and quantify their relative carbon impact.
- **Compare runs** before/after optimization to validate that a fix actually reduces I/O.
- **Catch carbon regressions** in CI as a guardrail.

It is **NOT a regulatory carbon accounting tool**. Do **NOT** use it for:

- CSRD (Corporate Sustainability Reporting Directive) reporting.
- GHG Protocol Scope 3 disclosures.
- Audit-grade compliance documents.
- Comparing absolute CO₂ values across different infrastructures (the model assumes a uniform, average server profile).
- Replacing real measured energy data (RAPL, Scaphandre, in-process power meters).

### What works

| Use case | Reliability |
|---|---|
| Detect waste (N+1, fanout, redundant) | ✅ deterministic counting |
| Compare runs (baseline vs. fix) | ✅ relative deltas are meaningful |
| Rank endpoints by relative impact | ✅ within a single deployment |
| CI carbon regression guardrails | ✅ via `[thresholds] io_waste_ratio_max` |
| Absolute CO₂ in compliance reports | ❌ 2× multiplicative uncertainty |
| Cross-infrastructure comparison | ❌ assumes uniform server profile |
| Replacing measured energy | ❌ proxy only |

### Multi-region scoring (Phase 5a)

When OTel spans carry the `cloud.region` resource attribute, perf-sentinel automatically buckets I/O ops per region and applies the correct grid intensity coefficient. The fallback chain is:

1. `event.cloud_region` from the OTel attribute.
2. `[green.service_regions]` per-service config mapping.
3. `[green] default_region`.

I/O ops with no resolvable region land in a synthetic `"unknown"` bucket and contribute zero operational CO₂ (a `tracing::warn!` is emitted). Embodied carbon is still emitted because hardware emissions are region-independent.

See `docs/design/05-GREENOPS-AND-CARBON.md` for the full methodology, formula, and SCI v1.0 alignment notes.

## gCO2eq energy constant (legacy section, kept for cross-references)

The carbon estimation uses a fixed energy constant (`0.1 uWh per I/O operation`) as a rough order-of-magnitude approximation. See **Carbon estimates accuracy** above for the complete methodology and disclaimer.

## pg_stat_statements ingestion

- **No trace correlation.** `pg_stat_statements` data has no `trace_id` or `span_id`. It cannot be used for per-trace anti-pattern detection (N+1, redundant). It provides complementary hotspot analysis and cross-referencing with trace-based findings.
- **CSV parsing.** The CSV parser handles RFC 4180 quoting (double-quoted fields, escaped `""`), but assumes UTF-8 input. Non-UTF-8 files will fail to parse.
- **Pre-normalized queries.** PostgreSQL normalizes `pg_stat_statements` queries at the server level. perf-sentinel applies its own normalization on top for cross-referencing, which may produce slightly different templates.
- **No live connection.** perf-sentinel reads exported CSV or JSON files. It does not connect to PostgreSQL directly.
- **Entry count.** The parser pre-allocates memory based on input size, capped at 100,000 entries. Files exceeding 1,000,000 entries (CSV rows or JSON array elements) are rejected with an error to prevent memory exhaustion.
