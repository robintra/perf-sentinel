# Configuration reference

perf-sentinel is configured via a `.perf-sentinel.toml` file. All fields are optional and have sensible defaults.

<img alt="CLI commands overview" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands.svg">

## Contents

- [Configuration fragments](#configuration-fragments): deterministic multi-file loading.
- [Subcommands](#subcommands): which subcommands read `.perf-sentinel.toml`.
- [Sections](#sections): full per-section reference (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`, `[reporting]`).
- [Minimal configuration](#minimal-configuration): the smallest useful `.perf-sentinel.toml`.
- [Full configuration example](#full-configuration-example): every section populated with example values.
- [Migration from 0.5.x](#migration-from-05x): the 8 legacy top-level keys removed in 0.6.0 and how to migrate.
- [Environment variables](#environment-variables): which env vars override config-file values.

## Configuration fragments

perf-sentinel loads TOML documents from the directory `.perf-sentinel.d/`
beside the main config, then loads `.perf-sentinel.toml` last. This also
applies when `--config path/to/custom.toml` is used: fragments come from
`path/to/.perf-sentinel.d/` and `custom.toml` remains the final override.
The main file is optional only when `--config` is not supplied.
Defaults are used only when neither the implicit main file nor any fragment
exists. An unreadable file or an individual TOML parse error stops the command
with exit code 75. After all overrides are applied, the merged configuration
must pass typed deserialization and validation or the command also stops.

Fragment names must follow `NN-lowercase-name.toml`, where `NN` is a unique
two-digit priority from `00` to `99`. Files load in ascending priority order.
Duplicate priorities, uppercase names and ambiguous separators are rejected.
Non-TOML files in the directory are ignored.

When both values are tables, they merge recursively. Any other later value
replaces the earlier value at the same key. The final merged document must
still match the typed configuration schema. Since 0.12.0 that schema is
strict: a key or a table name no section declares fails the load instead of
being ignored, so a misspelled knob stops the command rather than silently
leaving the default in place. The examples use these reserved bands:

| Priority     | Purpose                                                 |
|--------------|---------------------------------------------------------|
| `00` to `19` | shared defaults, thresholds and detection               |
| `20` to `39` | energy sources and GreenOps measurement                 |
| `40` to `49` | carbon-intensity sources                                |
| `50` to `69` | daemon and deployment topology                          |
| `70` to `89` | reporting and organisation-specific policy              |
| `90` to `99` | local overrides, preferably kept out of version control |

The ready-to-copy fragments in `examples/` preserve their priority in their
filename. Keep those names when copying them into `.perf-sentinel.d/`:
`30-green-alumet.toml`, `31-green-cloud.toml`,
`32-green-scaphandre.toml`, `33-green-kepler.toml`,
`34-green-redfish.toml` and `40-green-electricity-maps.toml`.
`60-daemon-docker.toml` is a standalone main config for the collector and
sharded Compose topologies. Mount it as `.perf-sentinel.toml`, then place only
the optional GreenOps fragments in the sibling `.perf-sentinel.d/` directory.

## Subcommands

| Subcommand     | Description                                                                                                                                                                                                     |
|----------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `analyze`      | Batch analysis of trace files. Reads from file or stdin                                                                                                                                                         |
| `explain`      | Tree view of a specific trace with findings annotated inline                                                                                                                                                    |
| `watch`        | Daemon mode: real-time OTLP ingestion and streaming detection                                                                                                                                                   |
| `query`        | Query a running daemon for findings, correlations or status. Colored text output by default, `--format json` for scripting. `query inspect` opens a live TUI                                                    |
| `demo`         | Run analysis on an embedded demo dataset                                                                                                                                                                        |
| `bench`        | Benchmark throughput on a trace file                                                                                                                                                                            |
| `pg-stat`      | Analyze `pg_stat_statements` exports (CSV/JSON or Prometheus)                                                                                                                                                   |
| `inspect`      | Interactive TUI to browse traces, findings and span trees                                                                                                                                                       |
| `diff`         | Compare two trace sets and emit a delta report (new/resolved findings, severity changes, per-endpoint I/O op deltas). Text/JSON/SARIF output                                                                    |
| `report`       | Single-file HTML dashboard for post-mortem exploration in any browser. Accepts a trace file, a pre-computed Report JSON, or stdin via `--input -` (auto-detects array-of-events vs Report object, BOM-tolerant) |
| `tempo`        | Fetch traces from a Grafana Tempo HTTP API (single trace by ID or search-then-fetch by service) and pipe them through the analysis pipeline. Gated behind the `tempo` feature                                   |
| `jaeger-query` | Fetch traces from any Jaeger query API backend (Jaeger, Victoria Traces) and pipe them through the analysis pipeline. Gated behind the `jaeger-query` feature                                                   |
| `calibrate`    | Correlate a trace file with measured energy readings (Scaphandre, cloud monitoring CSV) and emit a TOML of I/O-to-energy coefficients to load via `[green] calibration_file`                                    |

## Sections

### `[thresholds]`

Quality gate thresholds. The quality gate fails if any rule is violated.

| Field                              | Type    | Default | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
|------------------------------------|---------|---------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `n_plus_one_sql_critical_max`      | integer | `0`     | Maximum number of **critical** N+1 SQL findings before the gate fails                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `n_plus_one_http_warning_max`      | integer | `3`     | Maximum number of **warning or higher** N+1 HTTP findings before the gate fails                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `n_plus_one_messaging_warning_max` | integer | `3`     | Maximum number of **warning or higher** N+1 messaging findings before the gate fails. Warning+ rather than critical-only, like HTTP: a Kafka client may already batch the publishes it buffers, so the occurrence count is an upper bound there                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `io_waste_ratio_max`               | float   | `0.30`  | Maximum I/O waste ratio (0.0 to 1.0) before the gate fails                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `min_usable_span_ratio`            | float   | unset   | Minimum share (0.0 to 1.0) of I/O-shaped spans that must be analyzable (SQL spans carrying `db.statement`, HTTP CLIENT spans carrying a full URL) before the gate fails, computed per I/O kind and reported as the worst of them so healthy HTTP traffic cannot mask a broken SQL surface. An I/O kind carrying fewer than 20 spans is left unjudged, too small a sample for a build-blocking ratio; when no kind clears that floor the rule is skipped and the report carries a `tuning` warning saying so. Batch runs only (`analyze`, `report`), the daemon exports no tally. Guards against a false green from unusable instrumentation: below the threshold the gate fails even with zero findings. Unset disables the rule. Applies to OTLP input only, which carries the per-reason filter tally (surfaced in `analysis.ingest` of the JSON report) |

### `[detection]`

Detection algorithm parameters.

| Field                                  | Type    | Default                                       | Description                                                                                                                                                                                                                                                                                                                                              |
|----------------------------------------|---------|-----------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `n_plus_one_min_occurrences`           | integer | `5`                                           | Minimum number of occurrences (with distinct params) to flag an N+1 pattern                                                                                                                                                                                                                                                                              |
| `window_duration_ms`                   | integer | `500`                                         | Time window in milliseconds within which repeated operations are considered an N+1 pattern                                                                                                                                                                                                                                                               |
| `slow_query_threshold_ms`              | integer | `500`                                         | Duration threshold in milliseconds above which an operation is considered slow                                                                                                                                                                                                                                                                           |
| `slow_query_min_occurrences`           | integer | `3`                                           | Minimum number of slow occurrences of the same template to generate a finding                                                                                                                                                                                                                                                                            |
| `max_fanout`                           | integer | `20`                                          | Maximum child spans per parent before flagging as excessive fanout (range: 1-100000)                                                                                                                                                                                                                                                                     |
| `chatty_service_min_calls`             | integer | `15`                                          | Minimum HTTP outbound calls per trace to flag as chatty service. Severity: warning > threshold, critical > 3x threshold.                                                                                                                                                                                                                                 |
| `pool_saturation_concurrent_threshold` | integer | `10`                                          | Peak concurrent SQL spans per service to flag connection pool saturation risk. Uses a sweep-line algorithm on span timestamps.                                                                                                                                                                                                                           |
| `serialized_min_sequential`            | integer | `3`                                           | Minimum sequential independent sibling calls (same parent, no time overlap, different templates) to flag as potentially parallelizable.                                                                                                                                                                                                                  |
| `grouping_attributes`                  | list    | `["k8s.namespace.name", "service.namespace"]` | Resource or span attributes that separate one deployment from another, most specific first. The first one present on a span decides finding identity, so the same problem in two namespaces stays two findings. Every present attribute is captured and displayed. Max 8 entries, an empty list turns grouping off. Acknowledgment signatures ignore it. Since 0.19.0 the effective value is also the `grouping` label on the daemon's per-service metrics, see `[daemon] per_grouping_labels`. |
| `sanitizer_aware_classification`       | string  | `"auto"`                                      | How to classify SQL groups whose literals were collapsed to a placeholder (`?`, `$?`, `%s`, `@param`, `:name`) by an OTel agent or database driver. One of `"auto"`, `"strict"`, `"always"`, `"never"`. See note below.                                                                                                                                  |
| `sanitizer_aware_min_cv`               | number  | `0.5`                                         | Coefficient of variation (standard deviation over mean) of the per-span durations above which the sanitizer heuristic reads a group as N+1 rather than a cached repeat. Range `(0, 10]`. See note below.                                                                                                                                                  |

#### `sanitizer_aware_classification`

OpenTelemetry agents and database drivers ship with SQL statement
sanitization ON by default to keep PII out of trace attributes. The
placeholder style depends on the stack: JDBC agents produce bare `?`,
PostgreSQL native drivers (pgx, asyncpg, sqlx) produce `$1`/`$2`
(normalized to `$?`), Python DB-API drivers produce `%s`, .NET drivers
produce `@p0`/`@Name`, and Oracle/SQLAlchemy produce `:name`. In all
cases the spans reach perf-sentinel with the same template and no
extractable parameters, so the standard distinct-params rule rejects
the group and the redundant detector picks it up as `redundant_sql`
instead of `n_plus_one_sql`. This setting controls the heuristic that
recovers the correct classification:

- `"auto"` (default): emit `n_plus_one_sql` when **either** the ORM
  scope signal (Spring Data, Hibernate, EF Core, SQLAlchemy,
  ActiveRecord, GORM, Prisma, Diesel, Laravel/Eloquent, Doctrine, ...)
  **or** the per-span timing
  variance is high enough to indicate distinct row lookups. Otherwise
  leave the group to the redundant detector. Best recall on production
  Spring Data, EF Core and similar ORM stacks.
- `"strict"`: reclassify only when a primary signal (ORM scope marker,
  high occurrence >= 3 x `n_plus_one_min_occurrences`, or sequential
  siblings) fires conjointly with a corroborating signal (high timing
  variance or high occurrence). Preserves `redundant_sql` precision on
  moderate-count cached identical queries (legacy polling loops,
  unmemoized config lookups, typically 5-10 calls per request). Above
  the high-occurrence bar (default 15), any sanitized group fires
  regardless of ORM scope, sequential siblings, or variance, under the
  `looks_sanitized` guard. Use this when actionable `redundant_sql`
  findings are valuable signal that should not be silently absorbed
  into `n_plus_one_sql`. The simulation lab runs all of its stacks this
  way, because under `auto` an ORM scope marker alone reclassifies a
  cache-warmed repeat of the same query as an N+1. The change of verdict
  only applies to moderate counts, since the high-occurrence bar above
  fires under `strict` as well.
- `"always"`: reclassify any sanitized group with at least
  `n_plus_one_min_occurrences` spans as `n_plus_one_sql`. Aggressive,
  may flip a real single-param redundancy.
- `"never"`: disable the heuristic entirely and fall back to the strict
  `distinct_params` check.

Findings reclassified by the heuristic (whether under `"auto"`,
`"strict"`, or `"always"`) carry `classification_method =
"sanitizer_heuristic"` in their JSON representation so operators can
spot where it is firing. Findings produced by the standard rule omit
the field.

#### `sanitizer_aware_min_cv`

The timing-variance signal behind the modes above compares the
coefficient of variation of the group's per-span durations to this
threshold. Row lookups against different keys spread their durations
across cache hits and misses, repeats of one cached query cluster. The
default of `0.5` favors reporting an N+1 over missing one, since a wrong
call only swaps `redundant_sql` for `n_plus_one_sql` at the same
avoidable-I/O weight.

Raise it on a runtime whose scheduling jitter spreads even cached
repeats: PHP-FPM workers, CPU-throttled containers, shared CI runners.
The simulation lab measured a CV of about 0.75 on ten identical Doctrine
lookups served from cache under load, enough to cross the default and
turn a `redundant_sql` finding into `n_plus_one_sql` with a remediation
hint (`leftJoin`, `with()`) that does not apply to a repeat. At `1.0`
the group keeps its `redundant_sql` verdict.

The same threshold feeds the HTTP heuristic, which decides whether a
repeated outbound call with few distinct parameters reads as
`n_plus_one_http` or `redundant_http`, so raising it moves both
verdicts. What still reports a real N+1 whatever the variance depends
on the mode and the path:

- SQL under `"auto"`: the ORM scope marker alone, so raising the
  threshold changes nothing on an ORM-instrumented group.
- SQL under `"strict"`: the high-occurrence bar
  (`3 x n_plus_one_min_occurrences`).
- HTTP: the direct rule, since distinct path or query parameters
  classify the group before the heuristic runs.
- `"always"` ignores the variance entirely, `"never"` never consults it.

One value serves the whole configuration: a daemon in front of several
runtimes picks the noisiest one's threshold and accepts the loss on
moderate-count groups elsewhere.

The value is recorded in `detection_config` of every report. A report
written before the knob existed reads back as `0.5`, the value its run
hard-wired.

### `[green]`

> **See also.** The [Energy and SCI primer](METHODOLOGY.md#background-energy-and-sci-primer) in the methodology doc defines SCI v1.0 (E + I + M terms), RAPL, Scaphandre, SPECpower, Boavizta and the Electricity Maps API used by the config sections below. Read it once if any term feels unfamiliar.

GreenOps scoring configuration aligned with [SCI v1.0](https://github.com/Green-Software-Foundation/sci) (operational + embodied terms, confidence intervals, multi-region).

| Field                              | Type    | Default  | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
|------------------------------------|---------|----------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`                          | boolean | `true`   | Enable GreenOps scoring (IIS, waste ratio, top offenders, CO₂)                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `default_region`                   | string  | *(none)* | Fallback cloud region used when neither the span's `cloud.region` attribute nor the `service_regions` mapping resolves a region. Examples: `"eu-west-3"`, `"us-east-1"`, `"FR"`                                                                                                                                                                                                                                                                                                        |
| `embodied_carbon_per_request_gco2` | float   | `0.001`  | SCI v1.0 `M` term: hardware manufacturing emissions amortized per request (per trace), in gCO₂eq. Region-independent. A zero is deprecated since 0.9.25 and falls back to this default with a warning: no hardware has zero embodied carbon, and a zeroed coefficient erased the M term from the disclosure. The applied value is published in `calibration_inputs`                                                                                                                    |
| `use_hourly_profiles`              | boolean | `true`   | When `true`, the scoring stage uses time-of-day-specific grid intensities for the 30+ regions with embedded hourly profiles. Regions with monthly x hourly profiles (FR, DE, GB, US-East) also account for seasonal variation. Reports are tagged `model = "io_proxy_v3"` (monthly x hourly) or `"io_proxy_v2"` (flat-year hourly). Set to `false` to pin reports to the flat-annual model                                                                                             |
| `hourly_profiles_file`             | string  | *(none)* | Path to a JSON file with user-supplied hourly profiles. Can be absolute or relative to the config file. Profiles in this file take precedence over embedded profiles for the same region key. See "User-supplied profiles" below                                                                                                                                                                                                                                                       |
| `per_operation_coefficients`       | boolean | `true`   | When `true`, the proxy model weights energy per I/O op by operation type: SQL SELECT (0.5x), INSERT/UPDATE (1.5x), DELETE (1.2x) and HTTP payload size tiers (small <10 KB: 0.8x, medium 10 KB-1 MB: 1.2x, large >1 MB: 2.0x). Does not apply when Scaphandre or cloud SPECpower measured energy is available. Set to `false` to use the flat `ENERGY_PER_IO_OP_KWH` for all operations                                                                                                |
| `include_network_transport`        | boolean | ignored  | Deprecated and ignored since 0.9.25: the transport term is always computed, always displayed and always disclosed, a display toggle on a published figure had no remaining justification. The key still parses, with a warning. The term needs `response_size_bytes` on HTTP spans (OTel `http.response.body.size` attribute) and the callee region mapped via `[green.service_regions]`. Same-region calls are excluded. Transport CO₂ appears as `transport_gco2` in the JSON report |
| `network_energy_per_byte_kwh`      | float   | ignored  | Deprecated and ignored since 0.9.25. The coefficient is fixed at 0.04 kWh/GB so every disclosure scales transport identically, and the disclosure publishes the sourced 0.001-0.059 kWh/GB bracket beside it (see LIMITATIONS.md, network transport). The key still parses, with a warning                                                                                                                                                                                             |

#### `[green.service_regions]`

Per-service region overrides used when OTel `cloud.region` is absent from spans (e.g. Jaeger / Zipkin ingestion). Maps service name → region key.

```toml
[green]
default_region = "eu-west-3"
embodied_carbon_per_request_gco2 = 0.001

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc"  = "ap-southeast-1"
```

#### Region resolution chain

For each span, the carbon scoring stage resolves the effective region in this order (first match wins):

1. **`event.cloud_region`**: from the OTel `cloud.region` resource attribute (or span attribute as fallback). Most authoritative.
2. **`[green.service_regions][event.service]`**: per-service config override.
3. **`[green] default_region`**: global fallback.

I/O ops with no resolvable region land in a synthetic `"unknown"` bucket (zero operational CO₂; the row appears in `regions[]` for visibility). Embodied carbon is still emitted because hardware manufacturing emissions are region-independent. The region cardinality is capped at 256 distinct buckets; excess values fold into the `unknown` bucket to prevent memory exhaustion from misconfigured ingestion.

#### Output shape

When green scoring is enabled and at least one event is analyzed, the JSON report's `green_summary` includes:

- **`co2`**: structured `{ total, avoidable, operational_gco2, embodied_gco2 }` object. Both `total` and `avoidable` are `{ low, mid, high, model, methodology }` with **2× multiplicative uncertainty** (`low = mid/2`, `high = mid×2`). The `methodology` tag distinguishes `total` (`"sci_v1_numerator+transport"`: `(E × I) + M + T` summed over traces, including when `T` is zero) from `avoidable` (`"sci_v1_operational_ratio"`: region-blind global ratio, excludes embodied and transport). Legacy reports can carry `"sci_v1_numerator"`. `model` values, most precise wins: `"electricity_maps_api"` > `"scaphandre_rapl"` > `"kepler_ebpf"` > `"redfish_bmc"` > `"cloud_specpower"` > `"io_proxy_v3"` > `"io_proxy_v2"` > `"io_proxy_v1"`. When calibration factors are active on proxy models, `+cal` is appended (e.g. `"io_proxy_v2+cal"`). The `+cal` suffix never applies to a measured tag.
- **`regions[]`**: per-region breakdown with `{ region, grid_intensity_gco2_kwh, pue, io_ops, co2_gco2, intensity_source }`, **sorted by `co2_gco2` descending** (highest-impact regions first) with alphabetical tiebreak. `intensity_source` is `"annual"`, `"hourly"`, `"monthly_hourly"` or `"real_time"` (Electricity Maps API) depending on which carbon intensity source was used for the region.

Carbon intensity data is embedded in the binary (no network egress). See `docs/design/05-GREENOPS-AND-CARBON.md` for the complete formula and methodology and [docs/LIMITATIONS.md](LIMITATIONS.md#carbon-estimates-accuracy) for the directional / non-regulatory disclaimer.

#### User-supplied hourly profiles

Set `[green] hourly_profiles_file` to a JSON file to provide your own hourly profiles. This is useful for datacenter operators with their own power purchase agreements (PPAs) or for overriding the embedded data with local measurements.

```json
{
  "profiles": {
    "my-datacenter": {
      "type": "flat_year",
      "hours": [45.0, 44.0, 43.0, "... 24 values total ..."]
    },
    "eu-west-3": {
      "type": "monthly",
      "months": [
        [50.0, 49.0, "... 24 values for January ..."],
        ["... 11 more months ..."]
      ]
    }
  }
}
```

User-supplied profiles take precedence over embedded profiles for the same region key. Validation at config load: each `flat_year` must have exactly 24 values, each `monthly` must have exactly 12 arrays of 24 values. All values must be finite and non-negative. If the region key exists in the embedded carbon table, a warning is logged when the profile mean deviates more than 5% from the annual value, but the profile is still accepted.

#### Hourly profile region aliases

Country-code aliases and cloud-provider synonyms are resolved to the same hourly profile. For example, `"fr"`, `"francecentral"` and `"europe-west9"` all map to the `eu-west-3` (France) profile. Notable mappings:

- `"us"`, `"eastus"` -> `us-east-1` (US-East, the most common US deployment region)
- `"westeurope"`, `"nl"`, `"nl-ams"` -> `eu-west-4` (Netherlands)
- `"northeurope"`, `"ie"` -> `eu-west-1` (Ireland)
- `"uksouth"`, `"gb"`, `"uk"`, `"uk1"` -> `eu-west-2` (UK)
- `"westus2"` -> `us-west-2` (Oregon)
- `"gra11"`, `"gra"`, `"sbg"`, `"fr-par"`, `"outscale-eu-west-2"` -> `eu-west-3` (France)
- `"waw1"`, `"pl-waw"` -> `europe-central2` (Poland)
- `"bhs5"`, `"bhs"` -> `ca-central-1` (Quebec)

**OVHcloud, Scaleway and OUTSCALE keys.** OVHcloud names one datacenter three ways depending on which API you ask (`GRA11` for the OpenStack Public Cloud region, `GRA` for the zone code, `gra` for the S3 location string), and all three are keyed. OUTSCALE keys carry an `outscale-` prefix because OUTSCALE reuses AWS region identifiers for different places: its `eu-west-2` is Paris where the AWS one is London. An OUTSCALE deployment declares `default_region = "outscale-eu-west-2"` rather than the bare identifier, otherwise it scores against the British grid.

The full alias table is in `score/carbon_profiles.rs`. If your region key is not aliased, the flat annual value from the primary carbon table is used.

**Every energy and grid-intensity backend is daemon-only.** `[green.alumet]`, `[green.scaphandre]`, `[green.kepler]`, `[green.redfish]`, `[green.cloud]`, `[green.broker_static]` and `[green.electricity_maps]` are scraped by the `watch` daemon and by nothing else. A batch `analyze` or `report` run starts no scraper, so it scores with the I/O proxy estimate over embedded intensity data whatever these sections say, and it emits a warning naming the sections it had to ignore. Attributing power measured now to traces recorded earlier would produce a wrong figure rather than a missing one, which is why batch does not scrape.

#### `[green.scaphandre]` (optional, opt-in)

Opt-in integration with [Scaphandre](https://github.com/hubblo-org/scaphandre) for per-process energy measurement on Linux hosts with Intel RAPL support. When configured, the `watch` daemon spawns a background task that scrapes the Scaphandre Prometheus endpoint every `scrape_interval_secs` and uses the measured power readings to replace the fixed `ENERGY_PER_IO_OP_KWH` constant for each mapped service.

**Prefer `[green.alumet]` for new deployments.** Both integrations read the same RAPL counters, but Alumet's sampling is measurably less error-prone, as characterized by its own authors in [Dissecting the software-based measurement of CPU energy consumption](https://hal.science/hal-04420527v2/document) (Raffin et al.), and it attributes per cgroup rather than per process. Scaphandre support is kept for existing deployments, and `alumet_rapl` outranks `scaphandre_rapl` whenever both feed the same service. See [docs/LIMITATIONS.md](LIMITATIONS.md#scaphandre-precision-bounds).

| Field                  | Type    | Default  | Description                                                                                                                                                               |
|------------------------|---------|----------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | string  | *(none)* | Full URL of the Scaphandre Prometheus `/metrics` endpoint. Must start with `http://` or `https://` (TLS supported via hyper-rustls). Required when the section is present |
| `scrape_interval_secs` | integer | `5`      | How often to scrape, in seconds. Valid range: 1-3600                                                                                                                      |
| `process_map`          | table   | `{}`     | Maps perf-sentinel service names (from span `service.name`) to a per-service `ProcessMatcher` (see below)                                                                 |

Each `process_map` entry is a table with two fields: `exe_contains` (required, substring matched against the Scaphandre `exe` label) and `cmdline_contains` (optional, substring matched against the `cmdline` label). The matcher requires both substrings to be present when `cmdline_contains` is set. Exactly one Scaphandre process must match per entry, otherwise the scoring stage skips that service for the tick and emits a `warn` log naming the ambiguity.

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5

[green.scaphandre.process_map."order-svc"]
exe_contains = "bin/java"
cmdline_contains = "order-svc.jar"

[green.scaphandre.process_map."chat-svc"]
exe_contains = "bin/java"
cmdline_contains = "chat-svc.jar"

[green.scaphandre.process_map."native-svc"]
exe_contains = "/opt/native-svc/bin/native-svc"
```

**Why both `exe_contains` and `cmdline_contains`.** Scaphandre emits `exe` as an absolute path of the runtime (`/usr/lib/jvm/.../bin/java`, `/usr/share/dotnet/dotnet`). Several co-located services sharing a runtime (multiple JVMs, multiple .NET assemblies) collide on `exe`, and only `cmdline` discriminates them. Real Scaphandre also concatenates argv without separators: `java -jar /tmp/order-svc.jar` is emitted as `cmdline="java-jar/tmp/order-svc.jar"`. Configure `cmdline_contains` with a substring that appears in this concatenated form (e.g. the jar/dll filename), NOT with a POSIX command line containing spaces.

**Ignored in `analyze` batch mode.** Only the `watch` daemon spawns the scraper. The `analyze` command always uses the proxy model regardless of this section.

**Fallback behaviour.** When the endpoint is unreachable, a service is not present in `process_map` or a service had zero ops in the current scrape window, the scoring stage falls back to the proxy model for those spans. The first failure logs at `warn` level; subsequent failures log at `debug` to avoid spam. The `perf_sentinel_scaphandre_last_scrape_age_seconds` Prometheus gauge lets operators detect a hung scraper.

**Precision bounds (important).** Scaphandre improves the **per-service** energy coefficient but does NOT give per-finding attribution. RAPL is process-level, not span-level: two findings in the same process during the same scrape window share the same coefficient. See [docs/LIMITATIONS.md](LIMITATIONS.md#scaphandre-precision-bounds) for the full discussion.

#### `[green.kepler]` (optional, opt-in)

Opt-in integration with [Kepler](https://github.com/sustainable-computing-io/kepler) (CNCF sandbox) for per-container or per-process energy measurement via eBPF. Unlike Scaphandre, Kepler works on ARM64 (Graviton, Ampere, Apple Silicon, Cobalt 100) with degraded precision but a real signal. When configured, the `watch` daemon scrapes Kepler's Prometheus `/metrics` endpoint, computes a per-service joules delta vs the previous scrape, and publishes a measured per-op coefficient tagged `kepler_ebpf`.

| Field                  | Type   | Default       | Description                                                                                                                                                                         |
|------------------------|--------|---------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | string | *(none)*      | Full URL of the Kepler Prometheus `/metrics` endpoint. Required when the section is present                                                                                         |
| `scrape_interval_secs` | int    | `5`           | How often to scrape, in seconds. Valid range: 1-3600                                                                                                                                |
| `metric_kind`          | string | `"container"` | Which Kepler v2 counter to read: `"container"` (`kepler_container_cpu_joules_total`, keyed by `container_name`) or `"process"` (`kepler_process_cpu_joules_total`, keyed by `comm`) |
| `service_mappings`     | table  | `{}`          | Maps perf-sentinel service names to the Kepler label value identifying the same workload (container name for `container`, process command name for `process`)                       |
| `auth_header`          | string | *(none)*      | Optional `"Name: Value"` header. Prefer `PERF_SENTINEL_KEPLER_AUTH_HEADER` env var                                                                                                  |

```toml
[green.kepler]
endpoint = "http://kepler.kube-system.svc.cluster.local:9102/metrics"
scrape_interval_secs = 5
metric_kind = "container"

[green.kepler.service_mappings]
"order-svc" = "order-svc-deployment"
"chat-svc" = "chat"
```

**Ignored in `analyze` batch mode.** Like Scaphandre, only `watch` spawns the scraper.

**Counters sharing a label value are summed.** One container name repeated across pods (or one `comm` shared by several processes) yields several cumulative series under one mapping value. Their counters are summed before the per-window delta is computed, so the coefficient covers all of them together.

**Precedence vs Scaphandre.** Scaphandre RAPL outranks Kepler eBPF on x86_64 with RAPL access. The Kepler integration shines on ARM64 where Scaphandre is unavailable. See [docs/LIMITATIONS.md](LIMITATIONS.md#kepler-precision-bounds) for the ARM eBPF accuracy caveats (Kepler upstream issue #1556).

**Production deployment shape.** Kepler typically runs as a Kubernetes `DaemonSet`, one pod per node. The current scraper performs a direct GET and the response must expose the Kepler series themselves. A Prometheus server's own `/metrics` endpoint exposes Prometheus internals, not the series it scraped. For a multi-node cluster, run one perf-sentinel per node or provide a federation/proxy endpoint that directly exposes the aggregated Kepler series in Prometheus exposition format. Native PromQL query mode is reserved for a follow-up release.

#### `[green.alumet]` (optional, opt-in)

Opt-in integration with [Alumet](https://github.com/alumet-dev/alumet) (INRIA/LIG, EUPL-1.2) for measured energy. Alumet is a modular measurement framework: a source plugin (`rapl`, `nvidia-nvml`, ...) produces readings, optional transform plugins attribute them to workloads, and an output plugin exposes them. perf-sentinel scrapes the `prometheus-exporter` output. When configured, the `watch` daemon publishes a measured per-op coefficient tagged `alumet_rapl`, which **outranks every other measured source**, including Scaphandre.

| Field                  | Type   | Default  | Description                                                                                                                                   |
|------------------------|--------|----------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | string | *(none)* | Full URL of the Alumet `prometheus-exporter` `/metrics` endpoint (upstream default port 9091). Required when the section is present           |
| `scrape_interval_secs` | int    | `5`      | How often to scrape, in seconds. Valid range: 1-3600                                                                                          |
| `metric_name`          | string | *(none)* | Prometheus metric name **exactly as it appears on the wire**, including the exporter's `prefix`/`suffix`. Required, no default (see below)    |
| `label_key`            | string | *(none)* | Prometheus label carrying the workload identity. Required, no default. `name` for the `k8s` source (pod name), `domain` for a raw RAPL series |
| `energy_interval_secs` | float  | `1.0`    | Wall-clock seconds the scraped joules value covers. **Must match the `poll_interval` of the Alumet source feeding the metric** (see below)    |
| `service_mappings`     | table  | `{}`     | Maps perf-sentinel service names to the Alumet label value identifying the same workload                                                      |
| `auth_header`          | string | *(none)* | Optional `"Name: Value"` header. Prefer `PERF_SENTINEL_ALUMET_AUTH_HEADER` env var                                                            |

**Why `metric_name` and `label_key` have no default.** Alumet's exporter prepends `prefix` and appends `suffix` (default `_alumet`) to every metric name, and the per-service series is produced by an `energy-attribution` formula whose name *you* choose. No default could be right for every deployment, and a wrong guess would scrape nothing. Read the names off your own endpoint:

```bash
curl -s http://localhost:9091/metrics | grep -i energy
```

**Several rows per service are summed.** Alumet's `label_key` is routinely shared: one pod carries a row per RAPL domain (`package` + `dram`), and `label_key = "domain"` on a dual-socket host carries one `domain="package"` row per socket. Every row sharing a label value is summed, which is the physically correct read since energy is additive (NaN and negative rows are skipped). Two consequences to configure for. Pick `label_key` so that the rows sharing a value are the ones you want added together. And make sure those rows do not overlap: RAPL domains nest (`psys` contains `package`, `package` contains `pp0`/`pp1`), so a formula that emits both a parent and its child domain for one label double-counts the shared share. `package` plus `dram` sums correctly, `psys` plus `package` does not.

**Why `energy_interval_secs` exists.** This is the one field to get right. Alumet's exporter publishes every measurement as a Prometheus **gauge holding the last flushed value**, and `rapl_consumed_energy` is a `CounterDiff`: the joules burned during one source `poll_interval`, not a cumulative counter and not a power reading. perf-sentinel divides by this interval to recover watts. The interval appears nowhere on the wire, so it must be declared here, and it must match the Alumet side. **A mismatch rescales energy and carbon linearly and silently**: declaring `1.0` while Alumet polls at `5s` overstates energy 5x, with no warning. See [docs/LIMITATIONS.md](LIMITATIONS.md#alumet-precision-bounds). The daemon echoes the value it is using in the `Alumet scraper started` log line.

**Matching Alumet config.** Per-service attribution needs three Alumet plugins working together, `rapl` alone only measures the whole machine and `procfs` only identifies processes by PID:

```toml
# alumet-config.toml
[plugins.rapl]
poll_interval = "1s"          # <- perf-sentinel's energy_interval_secs must equal this

[plugins.k8s]
# pod discovery, provides the `name` / `namespace` attributes

[plugins.energy-attribution.formulas.attributed_energy_cpu]
expr = "cpu_energy * cpu_usage / 100.0"
ref = "cpu_energy"

[plugins.energy-attribution.formulas.attributed_energy_cpu.per_resource]
cpu_energy = { metric = "rapl_consumed_energy", resource_kind = "local_machine", domain = "package_total" }

[plugins.energy-attribution.formulas.attributed_energy_cpu.per_consumer]
cpu_usage = { metric = "cpu_percent", kind = "total" }

[plugins.prometheus-exporter]
port = 9091
suffix = "_alumet"            # <- why the metric name below ends in _alumet
```

The matching perf-sentinel side:

```toml
[green.alumet]
endpoint = "http://localhost:9091/metrics"
scrape_interval_secs = 5
metric_name = "attributed_energy_cpu_alumet"
label_key = "name"
energy_interval_secs = 1.0

[green.alumet.service_mappings]
"order-svc" = "order-svc-pod"
"chat-svc" = "chat-svc-pod"
```

**Ignored in `analyze` batch mode.** Like every measured-energy backend, only `watch` spawns the scraper.

**Precedence.** `alumet_rapl` leads the measured chain, ahead of `scaphandre_rapl`. Both read RAPL, but Alumet's sampling is measurably less error-prone and it attributes per cgroup rather than per process. Running both on the same service is supported, Alumet wins.

**Upstream packaging gotchas.** The upstream `.deb` ships `/etc/alumet/alumet-config.toml` (with sections for csv, procfs, perf, ...) and its `alumet-agent` wrapper points `ALUMET_CONFIG` at it unless the variable is already set. Enabling prometheus-exporter with `--plugins` works even though that file has no `prometheus-exporter` section. The agent fills the absent section from the plugin's defaults (`prefix ""`, `suffix "_alumet"`, port 9091). For a controlled config instead of the shipped one, point `ALUMET_CONFIG` at a fresh path so the agent regenerates defaults for your plugin set, or run `config regen`. In containers, the packaged binary carries file capabilities (`cap_perfmon`, `cap_sys_nice`, `cap_sys_ptrace`), so a plain `docker run` fails with EPERM unless those are granted with `--cap-add`.

**The scraper alone does not put `alumet_rapl` in the report.** Setting `[green.alumet]` starts the scraper (visible on `/api/energy`), but the measured coefficient only reaches `green_summary` when green scoring resolves a region for the spans (`[green] default_region`, `[green.service_regions]`, or a `cloud.region` span attribute). Without one, `per_service_energy_model` keeps showing the proxy tag, which is easy to misread as a broken Alumet integration.

**Alumet is pre-1.0** (v0.9.5 at time of writing). Metric names and plugin config may change between releases. If a scrape stops matching after an Alumet upgrade, the daemon warns with `no samples matched the configured metric` after three consecutive ticks.

##### `[green.alumet.database]` (optional)

Declares one database workload measured by Alumet. A database emits no spans, so it can never appear in `service_mappings` (zero ops, the per-op coefficient path skips it). Instead, its energy over each scoring window is multiplied by the SQL-only waste ratio (`avoidable_sql_io_ops / total_sql_io_ops`) and reported as `green_summary.database_waste`, a standalone figure excluded from `energy_kwh`, `co2` and the public disclosure. See `docs/METHODOLOGY.md` for the formula and [docs/LIMITATIONS.md](LIMITATIONS.md#alumet-precision-bounds) for why it is a lower bound.

```toml
[green.alumet.database]
label_value = "postgres-pod"   # value carried by label_key for the DB cgroup, verbatim
region = "eu-west-3"           # optional, enables the gCO2 conversion (declared, not inferred)
```

`label_value` is required, matched exactly like a `service_mappings` value. `region` is optional and uses the same region ids as `[green.service_regions]`: without it the waste is reported in kWh only. One database per config, declare the cgroup that serves your SQL traffic.

##### `[green.alumet.broker]` (optional)

The messaging twin of the section above. Declares one message broker measured by Alumet: a broker emits no spans of its own either, so its window energy is multiplied by the messaging-only waste ratio (`avoidable_messaging_io_ops / total_messaging_io_ops`) and reported as `green_summary.messaging_waste`.

```toml
[green.alumet.broker]
label_value = "kafka-pod"      # value carried by label_key for the broker cgroup, verbatim
region = "eu-west-3"           # optional, enables the gCO2 conversion
```

The same cgroup cannot feed two figures: a `label_value` that also appears in `service_mappings`, or that matches the database declaration, is rejected at config load. Requires an agent on the broker host, so it does not apply to a managed broker. For those, see `[green.broker_static]`.

#### `[green.broker_static]` (optional, opt-in)

Declares a **provisioned** broker cluster, with no agent and no metric. This is the only path that works for a managed broker (Confluent Cloud, MSK, SQS, managed Pulsar), where there is no host to instrument.

```toml
[green.broker_static]
nodes = 3                      # provisioned broker nodes, required
instance_type = "m5.2xlarge"   # looked up in the embedded SPECpower table, required
provider = "aws"               # optional: aws, gcp, azure, scaleway or generic (default)
region = "eu-west-3"           # optional, enables the gCO2 conversion
```

The energy is `nodes × max_watts × window duration`, following `E(n) = n × P_max`: provisioned nodes times their power ceiling. Three properties to accept before relying on it:

- **It bounds compute, not wall power.** `max_watts` is the power at 100 % CPU, drawn from a SPECpower table that covers CPU and baseboard and excludes storage, network and PSU overhead. Those dominate on a broker, so this is a ceiling on the declared vCPUs and not on what the cluster actually pulls from the wall. A storage-bound Kafka node can draw more than this figure reports.
- **It counts provisioned infrastructure, not consumed.** A three-node cluster is immobilized whether it runs at 10 % or 60 %. In the other direction, a stretch with no traffic bills at most one hour, so a mostly-idle cluster is under-counted.
- **It does not react to application changes.** Batching your publishes will not move this number, because nothing about it depends on traffic. If you want a figure that responds to remediation, you need the measured path.

An unknown `instance_type` warns and falls back to a provider default rather than failing: the figure stays coarser, and the warning says so. An unrecognised `provider` is rejected outright, because it would silently resolve to the generic on-prem watts. Daemon only, like every measured path, since a window duration is needed. When both this section and `[green.alumet.broker]` are configured, **the measurement wins**. Precedence follows the broker's own Alumet series, not the scrape endpoint: a scrape that answers without carrying the declared `label_value` measures nothing, so the declaration takes over rather than being suppressed by a working endpoint. Energy the series already banked is delivered first, and the delta that lands when it recovers is dropped once, since it reaches back over wall clock the declaration billed.

#### `[green.redfish]` (optional, opt-in)

Opt-in integration with the [Redfish](https://www.dmtf.org/standards/redfish) BMC standard for bare-metal wall-plug power readings. Unlike Scaphandre and Kepler (which measure CPU + DRAM only), Redfish reads the actual power supply output via the BMC, so periphery (NIC, drives, fans, PSU overhead) is included. Bare-metal only, no cloud VMs.

| Field                       | Type    | Default                                  | Description                                                                                                                                                                       |
|-----------------------------|---------|------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoints`                 | table   | *(empty)*                                | Map of `chassis_id` → endpoint table with `url` + `schema`. Required to activate the scraper                                                                                      |
| `scrape_interval_secs`      | int     | `60`                                     | How often to scrape each chassis. Valid range: 15-3600 (BMC rate-limit defense, several BMCs throttle below 30s)                                                                  |
| `service_mappings`          | table   | `{}`                                     | Maps perf-sentinel service names to the chassis hosting them. Every service mapped to the same chassis receives the same chassis-level coefficient                                |
| `ca_bundle_path`            | string  | *(none)*                                 | **Reserved for a follow-up.** Setting this field today causes the scraper to refuse to start with a clear error. Self-signed BMC certs are not supported in this release          |
| `auth_header`               | string  | *(none)*                                 | Curl-style Basic auth header. Prefer `PERF_SENTINEL_REDFISH_AUTH_HEADER` env var. Session-token auth (POST `/SessionService/Sessions`) is not yet supported                       |

Each endpoint table has two fields: `url` (string, full Redfish URL including path) and `schema` (string, either `"legacy_power"` or `"environment_metrics"`). The schema selects the canonical JSON pointer the parser uses, no operator-typed pointer involved:

| `schema`              | Path served by BMC                            | JSON pointer parser reads            |
|-----------------------|-----------------------------------------------|--------------------------------------|
| `legacy_power`        | `/redfish/v1/Chassis/{id}/Power`              | `/PowerControl/0/PowerConsumedWatts` |
| `environment_metrics` | `/redfish/v1/Chassis/{id}/EnvironmentMetrics` | `/PowerWatts/Reading`                |

```toml
[green.redfish]
scrape_interval_secs = 60

[green.redfish.endpoints."chassis-legacy-1"]
url = "https://bmc-rack-01.dc.example/redfish/v1/Chassis/1/Power"
schema = "legacy_power"

[green.redfish.endpoints."chassis-modern-1"]
url = "https://bmc-rack-02.dc.example/redfish/v1/Chassis/1/EnvironmentMetrics"
schema = "environment_metrics"

[green.redfish.service_mappings]
"order-svc"  = "chassis-legacy-1"
"chat-svc"   = "chassis-legacy-1"
"ledger-svc" = "chassis-modern-1"
```

**Which schema to choose.** `/Power` (legacy_power) was deprecated by DMTF Release 2020.4 but is still mandatory on BMC firmware as of 2026, every shipping vendor exposes it. `/EnvironmentMetrics` (environment_metrics) is the modern replacement that carries `PowerWatts.Reading` directly, present alongside `/Power` during the transition. Pick `legacy_power` unless your BMC documentation explicitly recommends `EnvironmentMetrics`. A mixed fleet is declared by giving each chassis the schema its firmware serves.

**Ignored in `analyze` batch mode.** Like Scaphandre and Kepler, only `watch` integrates Redfish.

**Node-level coefficient.** Every service mapped to the same chassis receives the **same** coefficient. Two services on one chassis will never get distinct measured per-op values via Redfish. See [docs/LIMITATIONS.md](LIMITATIONS.md#redfish-bmc-precision-bounds) for the full discussion of this trade-off and the vendor-specific JSON response variance.

#### `[green.cloud]` (optional, opt-in)

Cloud-native energy estimation via CPU utilization + SPECpower interpolation. When configured, the `watch` daemon scrapes CPU% from a Prometheus/VictoriaMetrics endpoint and uses an embedded lookup table (idle/max watts per cloud instance type) to estimate per-service energy consumption. Supports AWS, GCP, Azure and on-premise hardware with manual watts override.

| Field                   | Type    | Default  | Description                                                                                                                                                                  |
|-------------------------|---------|----------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `prometheus_endpoint`   | string  | *(none)* | Prometheus HTTP API base URL (e.g. `http://prometheus:9090` or `https://prometheus:9090`). TLS supported via hyper-rustls. Required.                                         |
| `scrape_interval_secs`  | integer | `15`     | Polling interval in seconds (range: 1-3600).                                                                                                                                 |
| `default_provider`      | string  | *(none)* | Default cloud provider: `"aws"`, `"gcp"`, `"azure"`, `"scaleway"`. Scaleway instance types are derived from its Product Catalog, see [INSTANCE-TYPES.md](INSTANCE-TYPES.md). |
| `default_instance_type` | string  | *(none)* | Fallback instance type for unmapped services.                                                                                                                                |
| `cpu_metric`            | string  | *(none)* | Default PromQL metric/query for CPU utilization.                                                                                                                             |

Per-service entries in `[green.cloud.services]` support two forms:

**Cloud instance (table lookup):**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"

[green.cloud.services]
"account-svc" = { provider = "aws", instance_type = "m7i.4xlarge" }       # Sapphire Rapids
"api-asia" = { provider = "gcp", instance_type = "c4d-standard-8" }       # AMD Turin
"analytics" = { provider = "azure", instance_type = "Standard_D8s_v6" }   # Emerald Rapids
"ml-bench" = { provider = "aws", instance_type = "m8g.4xlarge" }          # Graviton 4
```

The full list of covered types, with their idle and max wattage, is [`INSTANCE-TYPES.md`](./INSTANCE-TYPES.md). Modern instance families covered include AWS m7i/c7i/r7i, m7a/c7a, m6a/c6a, m7g/c7g, m8g/c8g; GCP c3, c3d, c4, c4d, n2d, t2a; Azure Standard_Dv6, Standard_Dadsv6, Standard_Dpsv6 (Cobalt 100), Standard_Ev6. One CPU-named bare-metal entry covers Sierra Forest (`xeon-6780e`, system-level watts assuming full chip ownership).

**Manual watts (on-premise or custom hardware):**

```toml
[green.cloud.services]
"my-service" = { idle_watts = 45, max_watts = 120 }
```

**Ignored in `analyze` batch mode.** Only the `watch` daemon spawns the Prometheus scraper.

**Fallback behaviour.** If the Prometheus endpoint is unreachable, the daemon falls back to the proxy model for all cloud-configured services. Unknown instance types fall back to a provider-level default.

**Precision bounds.** The SPECpower interpolation model has approximately +/-30% accuracy, better than the proxy model but less precise than Scaphandre RAPL. See `docs/LIMITATIONS.md` for details.

#### `[green.electricity_maps]` (optional, opt-in)

Real-time carbon intensity from the Electricity Maps API. Daemon-only.

| Field                  | Type    | Default                              | Description                                                                                                                                                                                                 |
|------------------------|---------|--------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `api_key`              | string  | none                                 | API auth token. Prefer `PERF_SENTINEL_EMAPS_TOKEN` env var for security                                                                                                                                     |
| `endpoint`             | string  | `https://api.electricitymaps.com/v4` | API base URL (`http://` or `https://`). v3 still works but emits a deprecation warning at startup                                                                                                           |
| `poll_interval_secs`   | integer | `300`                                | Poll interval in seconds (range: 60-86400). Free tier: use 3600+                                                                                                                                            |
| `emission_factor_type` | string  | `lifecycle`                          | Emission factor model. `lifecycle` (default) includes upstream emissions (manufacturing, transport). `direct` includes only combustion. Some Scope 2 frameworks prefer `direct` for stricter accountability |
| `temporal_granularity` | string  | `hourly`                             | API response aggregation. `hourly` (default), `5_minutes`, or `15_minutes`. Sub-hour values require a paid plan that exposes them, otherwise the API silently coarsens to hourly                            |

The `region_map` sub-table maps cloud regions to Electricity Maps zone codes:

```toml
[green.electricity_maps]
# Use PERF_SENTINEL_EMAPS_TOKEN env var instead of api_key in config
poll_interval_secs = 300

[green.electricity_maps.region_map]
"eu-west-3" = "FR"
"us-east-1" = "US-NY"
"ap-northeast-1" = "JP-TK"
```

**Staleness:** if the last successful poll is older than 3x `poll_interval_secs`, the scraper falls back to embedded hourly profiles.


**Rate limits:** the Electricity Maps free tier allows approximately 30 requests per month per zone. For free tier users, set `poll_interval_secs = 3600` or higher. The default of 300s is intended for paid plans.

**API version:** the default endpoint targets v4 since perf-sentinel 0.5.11. v3 remains accepted (the response schema is identical on `carbon-intensity/latest`), but a deprecation warning is logged once at daemon startup. To silence the warning, set `endpoint = "https://api.electricitymaps.com/v4"` explicitly. To keep v3 deliberately (for example to A/B-validate against v4), leave `endpoint = "https://api.electricitymaps.com/v3"` and acknowledge the warning.

**Unknown values for `emission_factor_type` and `temporal_granularity`:** these two knobs use a fail-graceful parser. A typo or unsupported value (e.g. `temporal_granularity = "5min"` instead of `"5_minutes"`) does not reject the config at load time. The value is sanitized, a `tracing::warn!` is emitted, and the daemon falls back to the default. Watch the daemon logs at startup if you suspect a typo, the warn line will name the offending field and value.

**Visibility in reports (since perf-sentinel 0.5.12):** the active scoring configuration (API version, emission factor type, temporal granularity) is surfaced in three places so Scope 2 reporters can audit which carbon model produced the numbers without reading the operator's TOML.

- The JSON report always carries `green_summary.scoring_config` while GreenOps scoring is enabled: it records the applied coefficients and an `electricity_maps` flag. The API-specific fields are meaningful only when that flag is `true`.
- The HTML dashboard renders a chip bandeau above the green-regions table. Default values (`v4`, `lifecycle`, `hourly`) are neutral chips, opt-in values (`direct`, `5_minutes`, `15_minutes`) are accent chips, the legacy `v3` endpoint shows as a warning chip mirroring the deprecation warning. Native browser tooltips explain each value.
- The terminal `print_green_summary` output prepends a one-liner `Carbon scoring: Electricity Maps v4, lifecycle, hourly` before the per-region breakdown.

The bandeau and the terminal line are hidden when `[green.electricity_maps]` is not configured.

#### `[green] calibration_file` (optional)

Path to a calibration TOML file generated by `perf-sentinel calibrate`. When present, per-service calibration factors are loaded at config time and multiply the proxy model energy per op. Does not affect Scaphandre or cloud SPECpower measured energy.

```toml
[green]
calibration_file = ".perf-sentinel-calibration.toml"
```

**`perf-sentinel calibrate` input size limits.** Both inputs are capped to protect against unbounded memory use: the `--traces` file is capped at 1 GiB (the fixed batch cap since 0.8.7, same as `analyze`) and the `--measured-energy` CSV is capped at 64 MiB. Calibrate exits with a clear error if either file exceeds its limit. 64 MiB is generous for thousands of RAPL samples per minute, if you need more, file an issue describing the workload.

#### `perf-sentinel tempo` (no config section)

The `tempo` subcommand runs in **batch mode** (not daemon), fetches traces from a Grafana Tempo HTTP API and pipes them through the standard analysis pipeline. Its own settings are CLI flags only, there is no `[tempo]` section: `--endpoint` is required, `--max-traces` defaults to `100` and is bounded to 1..=10000 (the client's own read ceiling, not Tempo's), alongside `--trace-id`, `--service`, `--lookback`, `--from`/`--to`, `--sort` and `--auth-header`. Run `perf-sentinel tempo --help` for the current list. A `[tempo]` table written into the config file fails the load since 0.12.0, as any unknown top-level table does. The `--config` file still applies for everything else, thresholds and detection in particular, since the fetched traces go through the same pipeline.

### `[daemon]`

Streaming mode (`perf-sentinel watch`) settings.

| Field                     | Type    | Default                     | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
|---------------------------|---------|-----------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`          | string  | `"127.0.0.1"`               | IP address to bind for OTLP and metrics endpoints. Use `127.0.0.1` for local-only access. **Warning:** setting a non-loopback address exposes unauthenticated endpoints to the network, use a reverse proxy or network policy                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `listen_port_http`        | integer | `4318`                      | Port for OTLP HTTP receiver and Prometheus `/metrics` endpoint (range: 1-65535)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `listen_port_grpc`        | integer | `4317`                      | Port for OTLP gRPC receiver (range: 1-65535)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `json_socket`             | string  | `"/tmp/perf-sentinel.sock"` | Unix socket path for JSON event ingestion                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `max_active_traces`       | integer | `10000`                     | Maximum number of traces held in memory. When exceeded, the oldest trace is evicted (LRU). Range: 1 to 1,000,000                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `trace_ttl_ms`            | integer | `30000`                     | Time-to-live for traces in milliseconds. A trace goes stale once no span has arrived for this long, and the sweep that evicts and analyses it runs on a ticker at half this value, so the effective deadline is this plus up to one tick. A span arriving in that gap lands in the stale trace and refreshes it rather than opening a new one, which matters to anyone replaying a trace id. Range: 100 to 3,600,000                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `sampling_rate`           | float   | `1.0`                       | Fraction of traces to analyze (0.0 to 1.0). Set below 1.0 to reduce load in high-traffic environments. Whole traces are kept or dropped on a hash of the trace id, so the per-trace detectors stay correct on what remains and ratios such as the I/O waste ratio sample numerator and denominator alike, but the absolute counts (findings, occurrences, the `perf_sentinel_*` totals) then describe that fraction of the traffic, and a pattern present in a small share of the traffic can be sampled out entirely. Below 1.0 the daemon emits a `tuning` entry in `Report.warning_details` saying so, and `0.0`, which this range accepts, gets its own message since no trace is analyzed at all. Sampling done by a collector upstream has the same effect and cannot be detected, see [HELM-DEPLOYMENT.md](HELM-DEPLOYMENT.md#collector-sampling-and-what-reaches-the-daemon)                                                                                                                                                                                                                  |
| `max_events_per_trace`    | integer | `1000`                      | Per-trace cap applied independently to stored events (ring buffer), retained inbound endpoint contexts (endpoint plus optional parent link), and span-ancestry entries (intermediate parent link plus optional resolved endpoint). A trace can hold all three bounded collections; events and ancestry entries allocate progressively and use their respective rotation policies. Range: 1 to 100,000                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `max_payload_size`        | integer | `16777216`                  | Maximum size in bytes for a single JSON payload (default: 16 MiB, raised from 1 MiB in 0.5.13 because a daemon snapshot from `/api/export/report` already exceeds 1 MiB on a modest cluster). Range: 1,024 to 104,857,600 (100 MB). The default sits at the upper inclusive boundary of the comfort zone by design. Since 0.8.7 this caps daemon network payloads only: batch subcommands (`analyze`, `diff`, `report`, `explain`, `calibrate`, `pg-stat`, `bench`) read local input files under a fixed 1 GiB cap instead                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `environment`             | string  | `"staging"`                 | Deployment environment label. Accepted values: `"staging"` (default, medium confidence) or `"production"` (high confidence). Stamps every finding with the corresponding `confidence` field for downstream tooling (perf-lint planned). Case-insensitive; any other value is rejected at config load                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `tls_cert_path`           | string  | *(absent)*                  | Path to a PEM-encoded TLS certificate chain for the OTLP receivers. When set alongside `tls_key_path`, both gRPC and HTTP listeners use TLS. When absent, listeners use plain TCP. Each TLS listener caps concurrent in-flight handshakes at 128 (non-configurable) and drops peers that do not complete the handshake within 10 seconds                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `tls_key_path`            | string  | *(absent)*                  | Path to a PEM-encoded TLS private key. Must be set together with `tls_cert_path` (both or neither). On Unix, the daemon warns if the key file is readable by group or others                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `api_enabled`             | boolean | `true`                      | Enable the daemon query API endpoints (`/api/findings`, `/api/explain/{trace_id}`, `/api/correlations`, `/api/status`). Set to `false` to disable the API while keeping OTLP ingestion and `/metrics` active                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `max_retained_findings`   | integer | `10000`                     | Maximum number of recent findings retained in the daemon's ring buffer for the query API. Older findings are evicted when the limit is reached. Range: 0 to 10,000,000, where `0` disables the store entirely and reclaims its memory (recommended when `api_enabled = false`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `max_export_findings`     | integer | `1000`                      | Maximum number of findings carried by one `/api/export/report` snapshot. Separate from the `/api/findings` cap, which paginates a browsing API, where this one sizes a deliberate export: a store holding tens of thousands of findings ships a slice of its most recent, and the snapshot says so in `warning_details`. Raising it grows the response body, and the HTML rendered from it, by a few KB per finding. Past roughly 2000 the snapshot outgrows the 8 MiB body limit `query inspect` and `query monitor` fetch it with, and the daemon logs an advisory. Range: 0 to 100,000, where `0` exports the envelope alone (green figures, no findings). Since the exported `quality_gate` counts findings from that slice, at `0` its three finding-count rules pass whatever the daemon detected. The fourth, `io_waste_ratio_max`, reads `green_summary`, which no cap empties, so the verdict is not a blanket pass, it just stops reflecting the findings. That is enough to make `0` suit a liveness probe and not an alerting one. Overridable per run with `watch --max-export-findings` |
| `max_retained_traces`     | integer | `50`                        | Maximum number of traces whose masked spans are retained so `/api/export/report` carries a span tree the HTML dashboard can draw. Without it the correlation window drops a trace's spans seconds after it completes and an exported report has findings with nothing to show around them. Costs memory in proportion to `max_events_per_trace`, hence the much smaller cap than `max_retained_findings`. Range: 0 to 10,000, where `0` retains none. Ignored (treated as `0`) when `api_enabled = false` or `max_retained_findings = 0`, nothing could serve the trees                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `ingest_queue_capacity`   | integer | `1024`                      | Capacity of the ingestion channel: span-event batches buffered between the listeners and the event loop. Once full, ingestion applies backpressure to producers. Raise it to absorb burstier traffic at the cost of memory. Range: 1 to 1,048,576                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `analysis_queue_capacity` | integer | `1024`                      | Capacity of the analysis worker queue: evicted and expired batches awaiting detect+score. Once full, whole batches are shed and counted on `perf_sentinel_analysis_shed_batches_total`. Raise it to tolerate longer analysis bursts before shedding. Range: 1 to 1,048,576                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `per_service_labels`      | boolean | `true`                      | Whether `perf_sentinel_findings_total` and `perf_sentinel_slow_duration_seconds` carry a `service` label (since 0.18.0). Cardinality is capped per daemon run (128 services on findings, 64 on the histogram), services past a cap fold into `service="_other"` so the totals stay exact. `false` makes the label empty on every series, restoring the pre-0.18 shape. An empty label is no label: an honor-labels scrape (the chart's ServiceMonitor) drops it and a plain scrape overwrites it with the target's name, so with the knob off the shipped dashboard's `Service` filter still lists services (from the per-service I/O counters) but selecting one shows 0 findings and no slow-span latency, keep `All` or leave the knob on. Does not affect the per-service I/O counters (`service_io_ops_total`, `service_avoidable_io_ops_total`, `service_analyzed_io_ops_total`), per-service by construction                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `per_grouping_labels`     | boolean | `true`                      | Whether `perf_sentinel_findings_total`, `perf_sentinel_slow_duration_seconds`, `perf_sentinel_service_io_ops_total`, `perf_sentinel_service_avoidable_io_ops_total` and `perf_sentinel_service_analyzed_io_ops_total` carry a `grouping` label next to `service` (since 0.19.0), holding the first attribute present from `[detection] grouping_attributes` (empty when the span carried none). Cardinality is capped per daemon run independently of the service caps (16 on the analysis side, 8 on the histogram, 32 at ingest), values past a cap fold into `grouping="_other"` so `sum by (service)` still equals the 0.18.0 series. `false` makes the label empty on every series, restoring the 0.18.0 shape. An empty label is no label, and since nothing attaches a `grouping` target label the series simply has none, so the shipped dashboard's `Grouping` filter reaches it under `All` only. Unlike `per_service_labels`, this knob also governs the three per-service I/O counters. Not named `namespace` because Prometheus Operator attaches a `namespace` target label and the chart's `honorLabels: true` would let the daemon's win |
| `memory_high_water_pct`   | integer | `0`                         | Memory-pressure admission control, as a percentage of the cgroup v2 memory limit. When the working-set ratio (`memory.current` minus reclaimable `inactive_file` page cache, over `memory.max`) crosses this high-water mark, ingest is rejected with a retryable status (counted on `perf_sentinel_otlp_rejected_total{reason="memory_pressure"}`, state on the `perf_sentinel_ingest_memory_pressure` gauge) and resumes once usage falls 5 percentage points below the mark (hysteresis, so ingest does not flap around the boundary), bounding RSS independently of queue depth. `0` disables the guard (default). Linux/cgroup-v2 only, inert elsewhere; fails open if the cgroup becomes unreadable. Set 80-85 to leave headroom above a typical steady-state footprint. The guard polls at a fixed cadence, so size the mark such that the `limit - mark` margin exceeds the peak in-flight footprint (a sustained flood can outrun a thin margin, see [RUNBOOK](RUNBOOK.md#daemon-memory-pressure-or-oom)). Range: 0, or 6 to 100 (1-5 would put the hysteresis low bound at or below zero)   |

##### Comfort zones and startup warnings

Daemon limits accept any value inside their hard bounds (rejected at config load), but `perf-sentinel watch` emits a one-shot `WARN` log at startup when a value falls outside the recommended comfort zone. The warning is informational: the daemon still runs. Use it as a sanity check that an unusual value was deliberate.

| Field                   | Comfort zone            | Why values outside the zone are unusual                                                                                                                                                                                                                            |
|-------------------------|-------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `max_payload_size`      | 256 KiB to 16 MiB       | Smaller may reject legitimate OTLP batches; larger increases ingest latency and RSS                                                                                                                                                                                |
| `max_active_traces`     | 1,000 to 100,000        | Smaller triggers aggressive LRU eviction; larger grows memory roughly linearly                                                                                                                                                                                     |
| `max_events_per_trace`  | 100 to 10,000           | Smaller truncates complex traces; larger rarely improves detection quality                                                                                                                                                                                         |
| `max_retained_findings` | 100 to 100,000 (or `0`) | Smaller evicts findings before `/api/findings` can serve them; larger holds a backlog. `0` disables the store and is silent                                                                                                                                        |
| `trace_ttl_ms`          | 1,000 to 600,000        | Below 1s flushes traces before slow spans land; above 10min keeps near-dead traces                                                                                                                                                                                 |
| `max_fanout`            | 5 to 1,000              | Smaller floods the findings store with noise; larger suppresses most fanout detections                                                                                                                                                                             |

Comfort zones judge the static value at startup. At runtime the daemon
complements them with a settings advisor: when lifetime counters show a
knob undersized for the observed load (queue sheds, ingest rejects,
near-full trace window...), `/api/export/report` emits `tuning` entries
in `Report.warning_details` naming the knob, its current value, and the
suggested adjustment. See [METRICS.md](METRICS.md) section "Warning
kinds: transient vs sticky" for the rule table.

#### `[daemon.correlation]` (optional)

Cross-trace temporal correlation in daemon mode. When enabled, the daemon detects recurring co-occurrences between findings from different services or traces (e.g. "every time the N+1 in order-svc fires, pool saturation appears in payment-svc within 2 seconds").

| Field                | Type    | Default | Description                                                                                                               |
|----------------------|---------|---------|---------------------------------------------------------------------------------------------------------------------------|
| `enabled`            | boolean | `false` | Enable cross-trace correlation. Requires `watch` daemon mode with sustained traffic to produce useful results             |
| `window_minutes`     | integer | `10`    | Rolling window in minutes over which co-occurrences are tracked                                                           |
| `lag_threshold_ms`   | integer | `2000`  | Maximum time lag in milliseconds between two findings to consider them co-occurring                                       |
| `min_co_occurrences` | integer | `3`     | Minimum number of co-occurrences before a correlation is reported                                                         |
| `min_confidence`     | float   | `0.5`   | Minimum confidence score (0.0 to 1.0) to report a correlation. Computed as `co_occurrence_count / total_occurrences_of_A` |
| `max_tracked_pairs`  | integer | `10000` | Maximum number of finding pairs retained simultaneously. It bounds what the correlator keeps, not what one batch walks: a wide topology scans the cross product of the incoming findings and the lag window whatever this is set to, so lowering it makes the daemon refuse more rather than allocate less. Pairs scale with finding types times services, so a handful of services can overrun the default; past the cap `/api/correlations` returns an arbitrary subset with nothing on the output saying so. `perf_sentinel_correlator_pairs_evicted_total` is the signal, and the daemon logs a warning on the first eviction. Not comfort-zone checked at startup |

```toml
[daemon.correlation]
enabled = true
window_minutes = 10
lag_threshold_ms = 2000
min_co_occurrences = 3
min_confidence = 0.5
```

Correlations are exposed via `GET /api/correlations` (when `api_enabled = true`) and emitted as NDJSON on the daemon's stdout stream.

#### `[daemon.ack]` (optional, since 0.5.20)

Daemon-side runtime ack store. Complements the CI TOML acks (see
`ACKNOWLEDGMENTS.md`) with a JSONL append-only file mutated through the
HTTP API endpoints `POST` / `DELETE` `/api/findings/{signature}/ack`.

| Field          | Type    | Default                                                | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
|----------------|---------|--------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`      | boolean | `true`                                                 | Enable the daemon ack endpoints. When `false`, `POST` / `DELETE` / `GET /api/acks` return 503 Service Unavailable, and `GET /api/findings` skips the ack filter                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `storage_path` | string  | `<data_local_dir>/perf-sentinel/acks.jsonl`            | Override for the JSONL file location. Resolved at runtime via `dirs::data_local_dir()` (XDG on Linux, Library/Application Support on macOS) when absent. Error policy splits by source: an explicit override that fails to open is fatal at startup, a default path that cannot be resolved or opened only logs a WARN and leaves the two ack write routes returning 503 (`GET /api/acks` is auth-only and still answers 200 with an empty list). There is no `/tmp` fallback because the file holds audit data that must survive a reboot. Minimal containers without `HOME` (the published `FROM scratch` image, for one) fall in the second case, so set this explicitly there |
| `api_key`      | string  | *(absent)*                                             | Optional secret gating ack access. When set, `POST` and `DELETE` on `/api/findings/{signature}/ack` **and** `GET /api/acks` require the `X-API-Key` header to match (constant-time compared via `subtle`); `GET /api/findings` stays unauthenticated. Set the `PERF_SENTINEL_ACK_API_KEY` environment variable to override this and keep the key out of the committed config; the env var takes precedence when present, the same convention as `PERF_SENTINEL_EMAPS_TOKEN`. Empty string (or an env var set to empty) is rejected at config load                                                                                                                                 |
| `toml_path`    | string  | `".perf-sentinel-acknowledgments.toml"` (CWD-relative) | Override for the CI TOML acks file. Read at startup, then re-read every minute so an edit applies without a restart, which matters when the file is a mounted ConfigMap. A failed re-read keeps the previous acks and logs a warning, and so does a file that has disappeared, so neither a half-written file nor an unmounted volume un-acknowledges anything. Only an explicitly configured path is polled, never the CWD-relative default, and `enabled = false` stops the poll along with the rest. Set an absolute path for systemd or container deployments where CWD is not the repo root                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |

```toml
[daemon.ack]
enabled = true
storage_path = "/var/lib/perf-sentinel/acks.jsonl"
# api_key = "<rotate-this>"
toml_path = "/etc/perf-sentinel/acknowledgments.toml"
```

The JSONL file is replayed and atomically rewritten (via tmp + rename)
at every daemon restart, so repeated `ack` / `unack` cycles cannot
accumulate beyond their net active state. On Unix, the file is created
with mode `0600` (owner read-write only).

#### `[daemon.hub_export]` (optional)

Bounded, asynchronous export of live findings to PerfSentinelHub. The daemon
coalesces repeated findings by signature and keeps only their latest value, so
a hot issue does not create one network request per detection.

| Field                 | Type    | Default    | Description                                                                                               |
|-----------------------|---------|------------|-----------------------------------------------------------------------------------------------------------|
| `enabled`             | boolean | `false`    | Enable Hub export. Detection remains non-blocking when the Hub is unavailable                             |
| `endpoint`            | string  | *(absent)* | Hub URL ending in `/api/import/findings`                                                                  |
| `source_id`           | string  | *(absent)* | Hub source identifier: 1-64 ASCII letters, digits, `.`, `_` or `-`                                        |
| `api_key_file`        | string  | *(absent)* | File containing the source import key. Required when enabled; the key must contain at least 32 characters |
| `batch_size`          | integer | `100`      | Findings per request, from 1 to 100                                                                       |
| `flush_interval_secs` | integer | `5`        | Maximum normal batching delay, from 1 to 300 seconds                                                      |
| `max_pending`         | integer | `10000`    | Maximum distinct pending signatures, from 1 to 1,000,000                                                  |

```toml
[daemon.hub_export]
enabled = true
endpoint = "https://hub.example.com/api/import/findings"
source_id = "production-a"
api_key_file = "/run/secrets/perf-sentinel-hub-api-key"
batch_size = 100
flush_interval_secs = 5
max_pending = 10000
```

The pending structure is a latest-value table, not an unbounded queue. A
signature is sent immediately when first discovered or when its severity
worsens, then refreshed at most hourly while it recurs. Both the pending table
and the recent-success cache are capped at `max_pending`; evicting from the
recent-success cache is not a loss and is not counted, while a pending-table
eviction, an oversized finding and a batch the Hub rejects with an unretryable
4xx all increment `perf_sentinel_hub_export_dropped_total`. Failed requests
retain their coalesced batch and retry with exponential backoff and jitter; a
daemon restart clears both caches. Requests and JSON bodies are bounded to 100
findings and 2 MiB. Use HTTPS outside a trusted private network.

On a graceful shutdown (SIGTERM, `helm upgrade`, a rolling restart) the
exporter flushes what it still holds before the process exits, for up to
10 seconds. That budget is deliberate: an unreachable Hub must not hold the
daemon past the orchestrator's grace period, where the next signal is
SIGKILL and nothing is flushed at all. When the budget expires, the findings
still pending are dropped and a `WARN` names how many. Size
`terminationGracePeriodSeconds` above that budget so the drain has room to
run, otherwise the pod is killed mid-flush.

Mount the API key as a Secret-backed file. Do not put it in a ConfigMap or in
`.perf-sentinel.toml`. With the Helm chart, use `extraVolumes` and
`extraVolumeMounts` to expose the key at the configured `api_key_file` path.

#### `[daemon.cors]` (optional, since 0.5.23)

Cross-origin resource sharing for the daemon's `/api/*` query
endpoints. Disabled by default (no `Access-Control-Allow-Origin`
header is emitted, the loopback-only posture is preserved). Enable
when a browser client needs to call the daemon, typically the HTML
report in live mode (`perf-sentinel report --daemon-url <URL>`, see
`HTML-REPORT.md`).

**Scope**: the CORS layer is wired only on the `/api/*` query API
sub-router. The OTLP ingest path (`/v1/traces`), Prometheus
exposition (`/metrics`), and liveness probe (`/health`) are NOT
exposed cross-origin even under wildcard mode. Browser pages cannot
post traces, scrape `/metrics`, or hit `/health` regardless of
`allowed_origins`. This containment is intentional, browser clients
have no legitimate use for those surfaces.

**Read-endpoint exposure**: every `/api/*` GET endpoint
(`/api/findings`, `/api/acks`, `/api/status`, `/api/correlations`,
`/api/explain/*`, `/api/export/report`) is unauthenticated by design,
in line with the loopback-only posture pre-0.5.23. Once you whitelist
an origin, any browser tab on that origin can read every finding
signature, ack metadata, and trace export the daemon holds. **Only
whitelist origins you trust to view all daemon-resident data.**
Mixing untrusted origins with wildcard mode (`["*", "https://x"]`)
is rejected at config load.

| Field             | Type          | Default | Description                                                                                                                                                                                                                                                                  |
|-------------------|---------------|---------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `allowed_origins` | array<string> | `[]`    | List of origins permitted to call the daemon's `/api/*` surface. `["*"]` is wildcard mode (development only, no credentials). A non-wildcard list whitelists exact origins. Each non-wildcard entry must be a full origin (scheme + host + optional port), no trailing slash |

Wildcard example (development):

```toml
[daemon.cors]
allowed_origins = ["*"]
```

Production example (whitelist):

```toml
[daemon.cors]
allowed_origins = [
    "https://reports.example.com",
    "https://gitlab.example.com",
]
```

Methods allowed: `GET`, `POST`, `DELETE`, `OPTIONS`.
Headers allowed: `Content-Type`, `X-API-Key`. (`X-User-Id` is not
advertised because the daemon does not enforce it server-side; the
`by` field on an ack POST body is operator-attested only.)
Preflight `Access-Control-Max-Age`: 120 seconds. Long enough to
amortize the OPTIONS roundtrip across a typical interaction, short
enough that a tightened whitelist takes effect on the next browser
preflight without a forced refresh.

The CORS layer does not set `Access-Control-Allow-Credentials: true`,
which is incompatible with `["*"]` and unnecessary because the daemon
auths via the `X-API-Key` header rather than cookies. Browsers running
on a non-whitelisted origin receive responses without the
`Access-Control-Allow-Origin` header and the request is blocked
client-side without a daemon-side rejection.

Origins that fail to parse as a valid HTTP header value (typically a
copy-paste with embedded control characters) are dropped at startup
with a `warn!` log and the rest of the list is honored. If every entry
is invalid, the layer is disabled entirely. If `daemon_api_enabled =
false`, the CORS layer is skipped (the `/api/*` sub-router is not
mounted in the first place) and a `warn!` notes the unused config.

Since 0.5.27, combining
`allowed_origins = ["*"]` with `[daemon.ack] api_key` also emits a
startup `warn!`. Wildcard CORS plus an `X-API-Key` auth lets any
browser origin replay a captured key through the daemon, even though
no cookie or `Allow-Credentials` mode is in play. Whitelist explicit
origins for production deployments where the API key is set.

### `[reporting]`

Public-disclosure settings consumed by `disclose`, `hash-bake` and `verify-hash`. The whole section is optional. An absent section means the operator never asked for a periodic disclosure. Full walkthrough in `docs/REPORTING.md`, field reference in `docs/SCHEMA.md`.

| Field                   | Type   | Default   | Description                                                                                                                                                                            |
|-------------------------|--------|-----------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `intent`                | string | *(unset)* | `internal`, `official` or `audited`. Read at daemon startup only: `audited` makes the daemon refuse to start (not implemented), `official` requires `org_config_path` and validates it |
| `org_config_path`       | string | *(unset)* | Path to the organisation/scope/methodology TOML, required when `intent = "official"`                                                                                                   |
| `confidentiality_level` | string | *(unset)* | `internal` or `public`. **Reserved**, validated then unused: the published value comes from `disclose --confidentiality`                                                               |
| `disclose_output_path`  | string | *(unset)* | **Reserved**, no effect today, `disclose --output` is what writes the report                                                                                                           |
| `disclose_period`       | string | *(unset)* | `calendar-quarter`, `calendar-month`, `calendar-year` or `custom`. **Reserved**, unused, see `disclose --period-type`                                                                  |

`disclose` reads none of these: it takes `--intent`, `--confidentiality`, `--period-type`, `--org-config` and `--output` from the command line. What this section still does is gate the daemon at startup through `intent` and `org_config_path`.

The `[reporting.sigstore]` sub-section holds the Sigstore endpoints, Rekor being the transparency log and Fulcio the certificate authority. **Both are reserved: they are parsed and then unused.** `verify-hash` delegates signature checking to the `cosign` binary and invokes `cosign verify-blob` without `--rekor-url` or `--fulcio-url`, so cosign follows its own configuration and setting either key here has no effect. Point a private Sigstore instance at cosign itself until this is wired up.

| Field        | Type   | Default                       | Description                                      |
|--------------|--------|-------------------------------|--------------------------------------------------|
| `rekor_url`  | string | `https://rekor.sigstore.dev`  | Rekor transparency log endpoint. Reserved.       |
| `fulcio_url` | string | `https://fulcio.sigstore.dev` | Fulcio certificate authority endpoint. Reserved. |

## Minimal configuration

An empty file or no file at all uses all defaults. A minimal configuration for CI might only set thresholds:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
io_waste_ratio_max = 0.25
```

## Full configuration example

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
n_plus_one_messaging_warning_max = 3
io_waste_ratio_max = 0.30
# min_usable_span_ratio = 0.9   # unset = disabled, see the thresholds table

[detection]
n_plus_one_min_occurrences = 5
window_duration_ms = 500
slow_query_threshold_ms = 500
slow_query_min_occurrences = 3
max_fanout = 20
chatty_service_min_calls = 15
pool_saturation_concurrent_threshold = 10
serialized_min_sequential = 3
# Recovery heuristic for already-parameterized SQL: "auto", "strict",
# "always", "never". The variance bar below is what separates a real N+1
# from a cached repeat. Raise it on a jittery runtime such as PHP-FPM,
# where repeats of one cached query spread past 0.5 and read as N+1.
sanitizer_aware_classification = "auto"
sanitizer_aware_min_cv = 0.5

[green]
enabled = true
default_region = "eu-west-3"

[daemon]
listen_address = "127.0.0.1"
listen_port_http = 4318
listen_port_grpc = 4317
json_socket = "/tmp/perf-sentinel.sock"
max_active_traces = 10000
trace_ttl_ms = 30000
sampling_rate = 1.0
max_events_per_trace = 1000
max_payload_size = 16777216
# Optional: enable TLS on both gRPC and HTTP listeners.
# Both fields must be set together (or both absent for plain TCP).
# tls_cert_path = "/etc/tls/server-cert.pem"
# tls_key_path = "/etc/tls/server-key.pem"
api_enabled = true
max_retained_findings = 10000
max_retained_traces = 50
# Optional: tune the bounded queues (defaults shown). Raise under bursty
# load to reduce ingestion backpressure / analysis shedding.
ingest_queue_capacity = 1024
analysis_queue_capacity = 1024
# Optional: `service` label on findings and slow-span histograms (bounded
# by the daemon's cardinality caps). false restores the pre-0.18 shape.
per_service_labels = true
# Optional: `grouping` label next to `service` on the same series plus the
# per-service I/O counters (bounded by its own caps). false restores the
# 0.18 shape.
per_grouping_labels = true

# Optional: reject OTLP ingest when cgroup memory crosses this percent of
# the container limit, bounding RSS against OOM. 0 disables (default).
# Linux/cgroup-v2 only. Set 80-85 to leave headroom above steady state.
memory_high_water_pct = 0

# Optional: cross-trace correlation (daemon mode only)
# [daemon.correlation]
# enabled = true
# window_minutes = 10
# lag_threshold_ms = 2000
```

## Migration from 0.5.x

Eight legacy top-level keys were deprecated in 0.5.26 and removed in 0.6.0. A 0.5.x config that still uses any of them now fails at load time with a migration message rather than silently falling back to the default. Update to the sectioned form below before upgrading.

| Removed (top-level)    | Use instead                  | Section       |
|------------------------|------------------------------|---------------|
| `n_plus_one_threshold` | `n_plus_one_min_occurrences` | `[detection]` |
| `window_duration_ms`   | `window_duration_ms`         | `[detection]` |
| `listen_addr`          | `listen_address`             | `[daemon]`    |
| `listen_port`          | `listen_port_http`           | `[daemon]`    |
| `max_active_traces`    | `max_active_traces`          | `[daemon]`    |
| `trace_ttl_ms`         | `trace_ttl_ms`               | `[daemon]`    |
| `max_events_per_trace` | `max_events_per_trace`       | `[daemon]`    |
| `max_payload_size`     | `max_payload_size`           | `[daemon]`    |

Migration example. Before (0.5.x):

```toml
n_plus_one_threshold = 5
listen_port = 4318
max_payload_size = 2097152
```

After (0.6.0+):

```toml
[detection]
n_plus_one_min_occurrences = 5

[daemon]
listen_port_http = 4318
max_payload_size = 2097152
```

Loading a 0.5.x file on 0.6.0 returns a `ConfigError::Validation` whose message names both the removed key and its replacement, so a single tail of the error stream tells you exactly what to edit.

## Environment variables

Configuration files must never contain secrets. For sensitive values (API keys, tokens), use environment variables in your deployment tooling. perf-sentinel itself does not read environment variables for configuration.

## Acknowledgments file

`.perf-sentinel-acknowledgments.toml` is a separate file from `.perf-sentinel.toml`. It lives at the root of the application repo and lists findings the team has accepted as known. Acknowledged findings are filtered from the CLI output (`analyze`, `report`, `inspect`, `diff`) and excluded from the quality gate.

Loading rules:

- The default path is `./.perf-sentinel-acknowledgments.toml` in the current working directory. Override with `--acknowledgments <path>`.
- If the file does not exist, the run is a no-op (no error, no output noise).
- `--no-acknowledgments` skips the file entirely (audit view).
- A typo in `signature`, a missing required field, or a malformed `expires_at` fails the run loud rather than silently widening the matched set.

Minimal entry:

```toml
[[acknowledged]]
signature = "redundant_sql:order-service:POST__api_orders:cafebabecafebabecafebabecafebabe"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-05-02"
reason = "Cache invalidation pattern, intentional. See ADR-0042."
```

The `expires_at = "YYYY-MM-DD"` field is optional. Omitting it makes the ack permanent. Setting it lets you require a periodic re-evaluation: when the date passes, the ack stops applying and the finding reappears in the next CI run.

There is no glob or wildcard support, each entry is matched against an exact signature. Signatures are emitted on every finding in the JSON output, copy-paste them into the file rather than recomputing the SHA-256 prefix by hand.

For the full workflow and FAQ, see [`ACKNOWLEDGMENTS.md`](ACKNOWLEDGMENTS.md).
