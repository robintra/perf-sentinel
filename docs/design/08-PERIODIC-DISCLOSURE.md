# Periodic disclosure report

Design notes for the periodic public disclosure pipeline: schema v1.0, aggregator, validator, daemon archive, and the `disclose` subcommand. Operator-facing usage lives in `docs/REPORTING.md`; the calculation chain lives in `docs/METHODOLOGY.md`; the wire reference lives in `docs/SCHEMA.md`. This document explains the design decisions behind each module.

## Module layout

```
crates/sentinel-core/src/report/periodic/
  ├── mod.rs        // re-exports
  ├── schema.rs     // v1.0 wire types
  ├── errors.rs     // ValidationError, HashError, AggregationError
  ├── hasher.rs     // canonical JSON + SHA-256 + binary_hash helper
  ├── validator.rs  // validate_official, validate_content_hash
  ├── aggregator.rs // NDJSON archive reader, per-service attribution
  └── org_config.rs // operator-supplied TOML loader

crates/sentinel-core/src/daemon/archive.rs   // archive writer
crates/sentinel-cli/src/disclose.rs          // CLI dispatcher
```

The split mirrors the pipeline pattern of the rest of the crate: pure functions over data, traits only at I/O borders (`std::fs` for the org-config and archive, `tokio::sync::mpsc` for the writer task). No new abstractions between stages.

## Schema determinism

The content hash is a SHA-256 over the canonical JSON form of the report with `integrity.content_hash` blanked to an empty string. Three invariants make this reproducible across builds and across consumers:

1. **Field order is the struct declaration order.** `serde_json` preserves struct field order during serialisation. Reordering fields in `schema.rs` is therefore a hash-breaking change and must be accompanied by a schema-version bump.
2. **Every map type is `BTreeMap`.** `HashMap` iterates in non-deterministic order and would defeat the hash. The schema uses `BTreeMap<String, String>` for `notes.reference_urls`, and the aggregator's intermediate buffers (`per_service`, `anti_patterns`, `first_seen`, `last_seen`) follow the same discipline.
3. **`Application::G1` and `Application::G2` are `#[serde(untagged)]`.** No discriminator field, dispatch by required-field presence (`anti_patterns` for G1, `anti_patterns_detected_count` for G2). The applications array is enforced as homogeneous by the validator, so the type level is permissive but the runtime invariant is strict.

The hasher implementation (`hasher.rs`) then runs `canonicalize(Value)` which rebuilds every JSON object via `BTreeMap<String, Value>` and recurses into arrays. This is defensive: `serde_json::Map` without the `preserve_order` feature is already a `BTreeMap`, but the explicit pass keeps the implementation correct if a future dependency enables the feature transitively.

The hash output is `"sha256:<64-hex>"`. Hex encoding is hand-rolled (`{byte:02x}`) to avoid pulling the `hex` crate, matching the existing pattern in `crate::acknowledgments`.

### Why blank the value instead of removing the key

Setting `content_hash` to `""` (empty string) preserves the key in the canonical form. Consumers verifying the hash do not have to know whether to add or strip the key; they just replace whatever value they read with `""` and recompute. The schema accepts both `^sha256:[0-9a-f]{64}$` and the empty string for the field so example files can ship with a placeholder.

## G1 / G2 granularity

The two granularities exist because publishable transparency reports must not leak per-anti-pattern detail (which can read like a runbook of weaknesses) while internal drafts benefit from it. The validator enforces:

- `confidentiality = "internal"` accepts G1 or G2.
- `confidentiality = "public"` requires G2.
- Mixing G1 and G2 entries in the same `applications` array is rejected.

The `#[serde(untagged)]` choice over an explicit discriminator was made because:

