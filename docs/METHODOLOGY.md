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
3. Per-service attribution uses the runtime-calibrated `per_service_*` maps when the source window carries them. Otherwise the global totals are distributed proportionally to the per-service I/O share read from `Report.per_endpoint_io_ops`. A window with zero per-service offenders is bucketed under `_unattributed` unless `--strict-attribution` was passed.

`efficiency_score = clamp(100 - 100 * io_waste_ratio, 0, 100)`. Per-service efficiency uses the same formula on the service's own avoidable / total ratio.

Quality signals (0.7.0+) summarise how much of the period was directly measured versus inferred from the proxy.

- `period_coverage = runtime_windows / total_windows`, in `[0, 1]`, with `runtime_windows_count` and `fallback_windows_count` carrying the absolute counts behind the ratio.
- `binary_versions` is the set of perf-sentinel binary versions observed across the period; a daemon upgrade mid-period makes this set carry more than one entry.
- `calibration_applied` on `methodology.calibration_inputs` flips to `true` when at least one window applied operator calibration coefficients to the proxy energy.
- `per_service_energy_models` and `per_service_measured_ratio` (in both `GreenSummary` per window and `Aggregate` over the period) surface the per-service fidelity view: which energy model fed each service and what fraction of its spans actually got measured.

The wire-format definitions for these fields live in the "Aggregate" and "Methodology" sections of `docs/SCHEMA.md`.

## Known limitations in schema v1.0

- **Energy and per-service carbon are runtime-calibrated when the source archive carries them.** Each window's `GreenSummary` now ships `energy_kwh`, `energy_model`, `per_service_energy_kwh`, `per_service_carbon_kgco2eq`, and `per_service_region`. The aggregator sums these directly. Archives written before this feature shipped do not carry the fields, so the aggregator falls back to a proxy energy (`total_io_ops × ENERGY_PER_IO_OP_KWH`) and a proportional I/O share for carbon, and emits a single `tracing::warn!` per such archive. The set of observed `energy_model` tags is surfaced under `methodology.calibration_inputs.energy_source_models`.
- **Optimization potential excludes embodied carbon.** `estimated_optimization_potential_kgco2eq` is the avoidable operational term only (you cannot un-manufacture silicon by fixing N+1 queries). The aggregate `total_carbon_kgco2eq` includes both operational and embodied terms. The disclaimer in `notes.disclaimers` calls this out explicitly.
- **Per-service carbon excludes embodied.** The embodied term (SCI `M`) lives only in the aggregate. `sum(per_service_carbon_kgco2eq) × 1000` approximates `co2.operational_gco2`, not `co2.total.mid`.
- **`_unattributed` bucket.** Windows whose `Report.per_endpoint_io_ops` is empty (and that lack runtime per-service maps) land in the `_unattributed` service. `disclose --strict-attribution` refuses such windows. Findings from those windows are also bucketed under `_unattributed` so a service is never published with `efficiency_score = 100` and non-zero anti-patterns.
- **Period coverage and the 75% gate (0.7.0+).** Every disclosure carries `aggregate.period_coverage`, the fraction of scoring windows that used runtime-calibrated energy versus the proxy fallback. An `intent = "official"` disclosure with coverage below 0.75 is rejected by the validator. An `intent = "internal"` disclosure below that threshold ships an explicit disclaimer in `notes.disclaimers`. The empirical rationale for 0.75 lives in `docs/design/08-PERIODIC-DISCLOSURE.md`.
- **Per-service measured ratio is span-uniform, window-mean (0.7.0+).** `per_service_measured_ratio` in `GreenSummary` is the fraction of a service's spans whose energy was resolved by Scaphandre or cloud SPECpower in that window. The period-level value in `Aggregate.per_service_measured_ratio` is the simple arithmetic mean of those per-window ratios, not span-weighted: a 10-span window and a 10000-span window contribute equally. A service whose `per_service_energy_model` shows `scaphandre_rapl` with `per_service_measured_ratio` of `0.05` had a single Scaphandre observation against 95% proxy fallback in the window: the tag indicates the best source observed, the ratio describes the fidelity.
- **Calibration applied is binary, period-wide (0.7.0+).** `methodology.calibration_inputs.calibration_applied` is `true` as soon as at least one window of the period had operator calibration active, even if 89 of 90 windows did not. The disclaimer text in `notes.disclaimers` reflects this exact wording so a reader cannot mistake the flag for "every window was calibrated".
- **Binary versions across the period (0.7.0+).** `aggregate.binary_versions` lists the perf-sentinel binary versions that produced the source archives. A period spanning multiple versions ships a disclaimer pointing the consumer to verify version compatibility before comparing this report against historical baselines. The set is capped at 256 entries; in the unlikely case a quarter spans more, overflow entries are silently dropped.

## Uncertainty bracket

Every disclosure ships with a `2x` multiplicative bracket on the carbon estimate. This is a deliberate signal that the output is directional and unsuitable for regulatory-grade emissions reporting (CSRD, GHG Protocol Scope 3). The `notes.disclaimers` block of a disclosure reiterates this in operator-readable English, including the v1.0-specific limitations above.

## Verifying a disclosure

A disclosure carries:

- `integrity.content_hash`: SHA-256 over the canonical JSON form (sorted object keys, compact serialisation, UTF-8) with `content_hash` blanked to an empty string. A consumer recomputes by setting their copy's `content_hash` to `""` and hashing.
- `integrity.binary_hash`: SHA-256 of the perf-sentinel binary that produced the file, taken via `std::env::current_exe()`. Pair with `binary_verification_url` to assert the binary matches a published release.

The hash chain in `integrity.trace_integrity_chain` is reserved for a future revision and is always `null` in schema v1.0.

## Cryptographic integrity (0.7.0+)

Two optional primitives layered on top of the content hash anchor a published disclosure in public infrastructure.

- **Sigstore signature** (`integrity.signature`). When the operator signs the disclosure's in-toto v1 attestation via `cosign attest`, the report carries metadata (`bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, `signed_at`) that lets a consumer recover the bundle and verify it through Rekor public. `verify-hash` rejects bundles without a Rekor inclusion proof, so transparency is a property of the format, not optional.
- **SLSA build provenance** (`integrity.binary_attestation`). Official perf-sentinel release binaries carry a SLSA Build L3 attestation produced by the GitHub Actions release workflow (`actions/attest-build-provenance` from v0.7.1 onward, `slsa-framework/slsa-github-generator` SLSA L2 on the v0.7.0 release). The report records the locator metadata so a consumer can verify the attestation against the binary referenced by `integrity.binary_verification_url`, via `gh attestation verify <binary> --owner robintra --repo perf-sentinel` for 0.7.1+ or `slsa-verifier verify-artifact` against the legacy `multiple.intoto.jsonl` release asset on 0.7.0.

Combined, the two primitives form the chain `source -> SLSA -> binary -> report -> Sigstore signature`. `verify-hash` chains content hash recompute, cosign signature, and the SLSA verification hint in a single command. The methodology, failure modes, and Rekor public privacy considerations live in `docs/design/10-SIGSTORE-ATTESTATION.md`.
