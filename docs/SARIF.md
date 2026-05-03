# SARIF output reference

perf-sentinel emits SARIF v2.1.0 via `--format sarif` on `analyze` and `diff`.
This reference lists the fields populated per `result`. For the cross-format
acknowledgments workflow, see [ACKNOWLEDGMENTS.md](./ACKNOWLEDGMENTS.md).

## Per-result fields

| Field | Source | Notes |
|---|---|---|
| `ruleId` | `Finding.finding_type` | Snake-case rule id (`n_plus_one_sql`, `redundant_http`, ...). |
| `level` | `Finding.severity` | `error` for critical, `warning` for warning, `note` for info. |
| `message.text` | composed from `Finding` fields | Human-readable summary including occurrences and window. |
| `logicalLocations[0]` | `Finding.service` | `kind: "module"`. |
| `logicalLocations[1]` | `Finding.source_endpoint` | `kind: "function"`. |
| `properties.confidence` | `Finding.confidence` | One of `ci_batch`, `daemon_staging`, `daemon_production`. Read by perf-lint to boost or reduce IDE severity. |
| `properties.signature` | `Finding.signature` | Canonical signature, also exposed in `fingerprints`. Skipped when empty (legacy baselines pre-0.5.17). |
| `properties.acknowledged` | `--show-acknowledged` | `true` for acknowledged entries, omitted for normal findings. |
| `properties.acknowledgmentReason` / `acknowledgmentBy` / `acknowledgmentAt` | ack metadata | BiDi and invisible-format characters are stripped before emission (Trojan Source defense). |
| `rank` | `Finding.confidence.sarif_rank()` | Integer 0 to 100. `ci_batch=30`, `daemon_staging=60`, `daemon_production=90`. |
| `locations[]` | `Finding.code_location` | Physical source location, populated when the instrumentation agent emits `code.filepath` and `code.lineno` span attributes. Hostile filepaths (absolute, traversal, BiDi, percent-encoded) are rejected at emission. |
| `fixes[]` | `Finding.suggested_fix` | Description-only SARIF fix object: free-text recommendation under `description.text`. |
| `fingerprints["perfsentinel/v1"]` | `Finding.signature` | SARIF v2.1.0 section 3.27.17 fingerprint. Used by GitHub Code Scanning and GitLab SAST for deduplication across runs. Skipped when the signature is empty. |

## Tool

`runs[].tool.driver` is always `{ name: "perf-sentinel", version: <CARGO_PKG_VERSION>, informationUri: "https://github.com/robintra/perf-sentinel" }`. The `rules` array pre-declares all 10 finding types regardless of how many results are emitted, so SARIF consumers can show rule descriptions even on a clean run.

## Schema

`$schema` points to `https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json`.
