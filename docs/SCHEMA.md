# Schema reference: perf-sentinel-report v1.0

This document describes the JSON shape of a periodic disclosure report in prose. The machine-readable JSON Schema lives at `docs/schemas/perf-sentinel-report-v1.json` (draft 2020-12). Two filled examples sit under `docs/schemas/examples/`.

## Top-level keys

| key               | type           | required | notes                                                                    |
|-------------------|----------------|----------|--------------------------------------------------------------------------|
| `schema_version`  | string (const) | yes      | `"perf-sentinel-report/v1.0"`                                            |
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

`intent` is one of `internal | official | audited`. `audited` is reserved for a future release: the JSON schema accepts the value for forward compatibility, but the CLI refuses it today with exit code 2. `confidentiality_level` is one of `internal | public`. `integrity_level` is one of `none | hash-only | signed | audited`. The v1.0 schema produces `hash-only`. `generated_at` is an RFC 3339 UTC timestamp. `generated_by` is one of `daemon | cli-batch | ci`. `perf_sentinel_version` is the SemVer string of the binary that wrote the file. `report_uuid` is a v4 UUID stamped per run.

## Organisation

`name` is required and non-empty. `country` is ISO 3166-1 alpha-2, upper case. `identifiers` is an open object with optional `siren`, `vat`, `lei`, `opencorporates_url`, `domain`. `sector` is an optional NACE rev2 code.

The publication domain (e.g. `transparency.example.fr`) is treated as an implicit identifier when `notes.reference_urls.project` is published from that host. The schema does not enforce this, by design.

## Period

`from_date` and `to_date` are ISO 8601 calendar dates (`YYYY-MM-DD`). `period_type` is one of `calendar-quarter | calendar-month | calendar-year | custom`. `days_covered` is `to_date - from_date + 1`. The official-intent validator enforces `days_covered >= 30`.

## Scope manifest

`total_applications_declared` is the size of the organisation's application portfolio. `applications_measured` is the count of services for which the disclosure carries data. Each entry in `applications_excluded` carries `service_name` and a non-empty `reason`. `environments_measured` lists the operator-defined environments observed (e.g. `["prod"]`). `total_requests_in_period` is an optional operator estimate; `requests_measured` is what perf-sentinel actually saw. `coverage_percentage` is `requests_measured / total_requests_in_period * 100` when the former is set.

## Methodology

`sci_specification` references the SCI revision (e.g. `"ISO/IEC 21031:2024"`). `perf_sentinel_version` mirrors the report metadata field for consumers that index only the methodology block. `enabled_patterns` and `disabled_patterns` each carry pattern names taken from the closed set defined by `FindingType::as_str()` (10 values). `core_patterns_required` is the closed list of patterns whose remediation directly cuts I/O and carbon: `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`. `conformance` is one of `core-required | extended | partial`; `core-required` is the minimum bar for an `intent = "official"` disclosure. `calibration_inputs.carbon_intensity_source` is one of `electricity_maps | static_tables | mixed`. `specpower_table_version` is the version of the embedded SPECpower table; the binary ships the only authoritative copy. `scaphandre_used` flags whether the runtime energy proxy came from Scaphandre RAPL.

`calibration_applied` (0.7.0+) is `true` if any scoring window in the period had operator-supplied per-service calibration coefficients applied to the proxy energy. The flag is methodologically distinct from `scaphandre_used` and `energy_source_models`: those describe which energy source produced the numbers, this flag describes whether those numbers were further adjusted by operator coefficients.

## Aggregate

Sums across the entire period and the entire `applications` array. `total_requests`, `total_energy_kwh`, `total_carbon_kgco2eq`, and `estimated_optimization_potential_kgco2eq` are non-negative finite numbers. `aggregate_waste_ratio` is in `[0, 1]`. `aggregate_efficiency_score` is in `[0, 100]` and equals `clamp(100 - 100 * io_waste_ratio, 0, 100)`. `anti_patterns_detected_count` is the sum of every per-service occurrences count, including non-avoidable patterns.

### Quality signals (0.7.0+)

The aggregate carries four optional fields that describe the quality of the source archives, not the workload itself. These let auditors gauge how much of the period was directly measured versus inferred from a proxy.

- `period_coverage` is in `[0, 1]` and equals `runtime_windows / (runtime_windows + fallback_windows)`. A value of `1.0` means every scoring window in the period carried runtime-calibrated energy (Scaphandre or cloud SPECpower). A value of `0.0` means every window fell back to the I/O proxy. The validator refuses an `intent = "official"` disclosure with `period_coverage < 0.75`, see `docs/design/08-PERIODIC-DISCLOSURE.md` for the threshold rationale.
- `runtime_windows_count` and `fallback_windows_count` carry the absolute counts behind that ratio, so a reader can distinguish "9 out of 10 windows runtime-calibrated" from "900 out of 1000".
- `binary_versions` is the set of distinct perf-sentinel binary versions that produced the archives folded into this period. A period spanning several versions (daemon upgrade mid-quarter, async releases across teams) flags this set with more than one entry, which the report disclaimer surfaces.

