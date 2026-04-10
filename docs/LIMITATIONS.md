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

## Field length limits at ingestion

All ingestion boundaries (OTLP, JSON, Jaeger, Zipkin) truncate string fields to prevent unbounded memory growth from oversized attributes. Limits: `service` 256 bytes, `operation` 256 bytes, `target` 64 KB, `source.endpoint` 512 bytes, `source.method` 512 bytes, `timestamp` 64 bytes, `trace_id`/`span_id` 128 bytes. Truncation preserves UTF-8 char boundaries. Fields within the limit are untouched (zero-copy fast path).

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

| Use case                              | Reliability                             |
|---------------------------------------|-----------------------------------------|
| Detect waste (N+1, fanout, redundant) | ✅ deterministic counting                |
| Compare runs (baseline vs. fix)       | ✅ relative deltas are meaningful        |
| Rank endpoints by relative impact     | ✅ within a single deployment            |
| CI carbon regression guardrails       | ✅ via `[thresholds] io_waste_ratio_max` |
| Absolute CO₂ in compliance reports    | ❌ 2× multiplicative uncertainty         |
| Cross-infrastructure comparison       | ❌ assumes uniform server profile        |
| Replacing measured energy             | ❌ proxy only                            |

### Multi-region scoring

When OTel spans carry the `cloud.region` resource attribute, perf-sentinel automatically buckets I/O ops per region and applies the correct grid intensity coefficient. The fallback chain is:

1. `event.cloud_region` from the OTel attribute.
2. `[green.service_regions]` per-service config mapping.
3. `[green] default_region`.

I/O ops with no resolvable region land in a synthetic `"unknown"` bucket and contribute zero operational CO₂ (a `tracing::warn!` is emitted). Embodied carbon is still emitted because hardware emissions are region-independent.

See `docs/design/05-GREENOPS-AND-CARBON.md` for the full methodology, formula, and SCI v1.0 alignment notes.

### Hourly carbon profiles

Embedded hourly UTC profiles are available for 30+ cloud regions across all major cloud providers and geographies. Four regions (FR, DE, GB, US-East) have **monthly x hourly** profiles (288 values each) that capture seasonal variation. The remaining regions have **flat-year** profiles (24 values, same shape all year).

**Monthly x hourly regions** (12 months x 24 hours):

- **France (`eu-west-3`)**: nuclear baseload with winter gas peaking. Higher intensity in winter, lower in summer.
- **Germany (`eu-central-1`)**: coal + renewables. Strong seasonal variance: winter coal use increases significantly.
- **UK (`eu-west-2`)**: wind + gas. Winter has more gas heating, summer has more wind.
- **US-East (`us-east-1`)**: gas + coal. Summer AC load and winter heating both push intensity above spring/fall.

**Flat-year hourly regions** (24-hour profile, same all year):

- **Europe (ENTSO-E)**: Ireland (`eu-west-1`), Netherlands (`eu-west-4`), Sweden (`eu-north-1`), Belgium (`europe-west1`), Finland (`europe-north1`), Italy (`eu-south-1`), Spain (`europe-southwest1`), Poland (`europe-central2`), Norway (`europe-north2`).
- **Americas (EIA / IESO / ONS)**: US Ohio (`us-east-2`), US N. California (`us-west-1`), US Oregon (`us-west-2`), Canada Quebec (`ca-central-1`), Brazil (`sa-east-1`).
- **Asia-Pacific (best-effort)**: Japan (`ap-northeast-1`), Singapore (`ap-southeast-1`), India (`ap-south-1`), Australia (`ap-southeast-2`).

Country-code aliases (`fr`, `de`, `gb`, `ie`, `se`, `no`, `jp`, `br`, etc.) and cloud-provider synonyms (`westeurope`, `northeurope`, `uksouth`, `francecentral`, etc.) are supported and resolve to the same profile.

