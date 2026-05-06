# Configuration reference

perf-sentinel is configured via a `.perf-sentinel.toml` file. All fields are optional and have sensible defaults.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands_dark.svg">
  <img alt="CLI commands overview" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands.svg">
</picture>

## Contents

- [Subcommands](#subcommands): which subcommands read `.perf-sentinel.toml`.
- [Sections](#sections): full per-section reference (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`, ...).
- [Minimal configuration](#minimal-configuration): the smallest useful `.perf-sentinel.toml`.
- [Full configuration example](#full-configuration-example): every section populated with example values.
- [Legacy flat format](#legacy-flat-format): pre-section format kept for backward compatibility.
- [Environment variables](#environment-variables): which env vars override config-file values.

## Subcommands

| Subcommand  | Description                                                                                                                                                                                                         |
|-------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `analyze`   | Batch analysis of trace files. Reads from file or stdin                                                                                                                                                             |
| `explain`   | Tree view of a specific trace with findings annotated inline                                                                                                                                                        |
| `watch`     | Daemon mode: real-time OTLP ingestion and streaming detection                                                                                                                                                       |
| `query`     | Query a running daemon for findings, correlations or status. Colored text output by default, `--format json` for scripting. `query inspect` opens a live TUI                                                        |
| `demo`      | Run analysis on an embedded demo dataset                                                                                                                                                                            |
| `bench`     | Benchmark throughput on a trace file                                                                                                                                                                                |
| `pg-stat`   | Analyze `pg_stat_statements` exports (CSV/JSON or Prometheus)                                                                                                                                                       |
| `inspect`   | Interactive TUI to browse traces, findings and span trees                                                                                                                                                           |
| `diff`      | Compare two trace sets and emit a delta report (new/resolved findings, severity changes, per-endpoint I/O op deltas). Text/JSON/SARIF output                                                                        |
| `report`    | Single-file HTML dashboard for post-mortem exploration in any browser. Accepts a trace file, a pre-computed Report JSON, or stdin via `--input -` (auto-detects array-of-events vs Report object, BOM-tolerant)     |
| `tempo`     | Fetch traces from a Grafana Tempo HTTP API (single trace by ID or search-then-fetch by service) and pipe them through the analysis pipeline. Gated behind the `tempo` feature                                       |
| `calibrate` | Correlate a trace file with measured energy readings (Scaphandre, cloud monitoring CSV) and emit a TOML of I/O-to-energy coefficients to load via `[green] calibration_file`                                        |

## Sections

### `[thresholds]`

Quality gate thresholds. The quality gate fails if any rule is violated.

| Field                         | Type    | Default | Description                                                                     |
|-------------------------------|---------|---------|---------------------------------------------------------------------------------|
| `n_plus_one_sql_critical_max` | integer | `0`     | Maximum number of **critical** N+1 SQL findings before the gate fails           |
| `n_plus_one_http_warning_max` | integer | `3`     | Maximum number of **warning or higher** N+1 HTTP findings before the gate fails |
| `io_waste_ratio_max`          | float   | `0.30`  | Maximum I/O waste ratio (0.0 to 1.0) before the gate fails                      |

### `[detection]`

Detection algorithm parameters.

| Field                                  | Type    | Default | Description                                                                                                                             |
|----------------------------------------|---------|---------|-----------------------------------------------------------------------------------------------------------------------------------------|
| `n_plus_one_min_occurrences`           | integer | `5`     | Minimum number of occurrences (with distinct params) to flag an N+1 pattern                                                             |
| `window_duration_ms`                   | integer | `500`   | Time window in milliseconds within which repeated operations are considered an N+1 pattern                                              |
| `slow_query_threshold_ms`              | integer | `500`   | Duration threshold in milliseconds above which an operation is considered slow                                                          |
| `slow_query_min_occurrences`           | integer | `3`     | Minimum number of slow occurrences of the same template to generate a finding                                                           |
| `max_fanout`                           | integer | `20`    | Maximum child spans per parent before flagging as excessive fanout (range: 1-100000)                                                    |
| `chatty_service_min_calls`             | integer | `15`    | Minimum HTTP outbound calls per trace to flag as chatty service. Severity: warning > threshold, critical > 3x threshold.                |
| `pool_saturation_concurrent_threshold` | integer | `10`    | Peak concurrent SQL spans per service to flag connection pool saturation risk. Uses a sweep-line algorithm on span timestamps.          |
| `serialized_min_sequential`            | integer | `3`     | Minimum sequential independent sibling calls (same parent, no time overlap, different templates) to flag as potentially parallelizable. |
| `sanitizer_aware_classification`       | string  | `"auto"`| How to classify SQL groups whose literals were collapsed to `?` by an OpenTelemetry agent's statement sanitizer. One of `"auto"`, `"strict"`, `"always"`, `"never"`. See note below.                                                                    |

#### `sanitizer_aware_classification`

OpenTelemetry agents ship with their SQL statement sanitizer ON by
default to keep PII out of trace attributes. When it is on, every span
of an ORM-induced N+1 reaches perf-sentinel with the same template and
no extractable parameters, so the standard distinct-params rule rejects
the group and the redundant detector picks it up as `redundant_sql`
instead of `n_plus_one_sql`. This setting controls the heuristic that
recovers the correct classification:

- `"auto"` (default): emit `n_plus_one_sql` when **either** the ORM
  scope signal (Spring Data, Hibernate, EF Core, SQLAlchemy,
  ActiveRecord, GORM, Prisma, Diesel, ...) **or** the per-span timing
  variance is high enough to indicate distinct row lookups. Otherwise
  leave the group to the redundant detector. Best recall on production
  Spring Data, EF Core and similar ORM stacks.
- `"strict"`: reclassify only when **both** signals fire conjointly:
  ORM scope present **and** timing variance high. Preserves
  `redundant_sql` precision on cached identical queries (legacy polling
  loops, unmemoized config lookups served from row cache), at the cost
  of missing N+1 patterns whose rows happen to be cache-warm. Use this
  when actionable `redundant_sql` findings are valuable signal that
  should not be silently absorbed into `n_plus_one_sql`.
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

### `[green]`

GreenOps scoring configuration aligned with [SCI v1.0](https://github.com/Green-Software-Foundation/sci) (operational + embodied terms, confidence intervals, multi-region).

| Field                              | Type    | Default  | Description                                                                                                                                                                                                                                                                                                                                                                                |
|------------------------------------|---------|----------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`                          | boolean | `true`   | Enable GreenOps scoring (IIS, waste ratio, top offenders, CO₂)                                                                                                                                                                                                                                                                                                                             |
| `default_region`                   | string  | *(none)* | Fallback cloud region used when neither the span's `cloud.region` attribute nor the `service_regions` mapping resolves a region. Examples: `"eu-west-3"`, `"us-east-1"`, `"FR"`                                                                                                                                                                                                            |
| `embodied_carbon_per_request_gco2` | float   | `0.001`  | SCI v1.0 `M` term: hardware manufacturing emissions amortized per request (per trace), in gCO₂eq. Region-independent. Set to `0.0` to disable embodied carbon                                                                                                                                                                                                                              |
| `use_hourly_profiles`              | boolean | `true`   | When `true`, the scoring stage uses time-of-day-specific grid intensities for the 30+ regions with embedded hourly profiles. Regions with monthly x hourly profiles (FR, DE, GB, US-East) also account for seasonal variation. Reports are tagged `model = "io_proxy_v3"` (monthly x hourly) or `"io_proxy_v2"` (flat-year hourly). Set to `false` to pin reports to the flat-annual model |
| `hourly_profiles_file`             | string  | *(none)* | Path to a JSON file with user-supplied hourly profiles. Can be absolute or relative to the config file. Profiles in this file take precedence over embedded profiles for the same region key. See "User-supplied profiles" below                                                                                                                                                           |
| `per_operation_coefficients`       | boolean | `true`   | When `true`, the proxy model weights energy per I/O op by operation type: SQL SELECT (0.5x), INSERT/UPDATE (1.5x), DELETE (1.2x) and HTTP payload size tiers (small <10 KB: 0.8x, medium 10 KB-1 MB: 1.2x, large >1 MB: 2.0x). Does not apply when Scaphandre or cloud SPECpower measured energy is available. Set to `false` to use the flat `ENERGY_PER_IO_OP_KWH` for all operations    |
| `include_network_transport`        | boolean | `false`  | When `true`, adds a network transport energy term for cross-region HTTP calls. Requires `response_size_bytes` on HTTP spans (OTel `http.response.body.size` attribute) and callee region mapped via `[green.service_regions]`. Same-region calls are excluded. Transport CO₂ appears as `transport_gco2` in the JSON report                                                                |
| `network_energy_per_byte_kwh`      | float   | `4e-11`  | Energy per byte for network transport (kWh/byte). Default 0.04 kWh/GB, midpoint of 0.03-0.06 range from Mytton et al. (2024). Only used when `include_network_transport = true`                                                                                                                                                                                                            |

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

- **`co2`**: structured `{ total, avoidable, operational_gco2, embodied_gco2 }` object. Both `total` and `avoidable` are `{ low, mid, high, model, methodology }` with **2× multiplicative uncertainty** (`low = mid/2`, `high = mid×2`). The `methodology` tag distinguishes `total` (`"sci_v1_numerator"`: `(E × I) + M` summed over traces or `"sci_v1_numerator+transport"` when network transport energy is included) from `avoidable` (`"sci_v1_operational_ratio"`: region-blind global ratio, excludes embodied). `model` values, most precise wins: `"electricity_maps_api"` > `"scaphandre_rapl"` > `"cloud_specpower"` > `"io_proxy_v3"` > `"io_proxy_v2"` > `"io_proxy_v1"`. When calibration factors are active on proxy models, `+cal` is appended (e.g. `"io_proxy_v2+cal"`).
- **`regions[]`**: per-region breakdown with `{ region, grid_intensity_gco2_kwh, pue, io_ops, co2_gco2, intensity_source }`, **sorted by `co2_gco2` descending** (highest-impact regions first) with alphabetical tiebreak. `intensity_source` is `"annual"`, `"hourly"`, `"monthly_hourly"` or `"real_time"` (Electricity Maps API) depending on which carbon intensity source was used for the region.

Carbon intensity data is embedded in the binary (no network egress). See `docs/design/05-GREENOPS-AND-CARBON.md` for the complete formula and methodology and `docs/LIMITATIONS.md#carbon-estimates-accuracy` for the directional / non-regulatory disclaimer.

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
- `"westeurope"`, `"nl"` -> `eu-west-4` (Netherlands)
- `"northeurope"`, `"ie"` -> `eu-west-1` (Ireland)
- `"uksouth"`, `"gb"`, `"uk"` -> `eu-west-2` (UK)
- `"westus2"` -> `us-west-2` (Oregon)

The full alias table is in `score/carbon_profiles.rs`. If your region key is not aliased, the flat annual value from the primary carbon table is used.

#### `[green.scaphandre]` (optional, opt-in)

Opt-in integration with [Scaphandre](https://github.com/hubblo-org/scaphandre) for per-process energy measurement on Linux hosts with Intel RAPL support. When configured, the `watch` daemon spawns a background task that scrapes the Scaphandre Prometheus endpoint every `scrape_interval_secs` and uses the measured power readings to replace the fixed `ENERGY_PER_IO_OP_KWH` constant for each mapped service.

| Field                  | Type    | Default  | Description                                                                                                                                                               |
|------------------------|---------|----------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | string  | *(none)* | Full URL of the Scaphandre Prometheus `/metrics` endpoint. Must start with `http://` or `https://` (TLS supported via hyper-rustls). Required when the section is present |
| `scrape_interval_secs` | integer | `5`      | How often to scrape, in seconds. Valid range: 1-3600                                                                                                                      |
| `process_map`          | table   | `{}`     | Maps perf-sentinel service names (from span `service.name`) to Scaphandre `exe` labels                                                                                    |

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5

[green.scaphandre.process_map]
"order-svc" = "java"
"chat-svc" = "dotnet"
"game-svc" = "game"
```

**Ignored in `analyze` batch mode.** Only the `watch` daemon spawns the scraper. The `analyze` command always uses the proxy model regardless of this section.

**Fallback behaviour.** When the endpoint is unreachable, a service is not present in `process_map` or a service had zero ops in the current scrape window, the scoring stage falls back to the proxy model for those spans. The first failure logs at `warn` level; subsequent failures log at `debug` to avoid spam. The `perf_sentinel_scaphandre_last_scrape_age_seconds` Prometheus gauge lets operators detect a hung scraper.

**Precision bounds (important).** Scaphandre improves the **per-service** energy coefficient but does NOT give per-finding attribution. RAPL is process-level, not span-level: two findings in the same process during the same scrape window share the same coefficient. See `docs/LIMITATIONS.md#scaphandre-precision-bounds` for the full discussion.

#### `[green.cloud]` (optional, opt-in)

Cloud-native energy estimation via CPU utilization + SPECpower interpolation. When configured, the `watch` daemon scrapes CPU% from a Prometheus/VictoriaMetrics endpoint and uses an embedded lookup table (idle/max watts per cloud instance type) to estimate per-service energy consumption. Supports AWS, GCP, Azure and on-premise hardware with manual watts override.

| Field                   | Type    | Default  | Description                                                                                                                          |
|-------------------------|---------|----------|--------------------------------------------------------------------------------------------------------------------------------------|
| `prometheus_endpoint`   | string  | *(none)* | Prometheus HTTP API base URL (e.g. `http://prometheus:9090` or `https://prometheus:9090`). TLS supported via hyper-rustls. Required. |
| `scrape_interval_secs`  | integer | `15`     | Polling interval in seconds (range: 1-3600).                                                                                         |
| `default_provider`      | string  | *(none)* | Default cloud provider: `"aws"`, `"gcp"`, `"azure"`.                                                                                 |
| `default_instance_type` | string  | *(none)* | Fallback instance type for unmapped services.                                                                                        |
| `cpu_metric`            | string  | *(none)* | Default PromQL metric/query for CPU utilization.                                                                                     |

Per-service entries in `[green.cloud.services]` support two forms:

**Cloud instance (table lookup):**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"

[green.cloud.services]
"account-svc" = { provider = "aws", instance_type = "c5.4xlarge" }
"api-asia" = { provider = "gcp", instance_type = "n2-standard-8" }
"analytics" = { provider = "azure", instance_type = "Standard_D8s_v3" }
```

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

| Field                | Type    | Default                              | Description                                                             |
|----------------------|---------|--------------------------------------|-------------------------------------------------------------------------|
| `api_key`              | string  | none                                 | API auth token. Prefer `PERF_SENTINEL_EMAPS_TOKEN` env var for security |
| `endpoint`             | string  | `https://api.electricitymaps.com/v4` | API base URL (`http://` or `https://`). v3 still works but emits a deprecation warning at startup |
| `poll_interval_secs`   | integer | `300`                                | Poll interval in seconds (range: 60-86400). Free tier: use 3600+        |
| `emission_factor_type` | string  | `lifecycle`                          | Emission factor model. `lifecycle` (default) includes upstream emissions (manufacturing, transport). `direct` includes only combustion. Some Scope 2 frameworks prefer `direct` for stricter accountability |
| `temporal_granularity` | string  | `hourly`                             | API response aggregation. `hourly` (default), `5_minutes`, or `15_minutes`. Sub-hour values require a paid plan that exposes them, otherwise the API silently coarsens to hourly |

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

- The JSON report carries a `green_summary.scoring_config` object with the 3 fields. Omitted when `[green.electricity_maps]` is not configured (additive on pre-0.5.12 baselines).
- The HTML dashboard renders a chip bandeau above the green-regions table. Default values (`v4`, `lifecycle`, `hourly`) are neutral chips, opt-in values (`direct`, `5_minutes`, `15_minutes`) are accent chips, the legacy `v3` endpoint shows as a warning chip mirroring the deprecation warning. Native browser tooltips explain each value.
- The terminal `print_green_summary` output prepends a one-liner `Carbon scoring: Electricity Maps v4, lifecycle, hourly` before the per-region breakdown.

The bandeau and the terminal line are hidden when `[green.electricity_maps]` is not configured.

#### `[green] calibration_file` (optional)

Path to a calibration TOML file generated by `perf-sentinel calibrate`. When present, per-service calibration factors are loaded at config time and multiply the proxy model energy per op. Does not affect Scaphandre or cloud SPECpower measured energy.

```toml
[green]
calibration_file = ".perf-sentinel-calibration.toml"
```

**`perf-sentinel calibrate` input size limits.** Both inputs are capped to protect against unbounded memory use: the `--traces` file uses `config.max_payload_size` (default 16 MiB since 0.5.13, same as `analyze`) and the `--measured-energy` CSV is capped at 64 MiB. Calibrate exits with a clear error if either file exceeds its limit. 64 MiB is generous for thousands of RAPL samples per minute, if you need more, bump `max_payload_size` and file an issue describing the workload.

#### `[tempo]` (optional)

Configuration for the `perf-sentinel tempo` subcommand. The subcommand runs in **batch mode** (not daemon), fetches traces from a Grafana Tempo HTTP API and pipes them through the standard analysis pipeline. All values below can also be set via CLI flags (flags override config).

| Field        | Type    | Default | Description                                        |
|--------------|---------|---------|----------------------------------------------------|
| `endpoint`   | string  | none    | Tempo HTTP API base URL (e.g. `http://tempo:3200`) |
| `max_traces` | integer | `100`   | Maximum traces to fetch in search mode             |

### `[daemon]`

Streaming mode (`perf-sentinel watch`) settings.

| Field                   | Type    | Default                     | Description                                                                                                                                                                                                                                                                                                                              |
|-------------------------|---------|-----------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`        | string  | `"127.0.0.1"`               | IP address to bind for OTLP and metrics endpoints. Use `127.0.0.1` for local-only access. **Warning:** setting a non-loopback address exposes unauthenticated endpoints to the network, use a reverse proxy or network policy                                                                                                            |
| `listen_port_http`      | integer | `4318`                      | Port for OTLP HTTP receiver and Prometheus `/metrics` endpoint (range: 1-65535)                                                                                                                                                                                                                                                          |
| `listen_port_grpc`      | integer | `4317`                      | Port for OTLP gRPC receiver (range: 1-65535)                                                                                                                                                                                                                                                                                             |
| `json_socket`           | string  | `"/tmp/perf-sentinel.sock"` | Unix socket path for JSON event ingestion                                                                                                                                                                                                                                                                                                |
| `max_active_traces`     | integer | `10000`                     | Maximum number of traces held in memory. When exceeded, the oldest trace is evicted (LRU). Range: 1 to 1,000,000                                                                                                                                                                                                                         |
| `trace_ttl_ms`          | integer | `30000`                     | Time-to-live for traces in milliseconds. Traces older than this are evicted and analyzed. Range: 100 to 3,600,000                                                                                                                                                                                                                        |
| `sampling_rate`         | float   | `1.0`                       | Fraction of traces to analyze (0.0 to 1.0). Set below 1.0 to reduce load in high-traffic environments                                                                                                                                                                                                                                    |
| `max_events_per_trace`  | integer | `1000`                      | Maximum events stored per trace (ring buffer). Oldest events are dropped when exceeded. Range: 1 to 100,000                                                                                                                                                                                                                              |
| `max_payload_size`      | integer | `16777216`                  | Maximum size in bytes for a single JSON payload (default: 16 MiB, raised from 1 MiB in 0.5.13 because a daemon snapshot from `/api/export/report` already exceeds 1 MiB on a modest cluster). Range: 1,024 to 104,857,600 (100 MB). The default sits at the upper inclusive boundary of the comfort zone by design                       |
| `environment`           | string  | `"staging"`                 | Deployment environment label. Accepted values: `"staging"` (default, medium confidence) or `"production"` (high confidence). Stamps every finding with the corresponding `confidence` field for downstream tooling (perf-lint planned). Case-insensitive; any other value is rejected at config load                                     |
| `tls_cert_path`         | string  | *(absent)*                  | Path to a PEM-encoded TLS certificate chain for the OTLP receivers. When set alongside `tls_key_path`, both gRPC and HTTP listeners use TLS. When absent, listeners use plain TCP. Each TLS listener caps concurrent in-flight handshakes at 128 (non-configurable) and drops peers that do not complete the handshake within 10 seconds |
| `tls_key_path`          | string  | *(absent)*                  | Path to a PEM-encoded TLS private key. Must be set together with `tls_cert_path` (both or neither). On Unix, the daemon warns if the key file is readable by group or others                                                                                                                                                             |
| `api_enabled`           | boolean | `true`                      | Enable the daemon query API endpoints (`/api/findings`, `/api/explain/{trace_id}`, `/api/correlations`, `/api/status`). Set to `false` to disable the API while keeping OTLP ingestion and `/metrics` active                                                                                                                             |
| `max_retained_findings` | integer | `10000`                     | Maximum number of recent findings retained in the daemon's ring buffer for the query API. Older findings are evicted when the limit is reached. Range: 0 to 10,000,000, where `0` disables the store entirely and reclaims its memory (recommended when `api_enabled = false`)                                                           |

##### Comfort zones and startup warnings

Daemon limits accept any value inside their hard bounds (rejected at config load), but `perf-sentinel watch` emits a one-shot `WARN` log at startup when a value falls outside the recommended comfort zone. The warning is informational: the daemon still runs. Use it as a sanity check that an unusual value was deliberate.

| Field                   | Comfort zone             | Why values outside the zone are unusual                                                                                     |
|-------------------------|--------------------------|-----------------------------------------------------------------------------------------------------------------------------|
| `max_payload_size`      | 256 KiB to 16 MiB        | Smaller may reject legitimate OTLP batches; larger increases ingest latency and RSS                                         |
| `max_active_traces`     | 1,000 to 100,000         | Smaller triggers aggressive LRU eviction; larger grows memory roughly linearly                                              |
| `max_events_per_trace`  | 100 to 10,000            | Smaller truncates complex traces; larger rarely improves detection quality                                                  |
| `max_retained_findings` | 100 to 100,000 (or `0`)  | Smaller evicts findings before `/api/findings` can serve them; larger holds a backlog. `0` disables the store and is silent |
| `trace_ttl_ms`          | 1,000 to 600,000         | Below 1s flushes traces before slow spans land; above 10min keeps near-dead traces                                          |
| `max_fanout`            | 5 to 1,000               | Smaller floods the findings store with noise; larger suppresses most fan-out detections                                     |

#### `[daemon.correlation]` (optional)

Cross-trace temporal correlation in daemon mode. When enabled, the daemon detects recurring co-occurrences between findings from different services or traces (e.g. "every time the N+1 in order-svc fires, pool saturation appears in payment-svc within 2 seconds").

| Field                | Type    | Default | Description                                                                                                               |
|----------------------|---------|---------|---------------------------------------------------------------------------------------------------------------------------|
| `enabled`            | boolean | `false` | Enable cross-trace correlation. Requires `watch` daemon mode with sustained traffic to produce useful results             |
| `window_minutes`     | integer | `10`    | Rolling window in minutes over which co-occurrences are tracked                                                           |
| `lag_threshold_ms`   | integer | `2000`  | Maximum time lag in milliseconds between two findings to consider them co-occurring                                       |
| `min_co_occurrences` | integer | `3`     | Minimum number of co-occurrences before a correlation is reported                                                         |
| `min_confidence`     | float   | `0.5`   | Minimum confidence score (0.0 to 1.0) to report a correlation. Computed as `co_occurrence_count / total_occurrences_of_A` |
| `max_tracked_pairs`  | integer | `1000`  | Maximum number of finding pairs tracked simultaneously. Prevents unbounded memory growth from high-cardinality findings   |

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

| Field          | Type    | Default                                                  | Description                                                                                                                                                                          |
|----------------|---------|----------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`      | boolean | `true`                                                   | Enable the daemon ack endpoints. When `false`, `POST` / `DELETE` / `GET /api/acks` return 503 Service Unavailable, and `GET /api/findings` skips the ack filter                      |
| `storage_path` | string  | `<data_local_dir>/perf-sentinel/acks.jsonl`              | Override for the JSONL file location. Resolved at runtime via `dirs::data_local_dir()` (XDG on Linux, Library/Application Support on macOS) when absent. The daemon refuses to start if the default cannot be resolved and no override is set; do not fall back to `/tmp` because the file holds audit data that must survive a reboot |
| `api_key`      | string  | *(absent)*                                               | Optional secret. When set, `POST` and `DELETE` on `/api/findings/{signature}/ack` require the `X-API-Key` header to match (constant-time compared via `subtle`). `GET /api/acks` and `GET /api/findings` stay unauthenticated by design (loopback reads). Empty string is rejected at config load |
| `toml_path`    | string  | `".perf-sentinel-acknowledgments.toml"` (CWD-relative)   | Override for the CI TOML acks file the daemon reads at startup. Set to an absolute path for systemd or container deployments where CWD is not the repo root                          |

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

| Field             | Type           | Default | Description                                                                                                                                                                                                                                              |
|-------------------|----------------|---------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `allowed_origins` | array<string>  | `[]`    | List of origins permitted to call the daemon's `/api/*` surface. `["*"]` is wildcard mode (development only, no credentials). A non-wildcard list whitelists exact origins. Each non-wildcard entry must be a full origin (scheme + host + optional port), no trailing slash |

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
io_waste_ratio_max = 0.30

[detection]
n_plus_one_min_occurrences = 5
window_duration_ms = 500
slow_query_threshold_ms = 500
slow_query_min_occurrences = 3
max_fanout = 20
chatty_service_min_calls = 15
pool_saturation_concurrent_threshold = 10
serialized_min_sequential = 3

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

# Optional: cross-trace correlation (daemon mode only)
# [daemon.correlation]
# enabled = true
# window_minutes = 10
# lag_threshold_ms = 2000
```

## Legacy flat format

For backward compatibility, perf-sentinel also accepts a flat (non-sectioned) format:

```toml
n_plus_one_threshold = 5
window_duration_ms = 500
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30
```

When both formats are present, sectioned values take priority over flat values. The sectioned format is recommended for new configurations.

### Deprecated keys

The following top-level (flat) keys are deprecated. They still resolve correctly for backward compatibility, but emit a `WARN`-level deprecation message at config load when no section override is set. They will be removed in a future release. Migrate to the sectioned form below.

| Deprecated (flat) | Use instead | Section |
|---|---|---|
| `n_plus_one_threshold` | `n_plus_one_min_occurrences` | `[detection]` |
| `window_duration_ms` | `window_duration_ms` | `[detection]` |
| `listen_addr` | `listen_address` | `[daemon]` |
| `listen_port` | `listen_port_http` | `[daemon]` |
| `max_active_traces` | `max_active_traces` | `[daemon]` |
| `trace_ttl_ms` | `trace_ttl_ms` | `[daemon]` |
| `max_events_per_trace` | `max_events_per_trace` | `[daemon]` |
| `max_payload_size` | `max_payload_size` | `[daemon]` |

Migration example. Before (deprecated):

```toml
n_plus_one_threshold = 5
listen_port = 4318
max_payload_size = 2097152
```

After (recommended):

```toml
[detection]
n_plus_one_min_occurrences = 5

[daemon]
listen_port_http = 4318
max_payload_size = 2097152
```

When both forms are present for the same setting, the sectioned form takes precedence and no deprecation warning is emitted for that key.

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
signature = "redundant_sql:order-service:POST__api_orders:cafebabecafebabe"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-05-02"
reason = "Cache invalidation pattern, intentional. See ADR-0042."
```

The `expires_at = "YYYY-MM-DD"` field is optional. Omitting it makes the ack permanent. Setting it lets you require a periodic re-evaluation: when the date passes, the ack stops applying and the finding reappears in the next CI run.

There is no glob or wildcard support, each entry is matched against an exact signature. Signatures are emitted on every finding in the JSON output, copy-paste them into the file rather than recomputing the SHA-256 prefix by hand.

For the full workflow and FAQ, see [`ACKNOWLEDGMENTS.md`](ACKNOWLEDGMENTS.md).