### Per-service quality fields (0.7.0+)

- `per_service_energy_models` maps each service to the set of energy-model tags observed across the period (`scaphandre_rapl`, `cloud_specpower`, `io_proxy_v3`, etc.). The `+cal` suffix is stripped before insertion, the period-wide `calibration_applied` flag in `methodology.calibration_inputs` carries that information instead.
- `per_service_measured_ratio` is the per-service mean of the per-window fraction of spans whose energy was resolved by Scaphandre or cloud SPECpower. A value close to `1.0` means the service is fully measured across the period, `0.0` means it relies on proxy fallback. This is a simple arithmetic mean of per-window ratios, not span-weighted: a window with 10 spans and a window with 10000 spans contribute equally to the mean.

## Applications

Two granularities, homogeneous per disclosure. The validator rejects a disclosure that mixes the two.

### G1 (intent `internal`)

Each entry carries the service-level totals plus an `anti_patterns: [...]` array. Every anti-pattern detail has `type` (one of the 10 known patterns), `occurrences`, `estimated_waste_kwh`, `estimated_waste_kgco2eq`, `first_seen`, and `last_seen`. Timestamps are RFC 3339 UTC. `display_name` and `service_version` are optional hints.

### G2 (intent `official` with confidentiality `public`)

Each entry carries the same service-level totals but replaces the array with a single `anti_patterns_detected_count` integer. The schema enforces that G2 entries do not carry an `anti_patterns` field, and vice versa.

The two granularities are encoded in the JSON Schema with mutually exclusive `not: { required: [...] }` clauses to make the discrimination explicit to schema validators.

## Integrity

`content_hash` is `"sha256:<64-hex>"` over the canonical JSON form of the document with the `content_hash` field blanked to an empty string. The schema also accepts an empty string for the field so example files can ship without a baked-in hash. `binary_hash` is `"sha256:<64-hex>"` of the perf-sentinel binary that produced the file. `binary_verification_url` points at the release artefact where consumers can fetch the same binary. `trace_integrity_chain` is reserved for a future schema revision and is `null` in v1.0.

`signature` (0.7.0+) is either `null` (hash-only report) or a typed object with `format` (`"sigstore-cosign-intoto-v1"`), `bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, and `signed_at`. The fields collectively let a verifier locate the cosign bundle and the Rekor inclusion proof.

`binary_attestation` (0.7.0+) is optional and, when present, carries a `format` (`"slsa-provenance-v1"`), `attestation_url`, `builder_id`, `git_tag`, `git_commit`, and `slsa_level` (`"L2"` for v0.7.0, `"L3"` from v0.7.1 onward since the release workflow moved to `actions/attest-build-provenance` which produces a level-3 attestation by construction). Consumers verify the binary downloaded from `binary_verification_url` with `gh attestation verify <binary> --owner robintra --repo perf-sentinel` for 0.7.1+ releases, or with `slsa-verifier verify-artifact --provenance-path multiple.intoto.jsonl ...` for the legacy 0.7.0 release.

`integrity_level` in `report_metadata` is one of `none`, `hash-only`, `signed`, `signed-with-attestation` (0.7.0+), `audited`. The reader can use it as a fast filter before parsing the integrity block.

## Notes

`disclaimers` carries seven default statements: the two standard SCI uncertainty lines (directional estimate, ~2x bracket), the embodied-carbon scope clarification (excluded from optimization potential), the embodied-per-service note (operational only at the service level, full at the aggregate), the runtime-attribution caveat (runtime-calibrated archives carry per-service data, older archives fall back to I/O share), and two regulatory-fitness lines (not for CSRD / GHG Scope 3, methodology reference). Operators can override the list in their org-config TOML. `reference_urls` is an open object mapping short keys (`methodology`, `schema`, `project`) to URLs. Operators can add custom keys.

## Boavizta and other omitted fields

`boavizta_version` was considered for `calibration_inputs` but is not part of v1.0 because perf-sentinel does not currently consume Boavizta data. The field will be re-introduced when the integration ships. Schema consumers MUST tolerate unknown fields gracefully because perf-sentinel will add them in minor revisions.

## Versioning

A backward-incompatible change to the schema increments the major version inside `schema_version` (`v2.0`, `v3.0`). Additive changes (new optional fields, new enum values that consumers can treat as unknown) increment the minor part (`v1.1`, `v1.2`). The JSON Schema `$id` URL contains the major version only.

## Cross-references

- `docs/REPORTING.md` is the operator-facing usage guide.
- `docs/METHODOLOGY.md` covers the calculation chain that fills `aggregate` and the per-application energy/carbon fields.
- `docs/schemas/perf-sentinel-report-v1.json` is the canonical JSON Schema.
- `docs/schemas/examples/example-internal-G1.json` and `example-official-public-G2.json` are filled examples.
