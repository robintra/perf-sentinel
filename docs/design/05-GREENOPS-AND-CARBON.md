# GreenOps scoring and carbon conversion

## I/O Intensity Score (IIS)

The central metric is the I/O Intensity Score: the number of I/O operations generated per user request for a given endpoint.

```
IIS(endpoint) = total_io_ops(endpoint) / invocation_count(endpoint)
```

An endpoint called across 3 traces with 18 total I/O operations has `IIS = 18 / 3 = 6.0`. This normalizes across different traffic volumes: a high-traffic endpoint with 1000 invocations and 6000 I/O ops has the same IIS (6.0) as a low-traffic one with 3 invocations and 18 ops.

The denominator uses `.max(1)` as a safety guard against division by zero, though this case cannot occur in practice (an endpoint that appears in `endpoint_stats` must have been seen in at least one trace).

## Scoring algorithm: five steps

### Step 1: endpoint statistics

```rust
for (trace_idx, trace) in traces.iter().enumerate() {
    for span in &trace.spans {
        total_io_ops += 1;
        let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats {
            total_io_ops: 0,
            invocation_count: 0,
            last_seen_trace: usize::MAX,
        });
        stats.total_io_ops += 1;
        if stats.last_seen_trace != trace_idx {
            stats.invocation_count += 1;
            stats.last_seen_trace = trace_idx;
        }
    }
}
```

**Single pass with per-trace sentinel:** `invocation_count` is bumped the first time a `(service, endpoint)` pair is seen within a given trace, then `last_seen_trace` is set to block further bumps in that same trace. Initializing the sentinel to `usize::MAX` (rather than `0`) keeps trace index `0` valid as a "first touch" marker. This avoids a second `get_mut` pass over a per-trace `HashSet` (one fewer `HashMap` probe per `(trace, endpoint)` pair).

**`EndpointStats<'a>` with borrowed `service`:** the `service` field borrows `&'a str` from the span events instead of cloning the String. The clone only happens later when building `TopOffender` structs for the output. This avoids one String clone per unique endpoint in the inner loop.

**Backing structure (`HashMap + sort` vs `BTreeMap`):** the per-endpoint map is a `HashMap` finalized with a single `sort_by` for the public view, not a `BTreeMap`. Under perf-sentinel's access pattern (many spans per unique endpoint, small K relative to N), measurements on 1M spans showed `HashMap + sort` consistently faster:

| Endpoint cardinality | Spans | `HashMap + sort` | `BTreeMap` | Ratio |
|---------------------:|------:|-----------------:|-----------:|------:|
|                   16 |    1M |            15 ms |      19 ms | 1.24x |
|                   64 |    1M |            16 ms |      31 ms | 1.94x |
|                  256 |    1M |            17 ms |      49 ms | 2.89x |
|                 1024 |    1M |            18 ms |      73 ms | 3.99x |

`BTreeMap`'s free-sort-on-iteration is dwarfed by its per-insert `O(log K)` overhead. The terminal sort is `O(K log K)` on small K (20-90 µs across the whole range), negligible next to the insert volume.

### Step 2: dedup avoidable I/O

```rust
let mut dedup: HashMap<(&str, &str, &str), usize> = HashMap::with_capacity(findings.len());
for f in &findings {
    if matches!(f.finding_type, FindingType::SlowSql | FindingType::SlowHttp) {
        continue; // slow findings are not avoidable
    }
    let avoidable = f.pattern.occurrences.saturating_sub(1);
    let entry = dedup.entry((&f.trace_id, &f.pattern.template, &f.source_endpoint)).or_insert(0);
    *entry = (*entry).max(avoidable);
}
```

**Why include `source_endpoint` in the key?** The same SQL template (e.g., `SELECT * FROM config WHERE key = ?`) may be called from two different endpoints in the same trace. Each endpoint's avoidable ops should be counted independently. Without `source_endpoint`, `max(5, 3) = 5` would undercount, the correct total is `5 + 3 = 8`.

**Why `max()` instead of `sum()`?** Within the same (trace, template, endpoint), both N+1 and redundant detectors may fire on overlapping sets of spans. Taking the max prevents double-counting: if N+1 reports 9 avoidable and redundant reports 4 avoidable for the same group, the true avoidable count is 9 (the larger set already includes the smaller one).

**Slow findings excluded:** slow queries are necessary operations that happen to be slow. They need optimization (indexing, caching), not elimination. Including them in the waste ratio would conflate "wasteful I/O" with "slow I/O".

### Step 3: compute IIS per endpoint

```rust
let iis_map: HashMap<&str, f64> = endpoint_stats.iter()
    .map(|(&ep, stats)| {
        let invocations = stats.invocation_count.max(1) as f64;
        (ep, stats.total_io_ops as f64 / invocations)
    })
    .collect();
```

The IIS map is computed once and reused for both finding enrichment (step 4) and top offender ranking (step 5).

### Step 4: enrich findings

Each finding receives a `GreenImpact`:

```rust
GreenImpact {
    estimated_extra_io_ops: if slow { 0 } else { occurrences - 1 },
    io_intensity_score: iis,
}
```

### Step 5: top offenders

Sorted by IIS descending, with alphabetical tiebreaker for deterministic output:

```rust
top_offenders.sort_by(|a, b| {
    b.io_intensity_score.partial_cmp(&a.io_intensity_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.endpoint.cmp(&b.endpoint))
});
```

`partial_cmp` with `unwrap_or(Equal)` handles `NaN` safely, though NaN cannot occur since the denominator is always >= 1.0.

## I/O waste ratio

```
waste_ratio = avoidable_io_ops / total_io_ops
```