- The discrimination is structural (`anti_patterns` vs `anti_patterns_detected_count`) and JSON Schema can already express it with `oneOf` plus `not: { required }` constraints.
- The applications array is meant to be homogeneous, so an external consumer parsing the JSON does not need to handle a mixed-tag array.
- Internal Rust callers also work on a homogeneous slice in practice, so the `match` on `Application::G1(_)` / `Application::G2(_)` is local to a few sites in the CLI builder.

## Validator collect-all

`validate_official` returns `Result<(), Vec<ValidationError>>` and accumulates every rule violation in one pass rather than bailing on the first. Rationale:

- Operators configuring an `intent = "official"` daemon fix the org-config in one round trip instead of discovering missing fields one at a time across restart cycles.
- Reviewers reading an unsuccessful CLI invocation see the full list of structural problems immediately.

The function dispatches to per-section helpers (`validate_organisation`, `validate_period`, `validate_scope_manifest`, `validate_methodology`, `validate_aggregate`, `validate_applications`). Each helper takes `&mut Vec<ValidationError>` and pushes. Sub-rules inside a helper continue running after a push: for instance, the methodology helper validates every entry of `enabled_patterns` and `core_patterns_required` against `KNOWN_PATTERNS` even if an early entry was already rejected.

`KNOWN_PATTERNS` is a `const &[&str]` in `validator.rs` that mirrors the variants of `FindingType`. A test (`known_patterns_matches_finding_type_count`) uses a match-exhaustive pattern on `FindingType` to force a CI failure if a future variant is added without updating the list.

`intent = "internal"` is a no-op: a draft is allowed to be incomplete. `intent = "audited"` short-circuits with a single `ValidationError::AuditedNotImplemented`, accepted by the JSON schema for forward-compatibility but unimplemented at runtime.

## Aggregator and per-service attribution

The aggregator reads NDJSON files (or directories of `*.ndjson` files), where each line is an envelope:

```json
{"ts":"<RFC 3339 UTC>","report":{...full Report...}}
```

For each in-period envelope:

1. **Global counters** sum `total_io_ops`, `avoidable_io_ops`, `total.mid` (gCO2), `avoidable.mid` (gCO2). gCO2 is divided by 1000 to obtain kgCO2eq.
2. **Per-service distribution** reads `Report.per_endpoint_io_ops` for the set of services that produced I/O in the window. Each service gets a share of the window's energy/carbon proportional to its I/O ops share.
3. **Finding attribution** walks `Report.findings`. Each finding is bucketed under its `service` and `finding_type.as_str()`. `first_seen` and `last_seen` track the window timestamp range per `(service, pattern_type)`.

When a window has zero entries in `per_endpoint_io_ops`, its global totals fall into the `"_unattributed"` bucket and the bucket surfaces in the applications array. This is a deliberate trade-off: silently dropping the window would inflate the per-service shares of subsequent windows; aborting the run on a single sparse window would be too aggressive for many real deployments. The `--strict-attribution` flag (and the corresponding `AggregationError::UnattributedWindow` variant) is the escape hatch for operators who prefer the strict posture.

Malformed lines (parse failures) are skipped with a `tracing::warn!` and counted in `malformed_lines_skipped`. The aggregator does not refuse to proceed on isolated parse errors. The motivation is the daemon archive: a partially-written line during a crash should not poison the entire period.

## Daemon archive writer

The writer is a `tokio::spawn` task fed by a bounded `tokio::sync::mpsc::Sender<OwnedArchive>` with capacity 256. Producer-side (in `process_traces`) calls `handle.try_send(OwnedArchive { ts, report })` so the daemon's per-window scoring path never blocks on disk I/O. Sending the typed `OwnedArchive` (not a pre-serialised string) keeps the `serde_json::to_string` cost off the hot path and lets the writer task amortise it against disk I/O.

The bounded channel uses drop-on-full: when the writer falls behind, new windows are dropped with a `tracing::warn!`. The 256-message capacity is sized so a steady-state stalled writer surfaces within seconds rather than letting an unbounded queue OOM the daemon.

