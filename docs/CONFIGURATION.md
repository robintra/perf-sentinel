# Configuration reference

perf-sentinel is configured via a `.perf-sentinel.toml` file. All fields are optional and have sensible defaults.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="diagrams/svg/cli-commands_dark.svg">
  <img alt="CLI commands overview" src="diagrams/svg/cli-commands.svg">
</picture>

## Subcommands

| Subcommand | Description                                                      |
|------------|------------------------------------------------------------------|
| `analyze`  | Batch analysis of trace files. Reads from file or stdin          |
| `explain`  | Tree view of a specific trace with findings annotated inline     |
| `watch`    | Daemon mode: real-time OTLP ingestion and streaming detection    |
| `demo`     | Run analysis on an embedded demo dataset                         |
| `bench`    | Benchmark throughput on a trace file                             |
| `pg-stat`  | Analyze `pg_stat_statements` exports (CSV/JSON) for SQL hotspots |
| `inspect`  | Interactive TUI to browse traces, findings and span trees       |

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

| Field                        | Type    | Default | Description                                                                                |
|------------------------------|---------|---------|--------------------------------------------------------------------------------------------|
| `n_plus_one_min_occurrences` | integer | `5`     | Minimum number of occurrences (with distinct params) to flag an N+1 pattern                |
| `window_duration_ms`         | integer | `500`   | Time window in milliseconds within which repeated operations are considered an N+1 pattern |
| `slow_query_threshold_ms`    | integer | `500`   | Duration threshold in milliseconds above which an operation is considered slow             |
| `slow_query_min_occurrences` | integer | `3`     | Minimum number of slow occurrences of the same template to generate a finding              |
| `max_fanout`                 | integer | `20`    | Maximum child spans per parent before flagging as excessive fanout (range: 1-100000)       |

### `[green]`

