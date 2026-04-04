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

GreenOps scoring configuration.

| Field     | Type    | Default  | Description                                                                                                                                                          |
|-----------|---------|----------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled` | boolean | `true`   | Enable GreenOps scoring (IIS, waste ratio, top offenders)                                                                                                            |
| `region`  | string  | *(none)* | Cloud region or country code for gCO2eq conversion. When set, the report includes estimated carbon emissions. Examples: `"eu-west-3"`, `"us-east-1"`, `"FR"`, `"DE"` |

When `region` is not set, the report shows raw I/O operation counts without carbon conversion. The carbon intensity table is embedded in the binary (no network calls).

### `[daemon]`

Streaming mode (`perf-sentinel watch`) settings.

| Field                  | Type    | Default                     | Description                                                                                                                                                                                                                    |
|------------------------|---------|-----------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`       | string  | `"127.0.0.1"`               | IP address to bind for OTLP and metrics endpoints. Use `127.0.0.1` for local-only access. **Warning:** setting a non-loopback address exposes unauthenticated endpoints to the network : use a reverse proxy or network policy |
| `listen_port_http`     | integer | `4318`                      | Port for OTLP HTTP receiver and Prometheus `/metrics` endpoint (range: 1-65535)                                                                                                                                                |
| `listen_port_grpc`     | integer | `4317`                      | Port for OTLP gRPC receiver (range: 1-65535)                                                                                                                                                                                   |
| `json_socket`          | string  | `"/tmp/perf-sentinel.sock"` | Unix socket path for JSON event ingestion                                                                                                                                                                                      |
| `max_active_traces`    | integer | `10000`                     | Maximum number of traces held in memory. When exceeded, the oldest trace is evicted (LRU)                                                                                                                                      |
| `trace_ttl_ms`         | integer | `30000`                     | Time-to-live for traces in milliseconds. Traces older than this are evicted and analyzed                                                                                                                                       |
| `sampling_rate`        | float   | `1.0`                       | Fraction of traces to analyze (0.0 to 1.0). Set below 1.0 to reduce load in high-traffic environments                                                                                                                          |
| `max_events_per_trace` | integer | `1000`                      | Maximum events stored per trace (ring buffer, max 100000). Oldest events are dropped when exceeded                                                                                                                             |
| `max_payload_size`     | integer | `1048576`                   | Maximum size in bytes for a single JSON payload (default: 1 MB, max 100 MB)                                                                                                                                                    |

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