Rotation triggers when `bytes_written` exceeds `max_size_mb * 1_048_576`. The active file is renamed to `<stem>-<UTC-timestamp>.ndjson` first, then a fresh file is opened via `OpenOptions::create_new(true).append(true)` to close the TOCTOU race where a co-resident attacker could plant a symlink between the rename and the re-open. `prune` removes the oldest rotated files until at most `max_files` remain. Pruning sorts by `mtime` descending and validates the timestamp suffix matches the `is_rotation_stamp` shape, so an unrelated file in the archive directory (e.g. `archive-evil.ndjson`) is never deleted.

`metadata_len` reads the existing file size at startup so the writer resumes correctly after a daemon restart without immediately rotating a near-full file.

### Why archive `Report` objects rather than findings

The aggregator needs `green_summary` (for energy/carbon) and `per_endpoint_io_ops` (for per-service attribution). A `findings` stream alone does not carry those. The daemon builds a `Report` from `findings + green_summary + per_endpoint_io_ops + analysis` immediately after `emit_findings_and_update_metrics`, then sends the serialised envelope. The cost is one `Vec<Finding>::clone` and one `serde_json::to_string` per window when the archive is enabled.

`per_endpoint_io_ops` was previously bound to `_` in `process_traces` (the value was already computed by `score_green` but discarded). Keeping it for the archive is a no-cost change in the hot path.

## Org-config TOML

The operator-supplied TOML is a partial blueprint for the static fields of a `PeriodicReport`. It carries `organisation`, `methodology`, `scope_manifest` (less the runtime numbers), and optional `notes`. The aggregator fills in the runtime sections (`aggregate`, `applications`, `integrity`).

`load_from_path` returns `OrgConfig` or `OrgConfigError` (`Io` or `Parse`). `validate_for_official` returns `Vec<String>` rather than typed errors because the daemon flattens them into `DaemonError::ReportingValidation { errors: Vec<String> }` for human-readable startup logging. The CLI's `disclose` subcommand calls the typed `validate_official` on the full assembled report so it can also catch aggregate-level violations (e.g. empty `applications`, ratio out of range).

The TOML fields mirror the wire schema verbatim. This is deliberate: an operator who reads the JSON Schema can write the TOML without consulting a second document, and a maintainer who renames a wire field must rename it in both places.

## Daemon startup gate

`daemon::run` calls `validate_official_reporting` before allocating any resource. The helper:

1. Returns `Ok` when `[reporting] intent != "official"`.
2. Loads the org-config from `[reporting] org_config_path`. Missing path or unreadable file becomes an entry in the error vec.
3. Calls `org_config::validate_for_official` and folds its `Vec<String>` into the same vec.
4. Returns `Err(DaemonError::ReportingValidation { errors })` if anything fails, with `Display` producing one indented line per error so journalctl / kubectl logs render nicely.

Listeners do not spawn when validation fails; the daemon exits with a non-zero status. Operators that prefer a soft mode set `intent = "internal"` (or omit the section).

## CLI dispatcher

`Commands::Disclose` was chosen over an extension of the existing `Commands::Report` to avoid breaking the CLI surface (`Report` is already the HTML/JSON dashboard subcommand). The verb `disclose` matches the operator vocabulary for transparency publication and reads cleanly in shell scripts.

The dispatcher (`disclose.rs::cmd_disclose`) returns `i32` so the caller can `std::process::exit(code)` directly. The contract:

- `0`: success, file written.
- `1`: I/O or parse failure (org-config unreadable, output unwritable, hash error).
- `2`: validation failure or `audited` short-circuit. The error list is printed to stderr.

`audited` is caught first, before any I/O, so the user gets the "not yet implemented" message regardless of org-config state.

`generated_by` is set to `"ci"` when `$CI` is in the environment, `"cli-batch"` otherwise. The daemon path will use `"daemon"` once scheduled disclosures are added; this is a placeholder for the field's three documented values.

