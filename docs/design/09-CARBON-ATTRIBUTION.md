# Per-service carbon attribution

Design notes for the runtime-calibrated per-service energy and carbon attribution surfaced in `GreenSummary` and consumed by the periodic disclosure aggregator. Pairs with `docs/METHODOLOGY.md` (operator-facing) and `docs/design/08-PERIODIC-DISCLOSURE.md` (aggregator + wire schema).

## Why

The first disclosure release recomputed `aggregate.total_energy_kwh` via a proxy at aggregate time, even when the underlying daemon had measured energy through Scaphandre or cloud SPECpower. It also distributed window-level CO2 to services proportionally to per-service I/O ops, ignoring the fact that two services in different regions emit at very different grid intensities.

The fix is to compute and serialise per-service energy + carbon at scoring time, so the aggregator can sum directly. Per-service values are runtime-calibrated end to end: the daemon sees the real region for each service and the real energy backend tag.

## Algorithm

Scoring runs in `score::compute_carbon_report`. The function already loops once over all spans in the batch and accumulates per-region carbon into `RegionAccumulator`. Sprint 2 adds a parallel `BTreeMap<String, ServiceCarbonAccumulator>` that follows the same single-pass shape.

For each span, after computing the per-span energy, region, intensity, and PUE, the inner loop now also runs:

```rust
let svc = state
    .per_service
    .entry(span.event.service.to_string())
    .or_insert_with(|| ServiceCarbonAccumulator {
        energy_kwh: 0.0,
        operational_gco2: 0.0,
        region: region_ctx.region_ref.to_string(),
    });
svc.energy_kwh += energy_kwh;
svc.operational_gco2 += op_co2;
```

Once the loop completes, `score_green` produces the GreenSummary maps:

- `per_service_energy_kwh[svc] = acc.energy_kwh`
- `per_service_carbon_kgco2eq[svc] = acc.operational_gco2 / 1000.0`
- `per_service_region[svc] = acc.region` (or `"unknown"` sentinel if empty)
- `energy_kwh = sum(per_service_energy_kwh.values())`
- `energy_model = select_co2_model_tag(window_flags)` when energy > 0, else empty string

The per-service map is keyed by service name (lowercased upstream by `CarbonContext.service_regions`). The `region` field on the accumulator is also lowercased before storage, matching the keys in `per_region` so the two maps collate. Empty energy yields an empty `energy_model` string, which marks the window as pre-sprint-2 for the aggregator's fallback path.

## Region attribution

The region recorded for a service is the region of the *first* span observed for that service in the window. Later spans for the same service keep this region even if they carry a different `cloud_region` attribute. Two consequences:

- A service deployed in two regions within the same scoring window is attributed entirely to its first observed region. The per-region row in `GreenSummary.regions` still reflects the split, so the global figures stay correct.
- Long-running services with stable `service_regions` configuration are unaffected: every span resolves to the same region.

This trade-off keeps the per-service map simple. A more granular `BTreeMap<(String, String), ServiceCarbonAccumulator>` keyed by `(service, region)` would surface multi-region splits but enlarge the wire payload and force consumers to fold rows themselves. v1.0 prefers the simpler shape.

## Model tag precedence

The per-window `energy_model` reuses the existing `select_co2_model_tag` from `score::region_breakdown`, which already implements the canonical precedence:

```
electricity_maps_api > scaphandre_rapl > cloud_specpower > io_proxy_v3 > io_proxy_v2 > io_proxy_v1
```

with the optional `+cal` suffix when calibration data is active. The tag reflects the highest-fidelity model present in the window. No per-service breakdown of model tags is exposed: a transparent global tag is more useful than a per-service map that consumers would have to fold anyway.

## Embodied carbon stays at the global level

The SCI `M` term lives only in `co2.total` and `aggregate.total_carbon_kgco2eq`. Per-service maps carry the operational term only. Reasons:

- Per-request embodied amortisation is already an arbitrary spread. Splitting it per service would surface a precision that does not exist in the underlying data.
- Embodied is not actionable through software optimisation. Removing N+1 patterns has no effect on `M`.
- Consumers (auditors, public dashboards) who need the per-service operational figure benefit from a cleaner number that maps directly to actionable optimisations.

The invariant `sum(per_service_carbon_kgco2eq) × 1000 ≈ co2.operational_gco2` (tolerance 1e-6) is tested.

## Aggregator branching

`report::periodic::aggregator::Builder::process_window` checks two predicates:

1. `report.green_summary.per_service_carbon_kgco2eq.is_empty() && report.green_summary.per_service_energy_kwh.is_empty()` — runtime maps absent.
2. `report.green_summary.energy_kwh > 0.0` — runtime energy total present.

When both runtime maps are non-empty, the aggregator sums the per-service values directly. When they are empty, it falls back to the proxy path inherited from the first release (proportional I/O share for carbon, `total_io_ops × ENERGY_PER_IO_OP_KWH` for energy). The two paths can coexist within the same archive directory: each window applies its own strategy.

A single `tracing::warn!` per archive file flags fallback usage so operators can spot stale archives. The counters `runtime_windows` and `fallback_windows` on `AggregateInputs` carry the split for downstream diagnostics.

## Hardening at the archive boundary

Archive lines are operator-controlled state on disk. The aggregator treats every f64 field read out of an archive as untrusted:

- `energy_kwh`, `per_service_energy_kwh.values()` and `per_service_carbon_kgco2eq.values()` go through `sanitize_f64` which clamps `NaN`, `+/-Inf` and negative numbers to `0.0`. Without this guard a single poisoned line would propagate `NaN` to every downstream sum.
- The `per_service` map is capped at `MAX_SERVICES = 4096` entries. Once the cap is reached, additional distinct services from the archive are silently dropped on the floor. Findings already routed to a known bucket continue to accumulate.
- `energy_source_models` is capped at `MAX_ENERGY_MODELS = 64` entries and each `energy_model` string is rejected when longer than 64 bytes. Tags differing only by the `+cal` suffix collapse into a single bare entry, so the set never carries both `scaphandre_rapl` and `scaphandre_rapl+cal`.

These caps mirror the runtime-side `MAX_REGIONS` cap in `score::carbon_compute`. They are silent (no error), the aggregator treats them as best-effort folding.

## Backward compatibility

All five new `GreenSummary` fields carry `#[serde(default)]`. An archive line written before sprint 2 deserialises with `energy_kwh = 0.0`, `energy_model = ""`, and empty maps. The aggregator detects this and falls back to the proxy.

No schema version bump. `perf-sentinel-report/v1.0` stays the wire identifier. Consumers that read only the documented sprint-1 set keep working; consumers that opt into the new fields gain runtime-calibrated values automatically.

## What we did not do

- Per-service energy model tags (`per_service_energy_model: BTreeMap<String, String>`). Possible but unused today; the window-level tag carries enough fidelity for the disclosure's audit trail.
- Multi-region per-service splits. The wire shape stays simple at the cost of approximate attribution for services that move regions mid-window.
- Embodied carbon attribution per service. Deliberately excluded.
- Schema version bump. The change is strictly additive.
