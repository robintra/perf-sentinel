# Methodology

This document explains how perf-sentinel turns OpenTelemetry traces into the `efficiency_score`, `energy_kwh`, and `carbon_kgco2eq` fields surfaced in a periodic disclosure report. It is condensed from the per-stage design notes in `docs/design/` and `docs/ARCHITECTURE.md`. The audience is an auditor or data scientist who wants to verify the calculation chain end to end without reading the full source tree.

## Pipeline at a glance

```
events -> normalize -> correlate -> detect -> score -> report
```

Each stage is a pure function over data, with traits only at the I/O borders (`IngestSource`, `ReportSink`). A finding produced by `detect` is paired with a green-impact estimate produced by `score`, then aggregated by the periodic-disclosure aggregator over a calendar period.

## I/O Intensity Score (IIS)

The base proxy for energy is the I/O operation count per `(service, endpoint)` pair. perf-sentinel counts SQL and outbound HTTP spans as I/O operations.

- `total_io_ops`: count of I/O spans across all traces in the analyzed window.
- `avoidable_io_ops`: count of I/O spans attributed to avoidable anti-patterns. The four avoidable patterns are N+1 SQL, N+1 HTTP, redundant SQL, redundant HTTP, all four enumerated by `FindingType::is_avoidable_io()` and listed in `core_patterns_required` of every official disclosure.
- `io_waste_ratio = avoidable_io_ops / total_io_ops`, in `[0, 1]`.

## Energy per operation

Operational energy is approximated as a single-coefficient proxy:

```
energy_kwh = total_io_ops * ENERGY_PER_IO_OP_KWH
```

`ENERGY_PER_IO_OP_KWH = 1e-7 kWh` is documented in `score/carbon.rs` and tagged as model `io_proxy_v3`. The coefficient is a directional estimate, not a measurement.

When the operator wires the optional Scaphandre RAPL scraper or a cloud-energy SPECpower scraper, perf-sentinel substitutes a measured per-service energy and switches the model tag to `scaphandre_rapl` or `cloud_specpower`. The methodology section of a disclosure surfaces `scaphandre_used` and `specpower_table_version` so consumers know which path produced the numbers.

## Operational CO2

The Software Carbon Intensity (SCI) operational term is `O = E * I`, where `E` is per-window energy in kWh and `I` is the grid intensity in gCO2eq/kWh for the workload's region.

perf-sentinel ships with a static grid-intensity table refreshed annually and accepts a real-time override via the Electricity Maps API when `[green.electricity_maps]` is configured. The `methodology.calibration.carbon_intensity_source` field of a disclosure is one of `electricity_maps`, `static_tables`, or `mixed` so an auditor can verify which path produced the operational CO2.

## Embodied CO2

The SCI `M` term covers manufactured-silicon emissions amortised per request. perf-sentinel uses a fixed default coefficient documented in `config.rs::DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2`, overridable via `[green] embodied_carbon_per_request_gco2`. Embodied CO2 is region-independent and is added to operational CO2 before the per-window total is summed across the disclosure period.

## Aggregation over a period

`perf-sentinel disclose` reads archived per-window `Report` envelopes (`{ts, report}`) and folds them in three stages.

1. Each envelope is filtered to fall inside the requested calendar period.
2. Global counters add up `total_io_ops`, `avoidable_io_ops`, `total.mid` (gCO2), `avoidable.mid` (gCO2). gCO2 is divided by 1000 to obtain `kgCO2eq`.
3. Per-service attribution distributes the global totals proportionally to the per-service I/O share read from `Report.per_endpoint_io_ops`. A window with zero per-service offenders is bucketed under `_unattributed` unless `--strict-attribution` was passed.

`efficiency_score = clamp(100 - 100 * io_waste_ratio, 0, 100)`. Per-service efficiency uses the same formula on the service's own avoidable / total ratio.

## Known limitations in schema v1.0

- **Energy and per-service carbon are runtime-calibrated when the source archive carries them.** Each window's `GreenSummary` now ships `energy_kwh`, `energy_model`, `per_service_energy_kwh`, `per_service_carbon_kgco2eq`, and `per_service_region`. The aggregator sums these directly. Archives written before this feature shipped do not carry the fields, so the aggregator falls back to a proxy energy (`total_io_ops × ENERGY_PER_IO_OP_KWH`) and a proportional I/O share for carbon, and emits a single `tracing::warn!` per such archive. The set of observed `energy_model` tags is surfaced under `methodology.calibration_inputs.energy_source_models`.
- **Optimization potential excludes embodied carbon.** `estimated_optimization_potential_kgco2eq` is the avoidable operational term only (you cannot un-manufacture silicon by fixing N+1 queries). The aggregate `total_carbon_kgco2eq` includes both operational and embodied terms. The disclaimer in `notes.disclaimers` calls this out explicitly.
- **Per-service carbon excludes embodied.** The embodied term (SCI `M`) lives only in the aggregate. `sum(per_service_carbon_kgco2eq) × 1000` approximates `co2.operational_gco2`, not `co2.total.mid`.
- **`_unattributed` bucket.** Windows whose `Report.per_endpoint_io_ops` is empty (and that lack runtime per-service maps) land in the `_unattributed` service. `disclose --strict-attribution` refuses such windows. Findings from those windows are also bucketed under `_unattributed` so a service is never published with `efficiency_score = 100` and non-zero anti-patterns.

## Uncertainty bracket

Every disclosure ships with a `2x` multiplicative bracket on the carbon estimate. This is a deliberate signal that the output is directional and unsuitable for regulatory-grade emissions reporting (CSRD, GHG Protocol Scope 3). The `notes.disclaimers` block of a disclosure reiterates this in operator-readable English, including the v1.0-specific limitations above.

## Verifying a disclosure

A disclosure carries:

- `integrity.content_hash`: SHA-256 over the canonical JSON form (sorted object keys, compact serialisation, UTF-8) with `content_hash` blanked to an empty string. A consumer recomputes by setting their copy's `content_hash` to `""` and hashing.
- `integrity.binary_hash`: SHA-256 of the perf-sentinel binary that produced the file, taken via `std::env::current_exe()`. Pair with `binary_verification_url` to assert the binary matches a published release.

The hash chain in `integrity.trace_integrity_chain` and a Sigstore signature in `integrity.signature` are reserved for a future revision and always `null` in schema v1.0.
