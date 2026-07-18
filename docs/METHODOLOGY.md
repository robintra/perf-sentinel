# Methodology

This document explains how perf-sentinel turns OpenTelemetry traces into the `efficiency_score`, `energy_kwh`, and `carbon_kgco2eq` fields surfaced in a periodic disclosure report. It is condensed from the per-stage design notes in `docs/design/` and `docs/ARCHITECTURE.md`. The audience is an auditor or data scientist who wants to verify the calculation chain end to end without reading the full source tree.

## Pipeline at a glance

```
events -> normalize -> correlate -> detect -> score -> report
```

Each stage is a pure function over data, with traits only at the I/O borders (`IngestSource`, `ReportSink`). A finding produced by `detect` is paired with a green-impact estimate produced by `score`, then aggregated by the periodic-disclosure aggregator over a calendar period.

## Background: Energy and SCI primer

If you have not implemented carbon scoring for software workloads before, this short primer is a prerequisite for the formulas in the rest of this document. It does not assume prior familiarity with the regulatory standards (CSRD, GHG Protocol, RGESN) nor with the energy-tooling stack (SCI v1.0, RAPL, Scaphandre, SPECpower, Boavizta, Electricity Maps API). Each is glossed in one line on first mention. Other perf-sentinel docs cross-reference this primer for green-scoring concepts, see [docs/CONFIGURATION.md](CONFIGURATION.md#green) and [docs/SCHEMA.md](SCHEMA.md#aggregate).

**The regulatory frameworks in scope.** perf-sentinel aligns its carbon model with three frameworks readers may have heard of, none of which is required to follow the rest of this document.

- **CSRD (Corporate Sustainability Reporting Directive)** is the mandatory EU 2024 sustainability-reporting regime. Large EU companies must publish audited emissions inventories along three scopes (direct, energy-purchased, value-chain). perf-sentinel can feed activity data into a CSRD pipeline but is not itself a CSRD reporting tool.
- **GHG Protocol (Greenhouse Gas Protocol)** is the international corporate-emissions accounting standard published by the WRI/WBCSD, the de-facto reference behind CSRD and most national regulations. Scope 2 covers purchased electricity, Scope 3 covers everything else upstream/downstream including software-purchased compute.
- **RGESN (Référentiel Général d'Écoconception de Services Numériques)** is the French eco-design framework for digital services published by ARCEP, Arcom and ADEME in 2024. It checks 78 criteria across architecture, content, hosting and lifecycle. perf-sentinel maps each detector onto the criteria it bears on, see the [RGESN 2024 crosswalk](#rgesn-2024-crosswalk) below.

**Why SCI v1.0.** Software Carbon Intensity is the standard developed by the Green Software Foundation and published as ISO/IEC 21031:2024 (ISO/IEC JTC 1, March 2024); the GSF-published artifact is the SCI Specification, current revision v1.1. It defines a per-functional-unit carbon score for software, `SCI = (E * I) + M`, expressed in gCO2eq per request (or per any functional unit you choose). The three terms map to three different physical phenomena and each is measured by a different toolchain. perf-sentinel uses SCI v1.0 because (a) it is the most widely-adopted methodology for comparing software-driven emissions across organisations, (b) it cleanly separates marginal/avoidable optimisation from total inventory accounting, (c) it is referenced by RGESN and aligns with GHG Protocol Scope 2/3 boundaries.

**The three SCI terms.**

- **E (Energy)** is the per-operation electricity, in kWh. perf-sentinel substitutes one of four measurement sources at runtime: an I/O proxy (`io_proxy_v3`, around `1e-7` kWh per I/O op, directional only), Scaphandre RAPL readings, cloud-provider CPU% mapped against SPECpower tables, or operator-supplied calibration coefficients via `[green] calibration_file`. The selected source is surfaced in `methodology.calibration.energy_source_models` so an auditor can verify which path produced E.
- **I (Grid intensity)** is the carbon emitted per kWh by the local electrical grid, in gCO2eq/kWh. perf-sentinel ships a static table (covering all major cloud regions and key national grids) and accepts a live override via the Electricity Maps API when `[green.electricity_maps]` is configured. Nationally-gridded rows are regenerated semiannually from Ember yearly data (generation-based) through a reviewed PR opened by the `refresh-datasets` workflow; subnational rows (North America, Brazil BR-CS) are hand-maintained from consumption-based zonal sources. The source is surfaced in `methodology.calibration.carbon_intensity_source` as one of `static_tables`, `electricity_maps`, or `mixed`.
- **M (Embodied carbon)** is the manufacturing emissions of the underlying silicon (CPU, RAM, networking, datacentre construction), amortised per request. perf-sentinel uses a default coefficient derived from Boavizta plus the HotCarbon 2024 paper, overridable via `[green] embodied_carbon_per_request_gco2`. M is region-independent and is added after `E * I`.

**Who reads which value.** A *sustainability auditor* preparing a CSRD scope-2 submission cares about `total_carbon_kgco2eq` and the `methodology.*` block proving the source of each term. An *SRE optimising the system* cares about `estimated_optimization_potential_kgco2eq`, which is the avoidable operational term (`avoidable_io_ops * ENERGY_PER_IO_OP_KWH * I`) and excludes M because you cannot un-manufacture silicon by fixing an N+1 query. The `efficiency_score` (0-100) is the operator-friendly summary derived from `io_waste_ratio` only, not from absolute emissions.

**Known limitation: 2x uncertainty bracket.** The carbon estimate ships with an explicit `2x` multiplicative bracket. This is a deliberate signal that the directional model (especially the I/O proxy and the static grid tables) is unsuitable for regulatory-grade emissions reporting. Tightening the bracket requires Scaphandre RAPL or cloud SPECpower for the E term and live Electricity Maps for the I term. The full uncertainty discussion lives in [docs/LIMITATIONS.md](LIMITATIONS.md).

**Related terms you will see in the sections below.** One-liners only, full definitions in the linked references.

- **RAPL (Running Average Power Limit)** is an Intel CPU feature that exposes a hardware energy counter readable via `/sys/class/powercap/intel-rapl/`. It gives per-package electricity consumption at millisecond granularity, with no instrumentation required in the application. AMD CPUs expose a similar interface under a different MSR. RAPL is what Scaphandre reads.
- **Scaphandre** is an open-source energy profiler that polls RAPL counters and exposes per-process power readings as a Prometheus endpoint. perf-sentinel scrapes Scaphandre and attributes the readings back to OTel-instrumented services via PID matching. [Project](https://github.com/hubblo-org/scaphandre).
- **SPECpower (`SPECpower_ssj2008`)** is a benchmark suite that maps CPU utilisation percentage to electricity draw for a published server SKU. The Cloud Carbon Footprint methodology uses SPECpower curves as a proxy when direct measurement is unavailable. perf-sentinel ships an embedded SPECpower table for the major cloud SKUs. [Benchmark](https://www.spec.org/power_ssj2008/).
- **CCF (Cloud Carbon Footprint)** is the open-source methodology Etsy published in 2020 that combines SPECpower tables, cloud-region grid intensities, and embodied amortisation. perf-sentinel's cloud-energy path is CCF-compatible, the same inputs and coefficients. [Project](https://www.cloudcarbonfootprint.org/).
- **Ember** is the energy think tank publishing open yearly electricity data (CC-BY-4.0), including the per-country CO2 intensity of generation. The nationally-gridded rows of the embedded static table are regenerated from it. [Data](https://ember-energy.org/data/).
- **Boavizta** is the French association that publishes open methodologies and reference data for digital-equipment lifecycle assessment, in particular the embodied-carbon coefficients for CPUs and servers. The default M term in perf-sentinel is derived from Boavizta plus the HotCarbon 2024 paper. [Project](https://boavizta.org/).
- **Electricity Maps API** is the commercial service (with a free API tier) that publishes hourly per-zone grid intensity in gCO2eq/kWh for 250+ zones worldwide. perf-sentinel calls it on-demand when `[green.electricity_maps]` is configured. Each request returns either a `direct` factor (operational generation only) or a `lifecycle` factor (operational plus manufacturing of generation assets). perf-sentinel records which one was used. [API docs](https://api-portal.electricitymaps.com/).
- **gCO2eq / kgCO2eq** is "grams (or kilograms) of CO2 equivalent". Equivalent because greenhouse gases other than CO2 (methane, nitrous oxide, ...) are weighted by their global-warming potential to a CO2 baseline. Standard unit across CSRD, GHG Protocol, SCI.
- **Marginal vs average emissions.** Average emissions is the grid-wide mean intensity over a window (what static tables and most Electricity Maps responses give). Marginal emissions is the intensity of the next kWh consumed (often a fossil-fired peaker), which matters for *demand-shifting* decisions but not for inventory reporting. perf-sentinel reports the average: SCI v1.1 (2024) permits short-run marginal, long-run marginal, or average grid intensity (the SCI v1.0 / ISO text required marginal rates). Marginal-mode scoring is a future enhancement.

## Academic grounding

The methodological choice to surface a directional score (`efficiency_score` on `io_waste_ratio`) and rank endpoints by relative impact, rather than report an absolute wattage figure, is grounded in an independent literature on software-energy measurement.

- **Hardware energy counters are accurate for their scope.** Khan, Hirki, Niemi, Nurminen and Ou ([*RAPL in Action: Experiences in Using RAPL for Power Measurements*, ACM TOMPECS 3(2):1-26, 2018](https://doi.org/10.1145/3177754)) characterise RAPL as a reliable energy source for the CPU and DRAM packages it covers, with the well-known caveat that it does not include peripherals, storage or PSU losses.
- **Software meters track the hardware signal.** Jay, Ostapenco, Lefèvre, Trystram, Orgerie and Fichel ([*An experimental comparison of software-based power meters: focus on CPU and GPU*, IEEE/ACM CCGrid 2023](https://doi.org/10.1109/CCGrid57682.2023.00020)) report strong correlation between software meters (Scaphandre among them) and an external wattmeter, while showing that the residual hardware-vs-software gap is significant and not constant across workloads. Software meters are good signal carriers, not substitutes for the absolute reading.
- **Relative beats absolute, and the main determinants are query patterns.** Ruch (*Towards Greener Software: Measuring Performance and Energy Efficiency of Enterprise Applications*, MSE Project Thesis, OST Eastern Switzerland University of Applied Sciences, supervisor Prof. Dr. Olaf Zimmermann, 2025) shows that absolute energy figures are not comparable across operating systems, applications and instruction sets, whereas the *relative distribution* of consumption is comparable across OS, applications and operation sets. The same work identifies database access patterns (number of queries, volume of records read, access technology) as the dominant energy determinants in enterprise applications.

perf-sentinel is positioned inside that tradition. The pipeline ranks endpoints by relative IIS, compares runs by `io_waste_ratio` deltas, and detects the database-access and inter-service patterns the literature identifies as primary energy determinants (N+1 SQL, N+1 HTTP, redundant SQL, redundant HTTP, fetch-all, fanout, chatty services, serialized calls). It does not claim wattmeter-grade absolute accuracy. The `2x` multiplicative bracket on the carbon estimate, the explicit positioning as a directional waste counter, and the full scope-and-precision discussion live in [docs/LIMITATIONS.md](LIMITATIONS.md).

## I/O Intensity Score (IIS)

The base proxy for energy is the I/O operation count per `(service, endpoint)` pair. perf-sentinel counts SQL and outbound HTTP spans as I/O operations.

- `total_io_ops`: count of I/O spans across all traces in the analyzed window.
- `avoidable_io_ops`: count of I/O spans attributed to avoidable anti-patterns. The four avoidable patterns are N+1 SQL, N+1 HTTP, redundant SQL, redundant HTTP, all four enumerated by `FindingType::is_avoidable_io()` and listed in `core_patterns_required` of every official disclosure.
- `io_waste_ratio = avoidable_io_ops / total_io_ops`, in `[0, 1]`.
- `total_sql_io_ops` and `avoidable_sql_io_ops`: the SQL share of the two counters above, same dedup semantics restricted to the SQL finding types. They let an operator apply the SQL-only waste ratio to an externally measured database energy reading (for example Alumet on the database cgroup): `db_waste = measured_db_energy * avoidable_sql_io_ops / total_sql_io_ops`. Both are `0` in reports produced by versions that predate the fields.
- `database_waste`: the daemon computes that same formula directly when `[green.alumet.database]` is declared, with the measured window energy of the declared database cgroup. The optional declared region converts the waste to gCO2 with the same intensity and PUE tables as services. The figure is a CPU-only lower bound with a count-based ratio, excluded from `energy_kwh`, `co2` and the public disclosure, see the "Alumet precision bounds" section of [docs/LIMITATIONS.md](LIMITATIONS.md).

## Energy per operation

Operational energy is approximated as a single-coefficient proxy:

```
energy_kwh = total_io_ops * ENERGY_PER_IO_OP_KWH
```

`ENERGY_PER_IO_OP_KWH = 1e-7 kWh` is documented in `score/carbon.rs` and tagged as model `io_proxy_v3`. The coefficient is a directional estimate, not a measurement.

When the operator wires the optional Scaphandre RAPL scraper or a cloud-energy SPECpower scraper, perf-sentinel substitutes a measured per-service energy and switches the model tag to `scaphandre_rapl` or `cloud_specpower`. The methodology section of a disclosure surfaces `scaphandre_used` and `specpower_table_version` so consumers know which path produced the numbers.

## Operational CO2

The Software Carbon Intensity (SCI) operational term is `O = E * I`, where `E` is per-window energy in kWh and `I` is the grid intensity in gCO2eq/kWh for the workload's region.

perf-sentinel ships with a static grid-intensity table and accepts a real-time override via the Electricity Maps API when `[green.electricity_maps]` is configured. The nationally-gridded rows are refreshed semiannually from Ember yearly data (generation-based, national granularity, latest year per country) through a reviewed PR. Subnational rows (North America, Brazil BR-CS) are hand-maintained from consumption-based zonal sources, and hourly profiles are renormalized whenever an annual value moves more than 5 percent. Generation-based and consumption-based accounting differ most on low-carbon, import-connected grids (the Nordic rows moved several-fold at the source switch); both stay well inside the documented 2x uncertainty bracket for high-carbon grids, and the bracket is the operative caveat either way. The `methodology.calibration.carbon_intensity_source` field of a disclosure is one of `electricity_maps`, `static_tables`, or `mixed` so an auditor can verify which path produced the operational CO2.

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

## RGESN 2024 crosswalk

The [RGESN 2024](https://www.arcep.fr/uploads/tx_gspublication/referentiel_general_ecoconception_des_services_numeriques_version_2024.pdf) (ARCEP, Arcom, ADEME) defines 78 eco-design criteria across nine families, numbered `family.criterion`. The table below maps each perf-sentinel detector to the criteria whose intent it bears on.

This is an **interpretive crosswalk, not a compliance certification**. The RGESN criterion titles do not name "N+1 query" or "slow query". These are the criteria a detection helps satisfy, surfaced so an auditor can connect a finding to the referential. The machine-readable form is `FindingType::rgesn_criteria()` in code and the per-pattern `rgesn_criteria` field on the disclosure report's anti-pattern details.

| Detector | RGESN criteria | Criterion intent |
|---|---|---|
| `n_plus_one_sql`, `n_plus_one_http` | 7.1, 6.1 | Server-side cache for most-used data, request budget per screen |
| `redundant_sql`, `redundant_http` | 7.1, 6.5 | Server-side cache, avoid loading unused resources |
| `chatty_service` | 4.9, 4.10, 6.1 | Limit and avoid unnecessary server requests, request budget per screen |
| `excessive_fanout`, `pool_saturation` | 3.2 | Architecture that scales resources to actual demand |
| `serialized_calls` | 8.10 | Minimize the impact of asynchronous compute and data transfers |
| `slow_sql`, `slow_http` | (none) | RGESN has no single-operation-latency criterion. Family 9 "Algorithmie" targets machine-learning workloads, not query latency. |

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

> **See also.** The [Sigstore primer](SUPPLY-CHAIN.md#background-sigstore-primer) in the supply-chain doc defines Cosign, Fulcio, Rekor, in-toto, OIDC and SLSA used throughout this section.

Two optional primitives layered on top of the content hash anchor a published disclosure in public infrastructure.

- **Sigstore signature** (`integrity.signature`). When the operator signs the disclosure's in-toto v1 attestation via `cosign attest`, the report carries metadata (`bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, `signed_at`) that lets a consumer recover the bundle and verify it through Rekor public. `verify-hash` rejects bundles without a Rekor inclusion proof, so transparency is a property of the format, not optional.
- **SLSA build provenance** (`integrity.binary_attestation`). Official perf-sentinel release binaries carry a SLSA Build L3 attestation produced by the GitHub Actions release workflow (`actions/attest-build-provenance` from v0.7.1 onward, `slsa-framework/slsa-github-generator` SLSA L2 on the v0.7.0 release). The report records the locator metadata so a consumer can verify the attestation against the binary referenced by `integrity.binary_verification_url`, via `gh attestation verify <binary> --repo robintra/perf-sentinel` for 0.7.1+ or `slsa-verifier verify-artifact` against the legacy `multiple.intoto.jsonl` release asset on 0.7.0.

Combined, the two primitives form the chain `source -> SLSA -> binary -> report -> Sigstore signature`. `verify-hash` chains content hash recompute, cosign signature, and the SLSA verification hint in a single command. The methodology, failure modes, and Rekor public privacy considerations live in `docs/design/10-SIGSTORE-ATTESTATION.md`.