GreenOps scoring configuration aligned with [SCI v1.0](https://github.com/Green-Software-Foundation/sci) (operational + embodied terms, confidence intervals, multi-region).

| Field                              | Type    | Default  | Description                                                                                                                                                                                                                                                                                                                                            |
|------------------------------------|---------|----------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`                          | boolean | `true`   | Enable GreenOps scoring (IIS, waste ratio, top offenders, CO₂)                                                                                                                                                                                                                                                                                         |
| `default_region`                   | string  | *(none)* | Fallback cloud region used when neither the span's `cloud.region` attribute nor the `service_regions` mapping resolves a region. Examples: `"eu-west-3"`, `"us-east-1"`, `"FR"`                                                                                                                                                                        |
| `embodied_carbon_per_request_gco2` | float   | `0.001`  | SCI v1.0 `M` term: hardware manufacturing emissions amortized per request (per trace), in gCO₂eq. Region-independent. Set to `0.0` to disable embodied carbon                                                                                                                                                                                          |
| `use_hourly_profiles`              | boolean | `true`   | When `true`, the scoring stage uses time-of-day-specific grid intensities for regions that have a 24-hour UTC profile embedded (FR, DE, GB, US-East). Reports that touched a profiled region are tagged `model = "io_proxy_v2"` instead of `"io_proxy_v1"`. Set to `false` to pin reports to the flat-annual model (useful for historical comparisons) |

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

- **`co2`**: structured `{ total, avoidable, operational_gco2, embodied_gco2 }` object. Both `total` and `avoidable` are `{ low, mid, high, model, methodology }` with **2× multiplicative uncertainty** (`low = mid/2`, `high = mid×2`). The `methodology` tag distinguishes `total` (`"sci_v1_numerator"`: `(E × I) + M` summed over traces) from `avoidable` (`"sci_v1_operational_ratio"`: region-blind global ratio, excludes embodied). `model` values, most precise wins: `"scaphandre_rapl"` → `"io_proxy_v2"` → `"io_proxy_v1"`.
- **`regions[]`**: per-region breakdown with `{ region, grid_intensity_gco2_kwh, pue, io_ops, co2_gco2, intensity_source }`, **sorted by `co2_gco2` descending** (highest-impact regions first) with alphabetical tiebreak. `intensity_source` is `"annual"` or `"hourly"` depending on which carbon table was consulted for the region.

Carbon intensity data is embedded in the binary (no network egress). See `docs/design/05-GREENOPS-AND-CARBON.md` for the complete formula and methodology, and `docs/LIMITATIONS.md#carbon-estimates-accuracy` for the directional / non-regulatory disclaimer.

#### `[green.scaphandre]` (optional, opt-in)

Opt-in integration with [Scaphandre](https://github.com/hubblo-org/scaphandre) for per-process energy measurement on Linux hosts with Intel RAPL support. When configured, the `watch` daemon spawns a background task that scrapes the Scaphandre Prometheus endpoint every `scrape_interval_secs` and uses the measured power readings to replace the fixed `ENERGY_PER_IO_OP_KWH` constant for each mapped service.

| Field                  | Type    | Default  | Description                                                                                                                                       |
|------------------------|---------|----------|---------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | string  | *(none)* | Full URL of the Scaphandre Prometheus `/metrics` endpoint. Must start with `http://` (TLS is not supported). Required when the section is present |
| `scrape_interval_secs` | integer | `5`      | How often to scrape, in seconds. Valid range: 1-3600                                                                                              |
| `process_map`          | table   | `{}`     | Maps perf-sentinel service names (from span `service.name`) to Scaphandre `exe` labels                                                            |

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

**Fallback behaviour.** When the endpoint is unreachable, a service is not present in `process_map`, or a service had zero ops in the current scrape window, the scoring stage falls back to the proxy model for those spans. The first failure logs at `warn` level; subsequent failures log at `debug` to avoid spam. The `perf_sentinel_scaphandre_last_scrape_age_seconds` Prometheus gauge lets operators detect a hung scraper.

**Precision bounds (important).** Scaphandre improves the **per-service** energy coefficient but does NOT give per-finding attribution. RAPL is process-level, not span-level: two findings in the same process during the same scrape window share the same coefficient. See `docs/LIMITATIONS.md#scaphandre-precision-bounds` for the full discussion.

### `[daemon]`

Streaming mode (`perf-sentinel watch`) settings.

| Field                  | Type    | Default                     | Description                                                                                                                                                                                                                                                                                    |
|------------------------|---------|-----------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`       | string  | `"127.0.0.1"`               | IP address to bind for OTLP and metrics endpoints. Use `127.0.0.1` for local-only access. **Warning:** setting a non-loopback address exposes unauthenticated endpoints to the network, use a reverse proxy or network policy                                                                  |
| `listen_port_http`     | integer | `4318`                      | Port for OTLP HTTP receiver and Prometheus `/metrics` endpoint (range: 1-65535)                                                                                                                                                                                                                |
| `listen_port_grpc`     | integer | `4317`                      | Port for OTLP gRPC receiver (range: 1-65535)                                                                                                                                                                                                                                                   |
| `json_socket`          | string  | `"/tmp/perf-sentinel.sock"` | Unix socket path for JSON event ingestion                                                                                                                                                                                                                                                      |
| `max_active_traces`    | integer | `10000`                     | Maximum number of traces held in memory. When exceeded, the oldest trace is evicted (LRU)                                                                                                                                                                                                      |
| `trace_ttl_ms`         | integer | `30000`                     | Time-to-live for traces in milliseconds. Traces older than this are evicted and analyzed                                                                                                                                                                                                       |
| `sampling_rate`        | float   | `1.0`                       | Fraction of traces to analyze (0.0 to 1.0). Set below 1.0 to reduce load in high-traffic environments                                                                                                                                                                                          |
| `max_events_per_trace` | integer | `1000`                      | Maximum events stored per trace (ring buffer, max 100000). Oldest events are dropped when exceeded                                                                                                                                                                                             |
| `max_payload_size`     | integer | `1048576`                   | Maximum size in bytes for a single JSON payload (default: 1 MB, max 100 MB)                                                                                                                                                                                                                    |
| `environment`          | string  | `"staging"`                 | Deployment environment label. Accepted values: `"staging"` (default, medium confidence) or `"production"` (high confidence). Stamps every finding with the corresponding `confidence` field for downstream consumers (perf-lint). Case-insensitive; any other value is rejected at config load |

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

[green]
enabled = true
region = "eu-west-3"

[daemon]
listen_address = "127.0.0.1"
listen_port_http = 4318
listen_port_grpc = 4317
json_socket = "/tmp/perf-sentinel.sock"
max_active_traces = 10000
trace_ttl_ms = 30000
sampling_rate = 1.0
max_events_per_trace = 1000
max_payload_size = 1048576
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

## Environment variables

Configuration files must never contain secrets. For sensitive values (API keys, tokens), use environment variables in your deployment tooling. perf-sentinel itself does not read environment variables for configuration.
