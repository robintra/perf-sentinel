# GreenOps scoring and carbon conversion

## I/O Intensity Score (IIS)

The central metric is the I/O Intensity Score: the number of I/O operations generated per user request for a given endpoint.

```
IIS(endpoint) = total_io_ops(endpoint) / invocation_count(endpoint)
```

An endpoint called across 3 traces with 18 total I/O operations has `IIS = 18 / 3 = 6.0`. This normalizes across different traffic volumes: a high-traffic endpoint with 1000 invocations and 6000 I/O ops has the same IIS (6.0) as a low-traffic one with 3 invocations and 18 ops.

The denominator uses `.max(1)` as a safety guard against division by zero, though this case cannot occur in practice (an endpoint that appears in `endpoint_stats` must have been seen in at least one trace).

## Scoring algorithm: five phases

### Phase 1: endpoint statistics

```rust
let mut seen_endpoints: HashSet<&str> = HashSet::new();
for trace in traces {
    seen_endpoints.clear();
    for span in &trace.spans {
        total_io_ops += 1;
        let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats { ... });
        stats.total_io_ops += 1;
        seen_endpoints.insert(key);
    }
    for &ep in &seen_endpoints {
        endpoint_stats.get_mut(ep).unwrap().invocation_count += 1;
    }
}
```

**HashSet reuse:** `seen_endpoints.clear()` reuses the same HashSet across trace iterations. Without this, each trace would allocate a new HashSet. For 10,000 traces, this saves 10,000 allocations.

**`EndpointStats<'a>` with borrowed `service`:** the `service` field borrows `&'a str` from the span events instead of cloning the String. The clone only happens later when building `TopOffender` structs for the output. This avoids one String clone per unique endpoint in the inner loop.

### Phase 2: dedup avoidable I/O

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

### Phase 3: compute IIS per endpoint

```rust
let iis_map: HashMap<&str, f64> = endpoint_stats.iter()
    .map(|(&ep, stats)| {
        let invocations = stats.invocation_count.max(1) as f64;
        (ep, stats.total_io_ops as f64 / invocations)
    })
    .collect();
```

The IIS map is computed once and reused for both finding enrichment (Phase 4) and top offender ranking (Phase 5).

### Phase 4: enrich findings

Each finding receives a `GreenImpact`:

```rust
GreenImpact {
    estimated_extra_io_ops: if slow { 0 } else { occurrences - 1 },
    io_intensity_score: iis,
}
```

### Phase 5: top offenders

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

### Energy constant

```rust
const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1; // 0.1 uWh per I/O op
```

This is a rough order-of-magnitude approximation, not a measured value. It accounts for a typical database query or HTTP round-trip on cloud infrastructure. The [Cloud Carbon Footprint project](https://www.cloudcarbonfootprint.org/docs/methodology/) uses a similar approach of estimating energy from resource usage rather than direct measurement.

The value must be disclosed as methodology per SCI requirements. It is documented in the code, in [LIMITATIONS.md](../LIMITATIONS.md) and here.

### Conversion formula

```
gCO2eq = io_ops × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE
```

Where:
- `carbon_intensity` = gCO2eq/kWh for the region's electricity grid
- `PUE` = Power Usage Effectiveness (datacenter overhead factor)

### Region lookup

The carbon intensity table is embedded as a static array and converted to a `HashMap` via `LazyLock`:

```rust
static REGION_MAP: LazyLock<HashMap<&'static str, (f64, Provider)>> =
    LazyLock::new(|| CARBON_TABLE.iter().map(...).collect());
```

**Why `LazyLock<HashMap>` instead of a linear scan?** The original implementation scanned all 41 entries on every call. With the HashMap, lookup is O(1). The initialization cost is paid once on first access.

**Case-insensitive lookup:** the public `lookup_region()` lowercases the input via `to_ascii_lowercase()` before lookup. All table keys are stored in lowercase. Internally, a private `lookup_region_lower()` skips the lowercasing for callers that have already normalized the region string (e.g., `score_green` pre-lowercases once and reuses the result across multiple calls to `io_ops_to_co2_grams`).

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