When `[green] use_hourly_profiles = true` (the default), the scoring stage uses the hour-specific (and month-specific when available) intensity for each span based on the span's UTC timestamp. Regions without a profile always use the flat annual value. Reports are tagged with `model = "io_proxy_v3"` (monthly x hourly), `"io_proxy_v2"` (flat-year hourly), or `"io_proxy_v1"` (annual), and each per-region breakdown row carries an `intensity_source` field (`"annual"`, `"hourly"`, or `"monthly_hourly"`).

**What this does and doesn't do.** The hourly path captures time-of-day variance (a 3am N+1 in France costs less than a 7pm N+1). Monthly x hourly profiles also capture seasonal variance for the 4 listed regions. It does **not** capture:

- **Weather-dependent fluctuations**: the embedded values are typical averages, not real-time data. A calm windless day in the UK will produce more carbon than the profile suggests.
- **Real-time grid data**: for live carbon intensity, consider the Electricity Maps API integration (planned for a future release).

**Estimated profiles.** The Asia-Pacific and Brazil profiles are estimated from fuel mix composition rather than hourly generation data. They are annotated as such in the source code. The diurnal shapes are approximations based on the known fuel mix (e.g. gas-dominated grids are nearly flat, coal-heavy grids have mild evening peaks).

**Timestamp requirements.** perf-sentinel parses timestamps as UTC and requires the canonical ISO 8601 form `YYYY-MM-DDTHH:MM:SS[.fff]Z` (trailing `Z`) or the space-separated variant. Strings with non-UTC offsets (`+02:00`, `-05:00`) are rejected rather than silently shifted. The carbon table is UTC-anchored, so naive offset handling would systematically skew the estimate. Spans with unparseable timestamps fall back to the flat annual intensity.

**Accuracy improvement (approximate).** Compared to the flat-annual model, the hourly profiles reduce the time-of-day component of the uncertainty budget from ~±50% to ~±20% **for the 4 listed regions only**. The overall 2× multiplicative uncertainty bracket on the CO₂ estimate is unchanged, because the energy-per-op proxy constant remains the dominant source of error.

To pin reports to the flat-annual model (e.g. to compare historical runs without the hourly shift), set `[green] use_hourly_profiles = false` in the config.

#### ⚠️ Germany (`eu-central-1`) hourly profile diverges from the flat annual

Unlike France, UK and US-East, whose hourly profiles stay within ±5% of their corresponding flat annual values in the primary carbon table, the Germany hourly profile has an **arithmetic mean of ~442 g/kWh**, whereas the embedded flat annual in `CARBON_TABLE[eu-central-1]` is **338 g/kWh** (a ~31% divergence). This reflects recent (2023-2024) ENTSO-E data on the German grid, which has been dominated by coal and variable renewables with pronounced peaks; the embedded flat annual predates this shift and is optimistic by comparison.

**What this means for your reports:**

- If you run reports with `default_region = "eu-central-1"` (or any span carrying `cloud.region = eu-central-1`) and the default `use_hourly_profiles = true`, you will see **CO₂ numbers roughly 31% higher** than you would have seen before the hourly profiles landed.
- The new numbers are closer to reality than the old flat-annual ones. **We do not recommend pinning to the old numbers** except for backward-compatibility purposes (e.g. regression-comparing a new run against a baseline captured before the hourly profiles landed).
- If you do need the old behaviour, set `[green] use_hourly_profiles = false` in your config. This disables hourly for all regions, not just Germany.
- If you have CI quality gates (`[thresholds] io_waste_ratio_max` etc.) calibrated on the old DE numbers, you will need to recalibrate after the upgrade.

The divergence is documented inline in `score/carbon.rs` so future data refreshes stay honest about the mismatch. A regression test (`de_flat_annual_numerical_regression`) pins the flat-annual value so accidental edits to the DE profile cannot silently corrupt it.

### Scaphandre precision bounds

