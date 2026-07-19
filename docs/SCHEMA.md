# Schema reference: perf-sentinel-report v1.4

This document describes the JSON shape of a periodic disclosure report in prose. The machine-readable JSON Schema lives at `docs/schemas/perf-sentinel-report-v1.json` (draft 2020-12). Two filled examples sit under `docs/schemas/examples/`.

v1.1 adds the `canonical_waste` and `operational_waste` tiers to `aggregate`. v1.2 adds `aggregate.temporal_coverage` (a measurement-continuity signal), `scope_manifest.coverage_basis` (a provenance marker), and the reserved `integrity.cross_period_log` hook. v1.3 adds `methodology.standard_crosswalk` (an interpretive ESRS E1 datapoint crosswalk) and per-pattern `applications[].anti_patterns[].rgesn_criteria` (RGESN 2024 criteria). v1.4 adds `aggregate.database_waste` (the database-side avoidable energy and carbon at both thresholds, with its provenance models, an informational lower bound kept out of every total). The schema accepts `perf-sentinel-report/v1.0` through `v1.4`, and every added field defaults when absent, so older readers and reports remain valid and the `content_hash` of an older report is unchanged when re-hashed on a newer binary.

## Top-level keys

| key               | type           | required | notes                                                                    |
|-------------------|----------------|----------|--------------------------------------------------------------------------|
| `schema_version`  | string (enum)  | yes      | `"perf-sentinel-report/v1.4"` (also accepts `"…/v1.3"`, `"…/v1.2"`, `"…/v1.1"`, `"…/v1.0"`) |
| `report_metadata` | object         | yes      | see [Report metadata](#report-metadata)                                  |
| `organisation`    | object         | yes      | see [Organisation](#organisation)                                        |
| `period`          | object         | yes      | see [Period](#period)                                                    |
| `scope_manifest`  | object         | yes      | see [Scope manifest](#scope-manifest)                                    |
| `methodology`     | object         | yes      | see [Methodology](#methodology)                                          |
| `aggregate`       | object         | yes      | see [Aggregate](#aggregate)                                              |
| `applications`    | array          | yes      | homogeneous: all G1 or all G2 entries, see [Applications](#applications) |
| `integrity`       | object         | yes      | see [Integrity](#integrity)                                              |
| `notes`           | object         | yes      | see [Notes](#notes)                                                      |

The schema does not set `additionalProperties: false`; new fields can be added in a SemVer-minor schema bump without breaking consumers that read only the documented set.

## Report metadata

A disclosure document carries three orthogonal axes that consumers should read before parsing any data: `intent` says *who the report is for* (a private draft, an external publication, or a third-party-audited publication), `confidentiality_level` says *how much per-service detail is exposed* (full G1 anti-pattern breakdown vs aggregated G2 counts), and `integrity_level` says *which cryptographic primitives back the document* (none, content hash only, Sigstore-signed, signed with SLSA build attestation). Together they let an auditor or a journalist filter the corpus before trusting any number inside.

`intent` is one of `internal | official | audited`. `audited` is reserved for a future release: the JSON schema accepts the value for forward compatibility, but the CLI refuses it today with exit code 2. `confidentiality_level` is one of `internal | public`. `integrity_level` is one of `none | hash-only | signed | signed-with-attestation | audited`. By default the CLI produces `hash-only`. `generated_at` is an RFC 3339 UTC timestamp. `generated_by` is one of `daemon | cli-batch | ci`. `perf_sentinel_version` is the SemVer string of the binary that wrote the file. `report_uuid` is a v4 UUID stamped per run.

## Organisation

`name` is required and non-empty. `country` is ISO 3166-1 alpha-2, upper case. `identifiers` is an open object with optional `siren`, `vat`, `lei`, `opencorporates_url`, `domain`. `sector` is an optional NACE rev2 code.

The publication domain (e.g. `transparency.example.fr`) is treated as an implicit identifier when `notes.reference_urls.project` is published from that host. The schema does not enforce this, by design.

## Period

`from_date` and `to_date` are ISO 8601 calendar dates (`YYYY-MM-DD`). `period_type` is one of `calendar-quarter | calendar-month | calendar-year | custom`. `days_covered` is `to_date - from_date + 1`. The official-intent validator enforces `days_covered >= 30`.

## Scope manifest

`total_applications_declared` is the size of the organisation's application portfolio. `applications_measured` is the count of services for which the disclosure carries data. Each entry in `applications_excluded` carries `service_name` and a non-empty `reason`. `environments_measured` lists the operator-defined environments observed (e.g. `["prod"]`). `total_requests_in_period` is an optional operator estimate, `requests_measured` is what perf-sentinel actually saw. `coverage_percentage` is `requests_measured / total_requests_in_period * 100` when the former is set.

`coverage_basis` (v1.2) makes the trust boundary explicit in-band. It lists which scope fields are `operator_declared` (unaudited assertions the binary cannot verify, the denominators `total_applications_declared` and `total_requests_in_period`, plus the exclusion lists) versus `machine_derived` (computed by the aggregator from the archives, `applications_measured`, `requests_measured`, `coverage_percentage`). A reader of `coverage_percentage` should treat its denominator as operator-asserted: an operator who sets `total_requests_in_period` low can present near-100% coverage of a self-defined universe. This is inherent to a self-disclosure model, the cryptographic integrity guarantees bind the published report, not the honesty of the declared portfolio size. See [docs/design/08-PERIODIC-DISCLOSURE.md](design/08-PERIODIC-DISCLOSURE.md).

## Methodology

`sci_specification` references the SCI revision (e.g. `"ISO/IEC 21031:2024"`). `perf_sentinel_version` mirrors the report metadata field for consumers that index only the methodology block. `enabled_patterns` and `disabled_patterns` each carry pattern names taken from the closed set defined by `FindingType::as_str()` (10 values). `core_patterns_required` is the closed list of patterns whose remediation directly cuts I/O and carbon: `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`. `conformance` is one of `core-required | extended | partial`; `core-required` is the minimum bar for an `intent = "official"` disclosure. `calibration_inputs.carbon_intensity_source` is one of `electricity_maps | static_tables | mixed`. `specpower_table_version` is the operator-declared version of the embedded SPECpower / CCF coefficient table, set in the org config TOML. `binary_specpower_vintage` (0.7.3+) is the vintage string the running binary embeds at build time, populated automatically by `perf-sentinel disclose`. Consumers may compare both strings to detect drift between operator disclosure and embedded data. `scaphandre_used` flags whether the runtime energy proxy came from Scaphandre RAPL. It is a historical, Scaphandre-specific field: it predates the other measured-energy backends and was never generalized, so it stays `false` for a period measured with Alumet, Kepler or Redfish. **`energy_source_models` is the general source of truth** for which energy sources produced a period's figures, it is derived automatically from the archived windows and carries every backend's tag (`alumet_rapl`, `scaphandre_rapl`, `kepler_ebpf`, `redfish_bmc`, `cloud_specpower`, `io_proxy_v*`). Consumers should read `energy_source_models` and treat `scaphandre_used` as a legacy hint. Generalizing the field would break a published, hashed and attested schema, so it is deferred to a future schema revision. `standard_crosswalk` (v1.3) is an interpretive map from this report's figures to ESRS E1 datapoints (`total_energy_kwh` to E1-5, the operational carbon term to E1-6 Scope 2 location-based, embodied carbon to E1-6 Scope 3). It carries its own `caveats` array. It is a mapping aid, not a certification, the location-based figure is not the market-based Scope 2 value ESRS also requires, and the 2x uncertainty bracket still applies. Absent on pre-v1.3 reports.

`calibration_applied` (0.7.0+) is `true` if any scoring window in the period had operator-supplied per-service calibration coefficients applied to the proxy energy. The flag is methodologically distinct from `scaphandre_used` and `energy_source_models`: those describe which energy source produced the numbers, this flag describes whether those numbers were further adjusted by operator coefficients.

## Aggregate

> **See also.** The [Energy and SCI primer](METHODOLOGY.md#background-energy-and-sci-primer) in the methodology doc defines SCI v1.0 terms (E, I, M), `efficiency_score`, `io_waste_ratio`, Scaphandre, SPECpower and the related vocabulary used by every field below.

Sums across the entire period and the entire `applications` array. `total_requests`, `total_energy_kwh`, `total_carbon_kgco2eq`, and `estimated_optimization_potential_kgco2eq` are non-negative finite numbers. `aggregate_waste_ratio` is in `[0, 1]`. `aggregate_efficiency_score` is in `[0, 100]` and equals `clamp(100 - 100 * io_waste_ratio, 0, 100)`. `anti_patterns_detected_count` is the sum of every per-service occurrences count, including non-avoidable patterns.

### Waste tiers (1.1+)

The report carries the avoidable energy and carbon at two N+1 detection thresholds, side by side, so the gap between them is auditable:

- `canonical_waste` is computed at a fixed N+1 threshold pinned in the binary (`2`), not the operator's config. It is the non-manipulable figure: an operator cannot shrink it by loosening their own threshold. This is the headline avoidable number, and the flat `estimated_optimization_potential_kgco2eq`, `aggregate_waste_ratio`, and `aggregate_efficiency_score` fields alias this tier since v1.1 (they carried the operational value in v1.0).
- `operational_waste` is computed at the operator's configured N+1 threshold and records that threshold in `n_plus_one_threshold`. Comparing it against `canonical_waste` shows how much avoidable waste the operator's threshold hides.

Each tier carries `n_plus_one_threshold` (integer), `energy_kwh` and `carbon_kgco2eq` (non-negative), `waste_ratio` (`[0, 1]`), and `efficiency_score` (`[0, 100]`). For `intent = "official"`, the validator requires `canonical_waste.n_plus_one_threshold` to equal the binary's canonical threshold; the operational threshold is the operator's recorded choice and is deliberately not range-checked, since a loose threshold is exactly what that tier exists to surface. The total energy and carbon (`total_energy_kwh`, `total_carbon_kgco2eq`) are span-derived and independent of either threshold.

### Database waste (v1.4)

`database_waste` is an optional object carrying the database-side avoidable energy and carbon over the period. It is a lower bound and an informational figure: never folded into `total_energy_kwh`, `total_carbon_kgco2eq` or the waste tiers, and absent on pre-v1.4 reports and when no window carried the figure. The per-window figure is the database energy multiplied by the SQL-only waste ratio: energy measured on the declared Alumet database cgroup when `[green.alumet.database]` is configured, otherwise estimated from the modeled energy of the window's SQL spans. `models` carries the distinct provenance tags observed over the period (`alumet_rapl` = measured, `estimated` = built from the modeled SQL-span energy), so an auditor can tell measurement from model without leaving the report. `windows_with_figure` counts the windows that carried the block. The `operational_*` figures use the operator's N+1 threshold, the `canonical_*` figures recompute the SQL ratio at the binary-pinned canonical threshold against the same energy, the same anti-manipulation construction as the waste tiers above.

### Quality signals (0.7.0+)

The aggregate carries four optional fields that describe the quality of the source archives, not the workload itself. These let auditors gauge how much of the period was directly measured versus inferred from a proxy.

- `period_coverage` is in `[0, 1]` and equals `runtime_windows / (runtime_windows + fallback_windows)`. A value of `1.0` means every scoring window in the period carried runtime-calibrated energy (Scaphandre or cloud SPECpower). A value of `0.0` means every window fell back to the I/O proxy. The validator refuses an `intent = "official"` disclosure with `period_coverage < 0.75`, see `docs/design/08-PERIODIC-DISCLOSURE.md` for the threshold rationale.
- `runtime_windows_count` and `fallback_windows_count` carry the absolute counts behind that ratio, so a reader can distinguish "9 out of 10 windows runtime-calibrated" from "900 out of 1000".
- `binary_versions` is the set of distinct perf-sentinel binary versions that produced the archives folded into this period. A period spanning several versions (daemon upgrade mid-quarter, async releases across teams) flags this set with more than one entry, which the report disclaimer surfaces.

### Per-service quality fields (0.7.0+)

- `per_service_energy_models` maps each service to the set of energy-model tags observed across the period (`scaphandre_rapl`, `cloud_specpower`, `io_proxy_v3`, etc.). The `+cal` suffix is stripped before insertion, the period-wide `calibration_applied` flag in `methodology.calibration_inputs` carries that information instead.
- `per_service_measured_ratio` is the per-service mean of the per-window fraction of spans whose energy was resolved by Scaphandre or cloud SPECpower. A value close to `1.0` means the service is fully measured across the period, `0.0` means it relies on proxy fallback. This is a simple arithmetic mean of per-window ratios, not span-weighted: a window with 10 spans and a window with 10000 spans contribute equally to the mean.

### Temporal coverage (v1.2)

`temporal_coverage` is a continuity signal: how much of the declared period actually carried measurements. It is an object with `temporal_coverage` (in `[0, 1]`, equal to `observed_days / days_in_period`), `observed_days` (distinct UTC calendar days carrying at least one archived window), `days_in_period` (mirrors `period.days_covered`), and `largest_gap_days` (the longest run of consecutive in-period days with no windows).

Read it as a lower bound on activity, not as daemon uptime. Daemon archiving is traffic-gated: a window with no traffic writes nothing, so legitimately quiet days (nights, weekends, low-traffic services) lower the figure. For that reason it is **never** a hard `official` gate. The `disclose` CLI publishes the value, emits a stderr warning below an informational threshold, and appends an in-band disclaimer carrying the same caveat. It exists so a reader can tell a continuously-measured period from one where the daemon ran only a handful of days, which the per-calendar-day `days_covered` alone cannot reveal.

## Applications

Two granularities, homogeneous per disclosure. The validator rejects a disclosure that mixes the two.

### G1 (intent `internal`)

Each entry carries the service-level totals plus an `anti_patterns: [...]` array. Every anti-pattern detail has `type` (one of the 10 known patterns), `occurrences`, `estimated_waste_kwh`, `estimated_waste_kgco2eq`, `first_seen`, and `last_seen`. Timestamps are RFC 3339 UTC. `rgesn_criteria` (v1.3) is the interpretive list of RGESN 2024 criteria the pattern relates to (see [docs/METHODOLOGY.md](METHODOLOGY.md#rgesn-2024-crosswalk)), empty for `slow_*` and absent on pre-v1.3 reports. `display_name` and `service_version` are optional hints.

### G2 (intent `official` with confidentiality `public`)

Each entry carries the same service-level totals but replaces the array with a single `anti_patterns_detected_count` integer. The schema enforces that G2 entries do not carry an `anti_patterns` field, and vice versa.

The two granularities are encoded in the JSON Schema with mutually exclusive `not: { required: [...] }` clauses to make the discrimination explicit to schema validators.

## Integrity

> **See also.** The [Sigstore primer](SUPPLY-CHAIN.md#background-sigstore-primer) in the supply-chain doc defines Cosign, Fulcio, Rekor, in-toto, OIDC and SLSA used throughout this section.

`content_hash` is `"sha256:<64-hex>"` over the canonical JSON form of the document with the `content_hash` field blanked to an empty string. The schema also accepts an empty string for the field so example files can ship without a baked-in hash. `binary_hash` is `"sha256:<64-hex>"` of the perf-sentinel binary that produced the file. `binary_verification_url` points at the release artefact where consumers can fetch the same binary. `trace_integrity_chain` is reserved for a future schema revision and is `null` today.

`signature` (0.7.0+) is either `null` (hash-only report) or a typed object with `format` (`"sigstore-cosign-intoto-v1"`), `bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, and `signed_at`. The fields collectively let a verifier locate the cosign bundle and the Rekor inclusion proof.

`binary_attestation` (0.7.0+) is optional and, when present, carries a `format` (`"slsa-provenance-v1"`), `attestation_url`, `builder_id`, `git_tag`, `git_commit`, and `slsa_level` (`"L2"` for v0.7.0, `"L3"` from v0.7.1 onward since the release workflow moved to `actions/attest-build-provenance` which produces a level-3 attestation by construction). Consumers verify the binary downloaded from `binary_verification_url` with `gh attestation verify <binary> --repo robintra/perf-sentinel` for 0.7.1+ releases, or with `slsa-verifier verify-artifact --provenance-path multiple.intoto.jsonl ...` for the legacy 0.7.0 release.

`cross_period_log` (v1.2) is reserved and absent today. It is the schema hook for an external append-only or Rekor-style log that chains successive periodic reports, so a third party can detect an operator who silently stopped publishing for several periods, the one gap the per-report integrity guarantees cannot close. It will be populated only under a future `intent = "audited"`, alongside the external audit attestation.

`integrity_level` in `report_metadata` is one of `none`, `hash-only`, `signed`, `signed-with-attestation` (0.7.0+), `audited`. The reader can use it as a fast filter before parsing the integrity block.

## Notes

`disclaimers` carries eight default statements: the two standard uncertainty disclaimers (directional estimate, ~2x multiplicative bracket; the SCI specification itself defines no uncertainty provisions), the embodied-carbon scope clarification (excluded from optimization potential), the embodied-per-service note (operational only at the service level, full at the aggregate), the runtime-attribution caveat (runtime-calibrated archives carry per-service data, older archives fall back to I/O share), two regulatory-fitness lines (not for CSRD / GHG Scope 3, methodology reference), and the ESRS E1 crosswalk note (the `standard_crosswalk` mapping is an aid, not a substitute for an audited CSRD inventory). Operators can override the list in their org-config TOML. `reference_urls` is an open object mapping short keys (`methodology`, `schema`, `project`) to URLs. Operators can add custom keys.

## Boavizta and other omitted fields

`boavizta_version` was considered for `calibration_inputs` but is not part of the schema today because perf-sentinel does not currently consume Boavizta data. The field will be re-introduced when the integration ships. Schema consumers MUST tolerate unknown fields gracefully because perf-sentinel will add them in minor revisions.

## Versioning

A backward-incompatible change to the schema increments the major version inside `schema_version` (`v2.0`, `v3.0`). Additive changes (new optional fields, new enum values that consumers can treat as unknown) increment the minor part (`v1.1`, `v1.2`). The JSON Schema `$id` URL contains the major version only.

## Cross-references

- `docs/REPORTING.md` is the operator-facing usage guide.
- `docs/METHODOLOGY.md` covers the calculation chain that fills `aggregate` and the per-application energy/carbon fields.
- `docs/schemas/perf-sentinel-report-v1.json` is the canonical JSON Schema.
- `docs/schemas/examples/example-internal-G1.json` and `example-official-public-G2.json` are filled examples.