When `total_io_ops == 0`, the ratio is `0.0` (not NaN). This is the fraction of I/O operations that could be eliminated by fixing detected anti-patterns. It aligns with the **Energy** component of the [SCI model (ISO/IEC 21031:2024)](https://sci-guide.greensoftware.foundation/) from the [Green Software Foundation](https://greensoftware.foundation/): reducing unnecessary computation reduces energy consumption.

## Carbon conversion

The scoring pipeline resolves two dimensions independently for every span: **energy per op** (`E`) and **grid intensity** (`I`). Both have fallback chains from the highest-fidelity source down to the embedded defaults.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/carbon-scoring_dark.svg">
  <img alt="Carbon scoring energy and intensity resolution" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/carbon-scoring.svg">
</picture>

### SCI v1.0 alignment

perf-sentinel implements the [Software Carbon Intensity v1.0](https://sci-guide.greensoftware.foundation/) specification (later [ISO/IEC 21031:2024](https://www.iso.org/standard/86612.html)) from the Green Software Foundation. The formula is:

```
SCI = ((E × I) + M) per R
```

Where:
- **`E`** = energy consumed by the workload (kWh)
- **`I`** = location-based carbon intensity of the grid (gCO₂eq/kWh)
- **`M`** = embodied emissions from hardware manufacturing, amortized
- **`R`** = functional unit (the "per X" denominator)

In perf-sentinel:
- **`R = 1 trace`**: one user-facing request. Each correlated trace is one functional unit.
- **`E = io_ops × ENERGY_PER_IO_OP_KWH`**: proxy from I/O op count.
- **`I = lookup_region(region).intensity`**: from the embedded carbon table.
- **`M = traces.len() × embodied_per_request_gco2`**: configurable, default 0.001 g/req.

### Energy constant

```rust
pub const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1; // 0.1 uWh per I/O op
```

This is a rough order-of-magnitude approximation, not a measured value. It accounts for a typical database query or HTTP round-trip on cloud infrastructure. The [Cloud Carbon Footprint project](https://www.cloudcarbonfootprint.org/docs/methodology/) uses a similar approach of estimating energy from resource usage rather than direct measurement.

The value must be disclosed as methodology per SCI requirements. It is documented in the code, in [LIMITATIONS.md](../LIMITATIONS.md) and here.

### Embodied carbon (`M` term)

```rust
pub const DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2: f64 = 0.001;
```

The default of `0.001 gCO₂/request` is derived from typical server lifecycle assumptions:

- A modern x86 server has an embodied carbon footprint of **~1000 kgCO₂eq** over a 4-year lifecycle (sources: [Boavizta API](https://doc.api.boavizta.org/) lifecycle assessments, [Cloud Carbon Footprint methodology](https://www.cloudcarbonfootprint.org/docs/methodology/)).
- 4 years × 365 days × 86400 seconds × 1 request/sec ≈ 126 million requests amortized per server.
- 1000 g per server / 126e6 requests ≈ **0.000008 gCO₂/req** (8e-6 g) at 1 req/sec, scaling to ~0.001 at lower request rates or larger / less amortized hardware.

The `0.001 g/req` default is a **conservative upper bound for lightly-loaded microservice servers**. AWS Customer Carbon Footprint methodology (2025) reports ~320 kgCO2eq/year for a Dell R640, which at typical utilization rates yields 10-50 ugCO2/req, 10-20x below our default. Users with measured infrastructure data should lower this value via `[green] embodied_carbon_per_request_gco2`.

**Embodied is region-independent.** Hardware manufacturing emissions don't vary by deployment location. perf-sentinel emits embodied carbon unconditionally when green scoring is enabled, even when no region resolves, so users see at least a floor estimate.

### Conversion formula

For each region bucket:
```
operational_region = io_ops_in_region × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE
```

Total operational across all regions:
```
operational_gco2 = Σ operational_region
```

Embodied:
```
embodied_gco2 = traces.len() × embodied_per_request_gco2
```

Total CO₂ midpoint:
```
total.mid = operational_gco2 + embodied_gco2
```

Avoidable CO₂ (via ratio, see "Avoidable via ratio" below):
```
accounted_io_ops = total_io_ops - unknown_ops
avoidable.mid = operational_gco2 × (avoidable_io_ops / accounted_io_ops)
```

Note the denominator: `accounted_io_ops` excludes the synthetic `unknown` bucket so the ratio is consistent with `operational_gco2` (which also excludes it). This keeps the numerator and denominator on the same accounting basis.

Uncertainty bracket (2× multiplicative, not arithmetic ±50%):
```
total.low  = total.mid × 0.5    // mid divided by 2
total.high = total.mid × 2.0    // mid multiplied by 2
(same for avoidable.low / avoidable.high)
```

This is a **log-symmetric interval**: the geometric mean of `low` and `high` equals `mid`. The 2× framing matches the order-of-magnitude uncertainty of the I/O proxy model better than a symmetric ±50% window would. See "Uncertainty framing" below.

Where:
- `carbon_intensity` = gCO₂eq/kWh for the region's electricity grid
- `PUE` = Power Usage Effectiveness (datacenter overhead factor)

### SCI v1.0 semantics: numerator vs intensity

The SCI v1.0 specification defines `SCI = ((E × I) + M) / R`, an **intensity** expressed per functional unit R. perf-sentinel reports the **numerator** of this formula, summed over all analyzed traces:

```
co2.total.mid = Σ operational_gco2 + embodied_gco2
              = (E × I) + M   (summed over analyzed traces)
```

This is a **footprint** (absolute gCO₂eq), not an intensity score. Consumers who want the per-request SCI intensity compute it downstream:

```
sci_per_trace = co2.total.mid / analysis.traces_analyzed
```

To tag this semantic distinction at the data layer, `CarbonEstimate` carries a `methodology` field with two possible values:

- `"sci_v1_numerator"`: used on `co2.total`. The `(E × I) + M` footprint summed over traces.
- `"sci_v1_operational_ratio"`: used on `co2.avoidable`. The region-blind global ratio `operational × (avoidable/accounted)`, excluding embodied carbon.

The two distinct values signal to downstream consumers that `total` and `avoidable` are computed differently and should not be compared as if they were homogeneous quantities.

### Avoidable via ratio (design choice)

Computing avoidable CO₂ accurately per-region would require threading region resolution through the finding dedup phase (which currently aggregates avoidable I/O ops globally by `(trace_id, template, source_endpoint)`). This is complex and error-prone.

Instead, perf-sentinel computes:

```
avoidable.mid = operational_gco2 × (avoidable_io_ops / accounted_io_ops)
```

This preserves the **relative scale** (a 50% waste reduction yields a 50% drop in avoidable CO₂) without requiring per-finding region attribution. The trade-off: when avoidable ops are concentrated in a high-intensity region, this ratio slightly under-attributes the savings. The simplification is documented as a known limitation and tagged at the data layer via `methodology: "sci_v1_operational_ratio"`.

**Embodied carbon is excluded from avoidable.** You can't optimize away manufactured silicon by fixing N+1 queries: embodied emissions are fixed per request regardless of how efficient the application is. The avoidable estimate only considers the operational term.

### Multi-region resolution

Each span resolves to an effective region via a 3-level chain (first match wins):

1. **`event.cloud_region`**: extracted from the OTel `cloud.region` resource attribute (with span-level fallback for SDKs that put it on individual spans). Most authoritative. Values are sanitized at the ingest boundary: invalid region strings (non-ASCII-alphanumeric-dash-underscore, length > 64 or empty) are silently dropped.
2. **`[green.service_regions][event.service.to_lowercase()]`**: config override for environments where OTel doesn't provide it (e.g. Jaeger / Zipkin ingestion). Case-insensitive (config loader lowercases keys).
3. **`[green] default_region`**: global fallback.

Spans with no resolvable region land in a synthetic `"unknown"` bucket: zero operational CO₂ contribution. The `regions[]` breakdown still shows the bucket so users see the orphan I/O ops (the visible signal for troubleshooting; detailed `tracing::debug!` messages are available via `RUST_LOG=debug`).

**Region cardinality cap.** The per-region BTreeMap is capped at 256 distinct regions in one scoring pass (`MAX_REGIONS` constant). Excess distinct region strings fold into the `unknown` bucket, preventing memory exhaustion from attacker-controlled or misconfigured OTLP `cloud.region` attributes.

**TopOffender scalar CO₂ in multi-region mode.** When multi-region scoring is active (either `[green.service_regions]` is non-empty or any span carries `cloud.region`), the `top_offenders[].co2_grams` scalar is set to `None` across the board. Computing it from `default_region` only would be inconsistent with the per-region breakdown; users should rely on `green_summary.regions[]` for per-region attribution in multi-region deployments.

### Uncertainty framing: 2× multiplicative, not ±50%

Every CO₂ estimate is reported as `{ low, mid, high }`:

```rust
pub struct CarbonEstimate {
    pub low: f64,           // mid × 0.5
    pub mid: f64,           // best estimate
    pub high: f64,          // mid × 2.0
    pub model: &'static str,       // "io_proxy_v1"
    pub methodology: &'static str, // "sci_v1_numerator" or "sci_v1_operational_ratio"
}
```

The factors `0.5` and `2.0` encode a **2× multiplicative uncertainty bracket** around the midpoint:

```
geometric_mean(low, high) = sqrt(low × high) = sqrt(mid² × 0.5 × 2.0) = mid
```

This is a **log-symmetric interval**: the mid is the geometric center, not the arithmetic center. The spread between `low` and `high` is a factor of 4 (high/low = 4), which is wider than a symmetric ±50% window (which would give high/low = 3).

**Why 2× and not ±50%?** The I/O proxy model has order-of-magnitude uncertainty at each step:
- `ENERGY_PER_IO_OP_KWH = 0.1 µWh/op` is an order-of-magnitude approximation.
- Grid intensity values from CCF/Electricity Maps are annual averages; real-time intensity varies 2-3× over a day.
- PUE values are provider averages; individual datacenters vary.
- Embodied carbon assumes a conservative server-lifecycle figure that may be off by an order of magnitude for specific hardware.

A symmetric ±50% window (giving high = 1.5 × mid) would understate this real uncertainty. The 2× multiplicative framing is deliberately chosen to be honest: the true value is within a factor of 2 of `mid`, in either direction.

The bounds reflect aggregate model uncertainty, **not** per-endpoint variance. The model doesn't have enough resolution to distinguish per-endpoint precision.

### Model versioning

The `model: "io_proxy_v1"` field versions the estimation methodology. Future improvements (per-operation weighting, hourly carbon profiles, RAPL integration) will bump this version, allowing downstream consumers to track which methodology produced a given report.

### Region lookup

The carbon intensity table is embedded as a static array and converted to a `HashMap` via `LazyLock`:

```rust
static REGION_MAP: LazyLock<HashMap<&'static str, (f64, Provider)>> =
    LazyLock::new(|| CARBON_TABLE.iter().map(...).collect());
```

**Why `LazyLock<HashMap>` instead of a linear scan?** The original implementation scanned all 41 entries on every call. With the HashMap, lookup is O(1). The initialization cost is paid once on first access.

**Case-insensitive lookup:** the public `lookup_region()` lowercases the input via `to_ascii_lowercase()` before lookup. All table keys are stored in lowercase. The multi-region scoring stage uses a `BTreeMap<String, usize>` (not `HashMap`) to bucket I/O ops per resolved region. This guarantees deterministic iteration order and stable floating-point sums across runs.

### PUE values

| Provider | PUE   | Source                                                                                                                                                    |
|----------|-------|-----------------------------------------------------------------------------------------------------------------------------------------------------------|
| AWS      | 1.135 | [AWS Sustainability](https://sustainability.aboutamazon.com/)                                                                                             |
| GCP      | 1.10  | [Google Environmental Report](https://sustainability.google/reports/)                                                                                     |
| Azure    | 1.185 | [Microsoft Sustainability Report](https://www.microsoft.com/en-us/corporate-responsibility/sustainability)                                                |
| Generic  | 1.2   | [Uptime Institute Global Survey 2023](https://uptimeinstitute.com/resources/research-and-reports/uptime-institute-global-data-center-survey-results-2023) |

PUE (Power Usage Effectiveness) measures the ratio of total datacenter energy to IT equipment energy. A PUE of 1.10 means 10% overhead for cooling, lighting and infrastructure. The industry average is ~1.58, but hyperscale cloud providers achieve significantly lower values.

### Carbon intensity data

Regional grid carbon intensities (gCO2eq/kWh) are sourced from [Electricity Maps](https://www.electricitymaps.com/) annual averages (2023-2024) and the [Cloud Carbon Footprint](https://www.cloudcarbonfootprint.org/) project. The table covers 15 AWS regions, 8 GCP regions, 6 Azure regions and 14 ISO country codes.

When the configured region is not found in the table, CO2 fields are omitted from the report (no default value is invented).

## Hourly carbon intensity profiles

The flat annual value per region discards the diurnal variance that can be large in grids with a high share of variable renewables or strong demand peaks. To capture that variance, perf-sentinel embeds a 24-value UTC profile per region for four regions with well-documented diurnal shapes:

- **France (`eu-west-3`)**: nuclear baseload, flat-with-evening-peak shape.
- **Germany (`eu-central-1`)**: coal + gas + variable renewables, strong morning/evening peaks.
- **UK (`eu-west-2`)**: wind + gas, moderate twin peaks.
- **US-East (`us-east-1`)**: gas + coal, flat daytime peak 13h-18h UTC (9am-2pm ET).

Each profile's arithmetic mean approximates the corresponding flat annual value within ±5%, preserving methodology continuity: enabling hourly profiles should not cause a sudden jump in the reported CO₂ for a representative-day run. Exception: Germany (`eu-central-1`) where the profile mean (~442 gCO₂/kWh) is ~31% above the embedded annual value (338), reflecting recent 2023-2024 data that is higher than the older CCF annual value. Users needing exact calibration can disable hourly profiles with `use_hourly_profiles = false`.

Sources: Electricity Maps annual open-data reports (2023-2024 typical diurnal shapes by zone), ENTSO-E Transparency Platform (European grid composition and demand curves), RTE eco2mix daily data (France), Fraunhofer ISE Energy-Charts (Germany), NGESO carbonintensity.org.uk (UK), EIA hourly generation data (US-East).

The table intentionally does **not** embed monthly profiles (24x12). The additional 12x data for seasonal variance provides marginal accuracy gain compared to the complexity cost. The `IntensitySource` tag already distinguishes annual vs hourly, so extending to monthly later would be backward-compatible.

The scoring path walks each span once and dispatches between three intensity sources:

```rust
let intensity_used = if ctx.use_hourly_profiles
    && hourly_profile_for_region_lower(region).is_some()
    && let Some(hour) = time::parse_utc_hour(&span.event.timestamp)
{
    lookup_hourly_intensity_lower(region, hour).unwrap_or(annual_intensity)
} else {
    annual_intensity
};
```

When the dispatch selects the hourly path for a region, the region's `RegionBreakdown` row is tagged `intensity_source: "hourly"` and the top-level `CarbonEstimate.model` flips from `"io_proxy_v1"` to `"io_proxy_v2"`. If the same report contains regions that went through the flat path, those regions stay tagged `intensity_source: "annual"` while the top-level model still reads `"io_proxy_v2"`. The tag records "the most precise model used anywhere in the run".

**Self-consistency of breakdown rows.** The identity `co2_gco2 ≈ io_ops × grid_intensity_gco2_kwh × pue × ENERGY_PER_IO_OP_KWH` holds only in the proxy-energy case (no Scaphandre/cloud snapshot). When measured energy is present and services within the same region use different coefficients, the displayed `grid_intensity_gco2_kwh` is still the ops-weighted mean intensity, but the per-op energy varies per service, making the identity approximate.

**Timestamps must be UTC.** `parse_utc_hour` rejects non-UTC offset forms (`+02:00`, `-05:00`) rather than silently shifting them, because the embedded profile is UTC-anchored. Spans with unparseable timestamps fall back to the flat annual intensity for the region.

**Sum-then-divide invariant (defence against dedup drift).** A single `compute_operational_gco2(io_ops, intensity, pue)` helper prevents the formula from being re-implemented inconsistently across paths. This is extended with a lower-level `per_op_gco2(energy_kwh, intensity, pue)` helper that is the single source of truth for the `energy × intensity × pue` multiplication. All three paths (proxy, hourly, Scaphandre) go through this helper. The bulk helper is implemented as `io_ops × per_op_gco2(ENERGY_PER_IO_OP_KWH, intensity, pue)`.

## Scaphandre per-process energy integration

The proxy model uses a fixed `ENERGY_PER_IO_OP_KWH` constant (0.1 µWh per op). This is a two-order-of-magnitude approximation and it treats all services and all workload shapes identically. perf-sentinel offers opt-in support for replacing the proxy with a measured service-level coefficient derived from [Scaphandre's](https://github.com/hubblo-org/scaphandre) per-process power readings.

**How it fits the architecture.** Scaphandre is an external, user-installed process. perf-sentinel does NOT bundle or fork Scaphandre. It scrapes the Prometheus `/metrics` endpoint Scaphandre already exposes. The `score/scaphandre/` module owns:

- `ScaphandreConfig`: parsed from `[green.scaphandre]` in `.perf-sentinel.toml`.
- `ScaphandreState`: backed by `ArcSwap<HashMap<String, ServiceEnergy>>` for lock-free reads from the scoring path. The scraper builds a fresh `Arc<HashMap>` on each successful scrape and atomically swaps it in; readers do a single `load_full()` to get their own `Arc` reference without contending on a lock.
- `spawn_scraper()`: a tokio task that runs every `scrape_interval_secs` and updates the state.
- `parse_scaphandre_metrics()`: escape-aware Prometheus text parser. Iterates by `.chars()` for UTF-8 safety. Has a fast path that avoids allocation when no backslash escapes are present in label values. Handles `\"` and `\\` sequences inside label blocks.
- `OpsSnapshotDiff`: a snapshot-diff helper that reads the per-service op counts from `MetricsState::service_io_ops_total` and computes the delta since the previous scrape.
- `apply_scrape()`: applies the parsed power readings + op deltas to the state using the formula below.

**The formula.** For each mapped service in a scrape window:

```
power_watts       = process_power_microwatts / 1_000_000
joules            = power_watts × scrape_interval_secs
kwh               = joules / 3_600_000
energy_per_op_kwh = kwh / ops_observed_in_window
```

When `ops_observed_in_window == 0`, the existing state entry is **kept** unchanged rather than cleared. This avoids model-tag flapping for idle services. The staleness threshold (3× the scrape interval) guards against stuck scrapers.

**Where the coefficient plugs in.** The daemon takes a synchronous snapshot of all energy sources at the start of each `process_traces` tick via `build_tick_ctx`. This merged map is attached to `CarbonContext.energy_snapshot` for the duration of the tick. Each `EnergyEntry` carries both the coefficient and a model tag (`"scaphandre_rapl"` or `"cloud_specpower"`). Inside `compute_carbon_report`'s span loop, the per-op energy is resolved as:

```rust
let (energy_kwh, measured_model) = match &ctx.energy_snapshot {
    Some(snapshot) => match snapshot.get(&span.event.service) {
        Some(entry) => (entry.energy_per_op_kwh, Some(entry.model_tag)),
        None => (ENERGY_PER_IO_OP_KWH, None),
    },
    None => (ENERGY_PER_IO_OP_KWH, None),
};
let op_co2 = per_op_gco2(energy_kwh, intensity_used, pue);
```

The scoring stage tracks per-region flags (`any_scaphandre`, `any_cloud_specpower`, `any_realtime_report`) and the top-level `CarbonEstimate.model` reflects the most precise source used: `"electricity_maps_api"` > `"scaphandre_rapl"` > `"cloud_specpower"` > `"io_proxy_v3"` > `"io_proxy_v2"` > `"io_proxy_v1"`. When calibration factors are active on proxy models, `+cal` is appended. All energy sources compose naturally with hourly profiles: a measured-energy op in eu-west-3 at 3am UTC uses the measured energy AND the hourly intensity simultaneously.

**Per-service op counter as single source of truth.** The scraper reads the per-service op counter from `MetricsState::service_io_ops_total` (a Prometheus `CounterVec` labeled with `service`) via `snapshot_service_io_ops()`. The daemon's event intake path increments this counter on every normalized event. Using the Prometheus counter directly, instead of a parallel counter that would need resetting every scrape window, avoids reset races and gives Grafana users a per-service op rate graph for free.

**Graceful shutdown.** The daemon captures the scraper `JoinHandle` and calls `.abort()` on it before the final `process_traces` drain in the Ctrl-C arm. This prevents "scrape failed" log lines from appearing after the "Shutting down daemon" message.

**What Scaphandre does NOT do.** See the `Scaphandre precision bounds` section in `docs/LIMITATIONS.md` for the full discussion. Short version: Scaphandre gives per-service coefficients, not per-finding attribution. Two N+1 findings in the same JVM during the same scrape window share the same coefficient by construction, because RAPL is process-level not span-level.

## Cloud-native energy estimation (CPU% + SPECpower)

For cloud VMs (AWS, GCP, Azure) that do not expose Intel RAPL to guests, perf-sentinel offers an alternative energy estimation path based on CPU utilization metrics and the SPECpower model. The module lives in `score/cloud_energy/` and mirrors the Scaphandre module structure.

**Architecture.** The `cloud_energy/` directory contains:

- `config.rs`: `CloudEnergyConfig` and per-service `ServiceCloudConfig` (provider, region, instance_type, optional idle/max watts overrides).
- `table.rs`: embedded SPECpower lookup table with idle and max watt values for ~60 common instance types across AWS (c5, m5, r5, t3 families), GCP (n2, e2, c2 families) and Azure (D, E, F series). Data sourced from Cloud Carbon Footprint.
- `scraper.rs`: Prometheus JSON API scraper. Queries `avg(rate(cpu_metric[interval]))` per service, fetches JSON from the Prometheus endpoint.
- `state.rs`: `CloudEnergyState` backed by `ArcSwap` for lock-free reads from the scoring path.
- `mod.rs`: re-exports and module documentation.

**The formula.** For each service with a cloud config:

```
cpu_percent       = prometheus_query(cpu_metric, service_label)
watts             = idle_watts + (max_watts - idle_watts) * (cpu_percent / 100)
joules            = watts * scrape_interval_secs
kwh               = joules / 3_600_000
energy_per_op_kwh = kwh / ops_in_window
```

`idle_watts` and `max_watts` come from the SPECpower table lookup by instance type or from user-provided overrides in the config. The op count comes from the same `MetricsState::service_io_ops_total` counter used by Scaphandre.

**Config example.**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"
default_instance_type = "c5.xlarge"
cpu_metric = "node_cpu_seconds_total"

[green.cloud.services.api-us]
provider = "aws"
region = "us-east-1"
instance_type = "c5.4xlarge"

[green.cloud.services.api-eu]
provider = "gcp"
region = "europe-west1"
instance_type = "n2-standard-8"
```

**Model tag and precedence.** The coefficient carries model tag `"cloud_specpower"`. In `build_tick_ctx`, Scaphandre entries take precedence: if both Scaphandre and cloud energy exist for the same service, the Scaphandre entry wins (it measures real power, the cloud entry interpolates). The top-level model tag reflects the most precise source: `electricity_maps_api` > `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.

**Daemon only.** Like Scaphandre, cloud energy estimation is a daemon-only feature. The `analyze` batch command always uses the proxy model.

**What cloud SPECpower does NOT do.** See `docs/LIMITATIONS.md` "Cloud SPECpower precision bounds" for the full discussion. The SPECpower model captures CPU-proportional power but not memory, I/O or network power. Shared tenancy is not corrected. Accuracy is approximately +/-30%.

## Electricity Maps real-time intensity integration

The `[green.electricity_maps]` block enables real-time grid carbon intensity polling. The daemon scraper periodically queries the Electricity Maps `/carbon-intensity/latest` endpoint per zone and feeds the result into the per-tick `CarbonContext`, where it overrides annual and hourly profiles for matched cloud regions. Documented at <https://app.electricitymaps.com/developer-hub/api/getting-started>.

**Per-zone deduplication.** The scraper iterates over `region_map` (`cloud_region -> zone`) but a single zone is fetched at most once per tick, even when several `cloud_region` keys point to the same zone (typical multi-AZ setups, or `aws:eu-west-3` and `local-k3d` both pinned to `FR`). The reading is then dispatched to every matching `cloud_region`. This keeps the API call count proportional to the number of distinct zones, not to the size of `region_map`. Critical on quota-constrained tiers, the free tier especially is capped at one zone today but quota math still benefits when the same zone-mapping appears across staging plus prod.

**Estimation metadata.** The Electricity Maps API surfaces two optional fields alongside `carbonIntensity`:

```json
{
  "zone": "FR",
  "carbonIntensity": 56.0,
  "isEstimated": true,
  "estimationMethod": "TIME_SLICER_AVERAGE"
}
```

`isEstimated` is `true` when the API filled a gap (Tier B/C zone, or temporal hole bridged by an algorithm such as `TIME_SLICER_AVERAGE`), and `false` for fully measured values. perf-sentinel parses both fields with `#[serde(default)]` to stay forward-compatible if a future API version stops emitting them.

The flags propagate through `IntensityReading` (state) into the per-tick `CarbonContext.real_time_intensity` and finally into the per-region accumulator. The `green_summary.regions[]` row exposes them as two optional fields:

```json
{
  "status": "known",
  "region": "eu-west-3",
  "intensity_source": "real_time",
  "grid_intensity_gco2_kwh": 56.0,
  "intensity_estimated": true,
  "intensity_estimation_method": "TIME_SLICER_AVERAGE",
  "co2_gco2": 1.234
}
```

Both fields use `#[serde(skip_serializing_if = "Option::is_none")]` so consumers that ignore them continue to deserialize the row unchanged. The fields only appear when `intensity_source == "real_time"`. Spans falling back to annual or hourly profiles never carry the metadata, even if the accumulator captured it from a sibling span.

This is the signal Scope 2 reports need to distinguish measured emissions from modeled ones. Auditors typically allow estimated values when the methodology is documented, surfacing the algorithm tag (`TIME_SLICER_AVERAGE`, `GENERAL_PURPOSE_ZONE_DEVELOPMENT`, etc.) makes the audit trail self-contained.

### User-facing rendering (0.5.10)

The two fields are surfaced in the two user-visible rendering layers so operators read the distinction at a glance.

**Dashboard.** The Regions table in the GreenOps tab carries a sixth column `Estimated`. Three visual states: an orange `Estimated` badge when `intensity_estimated == true` (hover surfaces a tooltip with the `intensity_estimation_method`), a green `Measured` badge when `intensity_estimated == false`, a neutral dash for rows whose `intensity_source` is not `real_time` (annual / hourly / monthly_hourly profiles carry no estimation metadata, the field stays `None` end-to-end). Both badges reuse the existing palette CSS variables (`--color-background-warning`, `--color-text-warning`, `--color-background-success`, `--color-text-success`) so dark and light themes adapt automatically.

**Terminal.** The `print_green_summary` per-region line gains a suffix after the `source: real_time` field. Format:

```
- fr: 42 I/O ops, 0.000123 gCO₂ (56 gCO₂/kWh, source: real_time, estimated/TIME_SLICER_AVERAGE)
- de: 24 I/O ops, 0.000456 gCO₂ (380 gCO₂/kWh, source: real_time, measured)
- us-east-1: 12 I/O ops, 0.000789 gCO₂ (410 gCO₂/kWh, source: annual)
```

The suffix is empty when `intensity_estimated` is `None`, so existing log scrapers keep matching pre-0.5.10 line shapes.

### API version (0.5.11)

perf-sentinel targets the `Electricity Maps` API v4 endpoint by default since 0.5.11. Earlier releases defaulted to v3, which Electricity Maps still serves but considers legacy. The migration was triggered by the v4 promotion to "latest" in the developer hub reference (<https://app.electricitymaps.com/developer-hub/api/reference>) and is forward-defense against an eventual v3 retirement.

The response schema on the `carbon-intensity/latest` endpoint is byte-identical between v3 and v4, so the migration is transparent for downstream consumers (`green_summary.regions[]` rows are unchanged regardless of the configured API version, the parsing path is the same struct).

Backward compatibility: existing `.perf-sentinel.toml` configs that pin `endpoint = "https://api.electricitymaps.com/v3"` keep working. The scraper detects the legacy path at startup via `ApiVersion::from_endpoint` (matches `.../v3` at end of URL or `.../v3/...` in path, with word-boundary guards against false positives like `/v30` or `/v300`) and emits a `tracing::warn!` message once per daemon start, pointing the operator to the v4 migration. Since 0.5.12 `ApiVersion::from_endpoint` is the single source of truth and is also consumed by the `green_summary.scoring_config.api_version` field. The endpoint string flows through `sanitize_for_terminal` before being logged so a hostile TOML cannot inject ANSI control bytes into the daemon log stream.

### Scoring config transparency (0.5.12)

The `green_summary.scoring_config` object exposes the runtime configuration of the Electricity Maps integration so auditors and Scope 2 reporters can see which carbon model produced the numbers without reading the operator's TOML. Three fields, each derived from `ElectricityMapsConfig` at config load time via `ScoringConfig::from_electricity_maps`:

- `api_version`: detected from `api_endpoint` via `ApiVersion::from_endpoint`. One of `v3` (legacy), `v4` (default), `custom` (proxy or mock without `/vN` suffix).
- `emission_factor_type`: mirrors the TOML knob, one of `lifecycle` (default) or `direct`.
- `temporal_granularity`: mirrors the TOML knob, one of `hourly` (default), `5_minutes`, `15_minutes`.

**Scope of the surface.** `scoring_config` captures the Electricity Maps **client configuration only**. It is a partial methodology footprint, not the full SCI input vector. A complete strict-replay of the carbon math from a saved baseline would also need `[green] embodied_carbon_per_request_gco2`, `[green] use_hourly_profiles`, `[green] per_operation_coefficients`, `[green] include_network_transport` and `[green] network_energy_per_byte_kwh` (none of which are in the JSON today), plus the per-region PUE drawn from the embedded provider table (recoverable only if the Provider classification is stable across runs). Surfacing the complete methodology footprint is tracked as future work, the 0.5.12 surface closes the audit gap on the Electricity Maps slice specifically because that is the slice the 0.5.10 + 0.5.11 work added knobs to without surfacing them.

**Backward compat.** The field is `None` (and the dashboard bandeau / terminal line are hidden) when `[green.electricity_maps]` is not configured, so reports produced without Electricity Maps stay shape-identical to pre-0.5.12. The wire form is additive on the JSON `green_summary` via `#[serde(skip_serializing_if = "Option::is_none", default)]`, so pre-0.5.12 baselines fed back through `report --before` keep parsing.

**Threading.** `Config::carbon_context()` populates `CarbonContext::scoring_config: Option<ScoringConfig>` from the loaded `green_electricity_maps`. `score_green` reads it from the context and copies it into the resulting `GreenSummary`. The daemon's per-tick `build_tick_ctx` inherits the field via the existing `Cow::Owned(ctx)` clone path, no per-tick rebuild. The CLI batch pipeline gets it directly from the once-built `CarbonContext`.

**Daemon snapshot path.** Since 0.5.13, `/api/export/report` serves a live `green_summary` refreshed by the event loop after each batch (regions, top offenders, avoidable I/O ratio, CO2 numbers). `scoring_config` is stitched on top from the daemon's startup `Config`, so the audit chip and the GreenOps tab both surface on the rendered HTML when an operator pipes the snapshot through `perf-sentinel report --input -`. The earlier 0.5.12 limitation (snapshot returned `GreenSummary::disabled(0)` and only the `scoring_config` field was patched, hiding the GreenOps tab) is removed.

**Defense against terminal injection:** the three fields are typed Rust enums with bounded variants, so the terminal renderer in `print_green_summary` does not need to wrap them in `sanitize_for_terminal` (unlike `intensity_estimation_method` which carries a free-form `String` from `--input` JSON). The HTML chip rendering uses `textContent` (not `innerHTML`) and `setAttribute("title", ...)`, both of which auto-escape.

## Per-operation energy coefficients

The proxy model uses a single `ENERGY_PER_IO_OP_KWH` constant (0.1 uWh) for every I/O operation. This treats a read-only `SELECT` hitting an index the same as a disk-heavy `INSERT` writing to WAL and data pages. The per-operation coefficient feature refines this by applying a multiplier based on the operation type.

**SQL verb multipliers.** The verb is extracted from the first word of the `target` field (the raw SQL statement), not from the `operation` field. This is necessary because OTLP-ingested spans store `db.system` (e.g., "postgresql") in `operation`, not the SQL verb. The first whitespace-delimited token reliably gives the SQL verb across all ingestion formats (native JSON, OTLP, Jaeger, Zipkin).

| SQL verb | Multiplier | Rationale                            |
|----------|------------|--------------------------------------|
| SELECT   | 0.5x       | Read-only index lookup, no WAL write |
| INSERT   | 1.5x       | WAL write + data page write          |
| UPDATE   | 1.5x       | Read + write                         |
| DELETE   | 1.2x       | Mark + WAL                           |
| Other    | 1.0x       | DDL, EXPLAIN, BEGIN, etc.            |

**HTTP payload size tiers.** For HTTP spans, the multiplier depends on `response_size_bytes` (extracted from OTel `http.response.body.size` or legacy `http.response_content_length`).

| Payload size | Multiplier | Threshold        |
|--------------|------------|------------------|
| Small        | 0.8x       | < 10 KB          |
| Medium       | 1.2x       | 10 KB to 1 MB    |
| Large        | 2.0x       | > 1 MB           |
| Unknown      | 1.0x       | attribute absent |

**Sources.** The relative ratios are derived from academic DBMS energy benchmarks (Xu et al. "An Analysis of Power Consumption in a DBMS", VLDB 2010; Tsirogiannis et al. "Analyzing the Energy Efficiency of a Database Server", SIGMOD 2010) and the Cloud Carbon Footprint methodology. The absolute values are order-of-magnitude estimates. The relative ordering (SELECT < DELETE < INSERT/UPDATE) is more robust across hardware generations.

**Where it plugs in.** In `compute_carbon_report`'s span loop, the proxy fallback path applies the coefficient:

```rust
let proxy_energy_kwh = if ctx.per_operation_coefficients {
    ENERGY_PER_IO_OP_KWH * energy_coefficient(&span.event)
} else {
    ENERGY_PER_IO_OP_KWH
};
```

When measured energy is available (Scaphandre or cloud SPECpower), the coefficient is NOT applied. Measured data is always more accurate than heuristic multipliers.

**Hot path detail.** The `energy_coefficient()` function is `#[inline]` and avoids allocation: it uses `split_ascii_whitespace().next()` (lazy, stops at the first space) for verb extraction and `eq_ignore_ascii_case` for matching instead of lowercasing. The most common verb (SELECT) matches on the first comparison.

**Config toggle.** `[green] per_operation_coefficients = true` (default). Set to `false` to use the flat constant. The model tag stays `io_proxy_v1` or `io_proxy_v2` regardless of this toggle. The per-operation coefficients are a refinement of the proxy model, not a new model class.

## Network transport energy

For cross-region HTTP calls, the energy cost of moving bytes over the internet backbone can be significant. perf-sentinel offers an optional network transport energy term.

**The formula.**

```
energy_transport_kwh = bytes_transferred * ENERGY_PER_BYTE_KWH
transport_co2        = energy_transport_kwh * source_region_intensity * source_pue
```

The default coefficient is `4e-11 kWh/byte` (0.04 kWh/GB), the midpoint of the 0.03-0.06 kWh/GB range from recent studies (Mytton, Lunden & Malmodin, J. Industrial Ecology, 2024; Sustainable Web Design, 2024). The previous Shift Project 2019 value (0.07 kWh/GB) was on the high end of estimates. Mytton et al. (2024) demonstrate that the kWh/GB model is a simplification: network equipment has significant fixed baseload power, so energy does not scale linearly with data volume. The coefficient is configurable for users with more precise data.

The carbon intensity and PUE of the **source** region (where the data originates) are used, since the network infrastructure serving the request is co-located with the source.

**Cross-region detection.** Transport energy is only computed when caller and callee are in different regions. The mechanism:

1. **Caller region**: resolved via the standard chain (`span.cloud_region` > `service_regions[service]` > `default_region`).
2. **Callee region**: the hostname is extracted from the HTTP target URL (e.g., `order-api` from `http://order-api:8080/api/orders`), then looked up in `ctx.service_regions`. If the hostname is not mapped, perf-sentinel conservatively assumes same-region (no transport term).
3. If both regions resolve and differ (case-insensitive comparison), the transport energy is computed and accumulated.

**What triggers it.** Three conditions must all be true for a span to contribute transport energy:

- `include_network_transport = true` in the config
- The span is an HTTP outbound call (`event_type == HttpOut`)
- The span has a `response_size_bytes` value (from OTel `http.response.body.size`)

**Report output.** Transport CO2 appears as `transport_gco2` in both `CarbonReport` and `GreenSummary`. It is included in the SCI total: `total_mid = operational + embodied + transport`. The field is omitted from JSON when zero or when the feature is disabled.

**Config.** `[green] include_network_transport = false` (default, opt-in). The coefficient is configurable via `[green] network_energy_per_byte_kwh`. The feature is disabled by default because the transport term is often negligible compared to compute energy and adds model complexity.

**Hot path optimizations.** The transport path runs inside the per-span scoring loop. Two micro-optimizations avoid allocations in the common case:
- The hostname extracted from the URL is compared against `service_regions` with a probe-before-allocate pattern: `to_ascii_lowercase()` is only called when the hostname contains uppercase bytes (rare for Kubernetes/Docker service names).
- The caller region reuses `region_ref` already resolved earlier in the same loop iteration instead of calling `resolve_region` again.

**Top-offender `co2_grams` scalar.** The per-offender `co2_grams` uses the flat `ENERGY_PER_IO_OP_KWH` constant, not the per-operation coefficients. When `per_operation_coefficients` is active (the default), `co2_grams` is set to `None` to avoid an inconsistency with the per-region breakdown. The top-offender ranking (by IIS) is unaffected since IIS counts operations, not CO2.

**Limitations.** See `docs/LIMITATIONS.md` "Network transport energy" for the full discussion: wide estimate range, no CDN effects, no compression modeling, config-based region detection only, no last-mile modeling.

## Energy state cache coherency

Both the Scaphandre scraper and the cloud SPECpower scraper publish per-service `energy_per_op_kwh` readings to the scoring path on every tick. The two states share an `ArcSwap`-backed storage in `crates/sentinel-core/src/score/energy_state.rs`. The two public types (`ScaphandreState` and `CloudEnergyState`) are thin newtype wrappers that delegate to `AgedEnergyMap` and keep their nominal identity for type-safe plumbing through the daemon.

The design is deliberately read-heavy and write-rare:

- **Writes**: once per scrape interval (default 5s for Scaphandre, 15s for cloud energy) by a single task.
- **Reads**: once per `process_traces` tick (typically multiple per second under real OTLP load).
- **Consistency**: readers get the `Arc` that was current when they called `load_full`, writers do not block anyone.

`ArcSwap` was picked over `RwLock<HashMap>` because the `process_traces` reader path is on the hot loop, and the swap pointer-exchange is wait-free vs an `RwLock` that briefly blocks on `read()` when a writer holds the lock.

## Confidence field on findings (planned perf-lint interop)

A `confidence` field is stamped on every `Finding` in the JSON and SARIF report, indicating the source context of the detection. The value is set by the pipeline caller (`pipeline::analyze_with_traces` for batch mode → always `CiBatch`; `daemon::process_traces` for streaming mode → derived from `config.daemon_environment`). Detectors themselves never reason about confidence. They emit `Confidence::default()` and the caller overrides it.

Values:

| Confidence          | Source                                                   | SARIF rank |
|---------------------|----------------------------------------------------------|------------|
| `CiBatch`           | `analyze` batch mode, always                             | 30         |
| `DaemonStaging`     | `watch` daemon with `[daemon] environment="staging"`     | 60         |
| `DaemonProduction`  | `watch` daemon with `[daemon] environment="production"`  | 90         |

The field surfaces in:

- **JSON report**: every finding object includes `"confidence": "ci_batch"` / `"daemon_staging"` / `"daemon_production"`.
- **SARIF v2.1.0**: per-result `properties.confidence` bag entry AND a standard SARIF `rank` value (0-100).
- **CLI terminal output**: NOT displayed (the terminal stays clean for interactive use).

The planned consumer is perf-lint, a companion IDE integration (not yet published), which will import runtime findings from perf-sentinel's JSON output and apply a severity multiplier based on the confidence. Any custom tooling consuming the same JSON or SARIF output can use the field the same way. See `docs/INTEGRATION.md` "Finding confidence field" for the integration example.