perf-sentinel ships an opt-in integration with [Scaphandre](https://github.com/hubblo-org/scaphandre) for per-process energy measurement via Intel RAPL counters. When `[green.scaphandre]` is configured, the `watch` daemon scrapes the Scaphandre Prometheus endpoint every few seconds and uses the measured power readings to replace the fixed `ENERGY_PER_IO_OP_KWH` proxy constant for each mapped service.

**Platform requirements.** Scaphandre works on:

- **Linux only** (no Windows, no macOS, no BSD).
- **Intel or AMD x86_64 CPUs with RAPL support**: most recent server and desktop chips, but notably **NOT ARM64**. Apple Silicon, Ampere, Graviton and similar cloud ARM instances cannot use this integration.
- **Bare metal or VMs with RAPL passthrough.** Most cloud VMs (AWS EC2, GCP GCE, Azure VMs) do **not** expose RAPL counters to guest OSes. Kubernetes pods running on bare-metal nodes can access RAPL if the host exposes `/sys/class/powercap/intel-rapl/` into the container (requires privileged access or explicit mount).

On unsupported platforms, the `[green.scaphandre]` section is parsed and the scraper spawns, but it will fail to find the endpoint and silently fall back to the proxy model. A single warn-level log line is emitted at first failure so operators notice the misconfiguration.

**What Scaphandre improves.** The integration replaces the fixed proxy coefficient (0.1 µWh per I/O op) with a **service-level measured value** derived from the actual power consumption of the mapped process over the scrape window. Formula:

```
energy_per_op_kwh = (process_power_watts × scrape_interval_secs) / ops_in_window / 3_600_000
```

This captures:

- **Actual process power** (not an averaged approximation).
- **Per-service differences**: Java vs .NET vs Node vs Go will have different energy footprints even for similar I/O work.
- **Workload variance over time**: an idle service and a loaded service get different coefficients as the daemon runs.

Reports where at least one service used a measured coefficient are tagged with `model = "scaphandre_rapl"`. Full precedence chain: `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v2` > `io_proxy_v1`.

**What Scaphandre does NOT do.** This is the critical limitation: **Scaphandre gives per-service coefficients, not per-finding attribution**. Specifically:

1. **RAPL is process-level, not span-level.** The metric `scaph_process_power_consumption_microwatts{exe="java"}` reports the total power draw of the `java` process. It cannot distinguish between two concurrent N+1 findings running in the same process at the same time. They share the coefficient by construction.
2. **Scrape interval is not the precision bottleneck.** A 5-second scrape window averages power over 5 seconds. Going to 1 second would not give you per-finding precision because RAPL itself averages at the 2s-Scaphandre-step granularity. The actual precision floor is "one coefficient per (service, scrape_window)".
3. **Concurrent services on the same process share nothing.** If your architecture runs multiple logical services in the same JVM, Scaphandre's `exe="java"` reading covers all of them together. perf-sentinel attributes the measured energy to whichever service name you mapped, which is a simplification.
4. **OS scheduler noise.** Per-process power attribution via `process_cpu_time / total_cpu_time` is inherently noisy under mixed loads.

**Correct mental model.** Scaphandre gives you a **dynamic, measured, service-level per-op coefficient** instead of a **fixed, proxied, global constant**. It is a meaningful improvement in the energy attribution layer of the carbon estimate stack, but it does not transform perf-sentinel into a regulatory-grade carbon accounting tool. The 2× multiplicative uncertainty bracket still applies.

**Staleness handling.** The daemon drops entries older than 3× the scrape interval when building the per-tick snapshot. A hung scraper or a service that stops emitting events will silently fall back to the proxy model after ~3 scrape intervals. The `perf_sentinel_scaphandre_last_scrape_age_seconds` Prometheus gauge lets operators set up Grafana alerts on scraper health.

**Batch mode.** `analyze` batch mode never spawns the scraper and never uses Scaphandre data. Even if `[green.scaphandre]` is present in the config, the `analyze` command skips it entirely and always uses the proxy model. Only the `watch` daemon integrates Scaphandre.

### Cloud SPECpower precision bounds

#### Platform requirements.

- A Prometheus-compatible endpoint (Prometheus, VictoriaMetrics, Thanos) that already has CPU utilization metrics from cloud provider exporters (cloudwatch_exporter, stackdriver-exporter, azure-metrics-exporter) or node_exporter.
- perf-sentinel does NOT query cloud provider APIs directly. It reads from Prometheus.

#### What cloud SPECpower improves.

The proxy model uses a fixed energy constant for all I/O operations. Cloud SPECpower replaces this with a CPU-utilization-aware estimate per service:

```
watts = idle_watts + (max_watts - idle_watts) * (cpu_percent / 100)
energy_per_op_kwh = (watts / 1000) * (interval_secs / 3600) / ops_in_window
```

This captures workload-proportional power scaling, which the fixed proxy constant cannot.

#### What cloud SPECpower does NOT do.

1. **Per-finding attribution:** like Scaphandre, this is a per-service coefficient.
2. **Memory or I/O power:** the SPECpower data captures CPU and baseboard, not storage or network.
3. **Shared tenancy correction:** the model assumes the instance's full power is attributed to perf-sentinel's traced workload.

#### Correct mental model.

Cloud SPECpower is an interpolation model with approximately +/-30% accuracy. It is a step up from the I/O proxy (order-of-magnitude estimate) but less precise than Scaphandre RAPL (direct hardware measurement).

#### Batch mode.

Cloud SPECpower is a daemon-only feature (`watch` mode). The `analyze` batch command always uses the proxy model.

### Per-operation energy coefficients

The per-operation energy multipliers (SQL verb weighting, HTTP payload size tiers) are heuristic estimates derived from academic DBMS energy benchmarks (Xu et al. VLDB 2010, Tsirogiannis et al. SIGMOD 2010) and the Cloud Carbon Footprint methodology. The relative ratios between operations (SELECT < DELETE < INSERT/UPDATE) are more robust than the absolute values, which vary across hardware generations and database engines.

Key limitations:

- **No query complexity analysis.** A full table scan SELECT costs more energy than an indexed point lookup, but both get the same 0.5x coefficient. The coefficients capture the average operation class, not the specific query plan.
- **HTTP payload size requires OTel attributes.** The `http.response.body.size` (or legacy `http.response_content_length`) attribute must be present on HTTP spans. When absent, the coefficient falls back to 1.0x (the base constant). Most HTTP instrumentation libraries do not emit this attribute by default.
- **Not used with measured energy.** When Scaphandre or cloud SPECpower provides measured per-service energy, the per-operation coefficients are ignored. This is by design: measured data is always more accurate than heuristic multipliers.

Set `per_operation_coefficients = false` to disable this feature and use the flat energy constant for all operations.

### Network transport energy

The optional network transport energy term estimates the energy cost of moving bytes between regions. The default coefficient (0.04 kWh/GB) is the midpoint of the 0.03-0.06 kWh/GB range from recent studies (Mytton, Lunden & Malmodin, J. Industrial Ecology, 2024; Sustainable Web Design, 2024).

Key limitations:

- **Wide estimate range.** Published values range from 0.06 to 0.08 kWh/GB depending on the study, year, and scope (backbone only vs. full path). The actual cost depends on the number of hops, distance, and infrastructure.
- **No CDN or compression effects.** Content delivery networks, HTTP compression, and connection reuse all reduce the effective transport energy but are not modeled.
- **Cross-region detection is config-based.** The callee region is determined by looking up the target hostname in `[green.service_regions]`. If the hostname is not mapped, perf-sentinel conservatively assumes same-region (no transport term). This means transport energy is only computed when the user explicitly configures cross-region service mappings.
- **No last-mile modeling.** The estimate covers backbone transport. The energy cost of the last mile (edge network, client device) is excluded.
- **Linear proportionality assumption.** The kWh/GB model assumes energy scales linearly with data volume. Mytton et al. (2024) show this is a simplification: network equipment has a significant fixed baseload power regardless of traffic. The estimate is directional, not precise.
- **Response body only.** Only the response body size (`http.response.body.size`) is counted. The request body (e.g., large POST payloads) is not available in standard OTel HTTP semantic conventions and is excluded. For write-heavy APIs this underestimates transport energy.
- **Caller's grid intensity used for network.** Network infrastructure is distributed across many grids, but perf-sentinel uses the caller region's carbon intensity as a proxy. This is a known simplification consistent with the directional estimation approach.

The feature is disabled by default (`include_network_transport = false`) and must be explicitly opted into.

## Chatty service detection

The chatty service detector only counts HTTP outbound spans (`type: http_out`). A trace with 15 SQL calls to the same database is not "chatty" in the inter-service sense. The threshold is per-trace, not per-endpoint: a trace that fans out across 3 endpoints each making 6 calls (18 total) will trigger at the trace level even though no single endpoint is particularly chatty.

Chatty service findings are NOT counted as avoidable I/O in the waste ratio. They represent an architectural concern (service decomposition granularity), not a batching opportunity.

## Connection pool saturation detection

The pool saturation detector uses a heuristic based on SQL span timestamp overlap, not actual connection pool metrics. It computes peak concurrency by treating each SQL span as an interval `[start, start + duration]` and running a sweep-line algorithm.

Limitations:
- Timestamps from distributed tracing may have clock skew, leading to imprecise overlap detection.
- The detector cannot distinguish between actual pool contention and intentional parallel queries (e.g., scatter-gather patterns).
- For precise monitoring, instrument your application with OTel connection pool metrics (`db.client.connection.pool.usage`, `db.client.connection.pool.wait_time`).

Pool saturation findings are NOT counted as avoidable I/O.

## Serialized calls detection

The serialized calls detector flags sequential sibling spans (same `parent_span_id`) that call different services or endpoints and could potentially be executed in parallel. Severity is `info` to reflect the inherent uncertainty.

False positive considerations:
- Sequential calls to the same service MAY have legitimate data dependencies the tool cannot observe (e.g., "create user" then "send welcome email" where the email needs the user ID).
- The detector skips sequences where all calls share the same normalized template (that pattern is N+1, not serialization).
- The `parent_span_id` field must be present on spans for this detector to work. Traces without parent-child relationships (e.g., flat JSON ingestion without span IDs) will not trigger serialized findings.

The detector reports at most one finding per parent span: the single longest non-overlapping subsequence (found via dynamic programming). If a parent has two disjoint groups of serializable calls separated by overlapping spans, only the longest group is reported.

Serialized call findings are NOT counted as avoidable I/O. They represent a latency optimization opportunity, not an I/O reduction.

## gCO2eq energy constant (legacy section, kept for cross-references)

The carbon estimation uses a fixed energy constant (`0.1 uWh per I/O operation`) as a rough order-of-magnitude approximation. See **Carbon estimates accuracy** above for the complete methodology and disclaimer.

## pg_stat_statements ingestion

- **No trace correlation.** `pg_stat_statements` data has no `trace_id` or `span_id`. It cannot be used for per-trace anti-pattern detection (N+1, redundant). It provides complementary hotspot analysis and cross-referencing with trace-based findings.
- **CSV parsing.** The CSV parser handles RFC 4180 quoting (double-quoted fields, escaped `""`), but assumes UTF-8 input. Non-UTF-8 files will fail to parse.
- **Pre-normalized queries.** PostgreSQL normalizes `pg_stat_statements` queries at the server level. perf-sentinel applies its own normalization on top for cross-referencing, which may produce slightly different templates.
- **No live connection.** perf-sentinel reads exported CSV or JSON files. It does not connect to PostgreSQL directly.
- **Entry count.** The parser pre-allocates memory based on input size, capped at 100,000 entries. Files exceeding 1,000,000 entries (CSV rows or JSON array elements) are rejected with an error to prevent memory exhaustion.