## Verification commands

A consumer recomputes the content hash with:

```bash
jq -c '.integrity.content_hash = ""' perf-sentinel-report.json \
  | jq -cS '.' \
  | shasum -a 256
```

The `jq -cS` step canonicalises object keys via jq's built-in `S` flag, which matches the `canonicalize` step in `hasher.rs`. The number-formatting may differ on inputs with non-default JSON representations of floats; the schema only uses `f64` values that `serde_json` emits in shortest round-tripping form, which is also what jq emits, so in practice both produce the same bytes.

## Configuration hooks

Two new config sections in `.perf-sentinel.toml`:

- `[reporting]` carries `intent`, `confidentiality_level`, `org_config_path`, `disclose_output_path`, `disclose_period`. Validated at config load.
- `[daemon.archive]` carries `path`, `max_size_mb` (default 100), `max_files` (default 12). Validated at config load and at archive open.

Both sections are optional. Their absence leaves perf-sentinel in its prior behaviour: NDJSON to stdout, no archive, no reporting gate.

## v1.0 limitations carried as disclaimers

- **Runtime-calibrated energy + per-service carbon when present.** `Builder::process_window` reads the source window's `green_summary.energy_kwh` and `per_service_carbon_kgco2eq` / `per_service_energy_kwh` / `per_service_region` maps when they are populated, and falls back to the I/O proxy + share distribution when they are not (sprint-1 archives). The aggregator surfaces the observed `energy_model` tags under `methodology.calibration_inputs.energy_source_models`. See `docs/design/09-CARBON-ATTRIBUTION.md`.
- **Optimization potential excludes embodied.** `estimated_optimization_potential_kgco2eq` sums `co2.avoidable.mid` only. `total_carbon_kgco2eq` is the full `co2.total.mid` (operational + embodied). The default disclaimers spell this out.
- **`_unattributed` co-routes findings.** A window with no `per_endpoint_io_ops` and no runtime per-service maps lands its energy/carbon AND its findings under `_unattributed`. Without this routing, a service with N+1 findings could publish at `efficiency_score = 100` when its `total_io_ops` happened to be zero in the same window.

## Future revisions

- **Sigstore signature**: `integrity.signature` is reserved. Adding a real signature is a SemVer-minor schema bump (additive field becoming non-null in some files).
- **`audited` intent**: the third intent value will require an external audit attestation. The shape will live under `integrity` or in a sibling section; not decided yet.
- **Trace integrity chain**: `integrity.trace_integrity_chain` is reserved for a Merkle root over the source traces that fed the disclosure. Out of scope for sprint 1.
- **Boavizta integration**: `methodology.calibration_inputs` will gain a `boavizta_version` field when the integration ships. Schema consumers must tolerate unknown calibration fields, which they already do because `additionalProperties` is unset.

## Source file mapping

| Source file                                            | Topic                                          |
|--------------------------------------------------------|------------------------------------------------|
| `report/periodic/schema.rs`                            | wire types, determinism invariants             |
| `report/periodic/hasher.rs`                            | canonical JSON + SHA-256, binary hash          |
| `report/periodic/validator.rs`                         | collect-all validator, KNOWN_PATTERNS          |
| `report/periodic/aggregator.rs`                        | NDJSON folding, per-service attribution        |
| `report/periodic/org_config.rs`                        | operator TOML loader                           |
| `report/periodic/errors.rs`                            | error enums                                    |
| `daemon/archive.rs`                                    | non-blocking NDJSON writer with rotation/prune |
| `daemon/mod.rs` (`validate_official_reporting`)        | startup gate                                   |
| `daemon/event_loop.rs`                                 | archive hook in `process_traces`               |
| `config.rs` (`ReportingConfig`, `DaemonArchiveConfig`) | TOML sections + validators                     |
| `sentinel-cli/src/disclose.rs`                         | CLI dispatcher, value enums, build_report      |
