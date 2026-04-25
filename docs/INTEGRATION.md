# Integration guide

perf-sentinel accepts OpenTelemetry traces via OTLP (gRPC on port 4317, HTTP on port 4318). This guide walks you from zero to your first finding for each deployment topology.

## Contents

- [Choose your topology](#choose-your-topology): comparison table for the four supported deployment modes.
- [Quick start: CI batch analysis](#quick-start-ci-batch-analysis): run perf-sentinel from a CI pipeline against a trace fixture.
- [Quick start: central collector](#quick-start-central-collector): production deployment via OpenTelemetry Collector.
- [Quick start: sidecar](#quick-start-sidecar): single-service debug in dev or staging.
- [Quick start: direct daemon](#quick-start-direct-daemon): local development.
- [Going further](#going-further): pointers to INSTRUMENTATION.md and CI.md for application-side and CI-side concerns.
- [Ingestion formats](#ingestion-formats): native JSON, OTLP, Jaeger, Zipkin, Tempo, pg_stat_statements auto-detection rules.
- [Explain mode](#explain-mode): trace-tree view of a single trace.
- [SARIF export](#sarif-export): SARIF v2.1.0 output for GitHub or GitLab code scanning.
- [Finding confidence field](#finding-confidence-field): JSON / SARIF `confidence` field for downstream consumers.
- [Daemon query API](#daemon-query-api): HTTP API on the OTLP HTTP port, see also [QUERY-API.md](./QUERY-API.md) for the full reference.
- [Advanced carbon scoring setup](#advanced-carbon-scoring-setup): multi-region scoring, Scaphandre, cloud-native energy, Electricity Maps, calibration.
- [Tempo integration](#tempo-integration): query a Grafana Tempo backend directly with `perf-sentinel tempo`.
- [Jaeger query API integration](#jaeger-query-api-integration-jaeger-and-victoria-traces): Jaeger upstream and Victoria Traces via a single subcommand.
- [Troubleshooting](#troubleshooting): common ingestion and detection issues.

## Choose your topology

| Topology                                                | Best for                          | Effort | Changes to services     |
|---------------------------------------------------------|-----------------------------------|--------|-------------------------|
| **[CI batch](#quick-start-ci-batch-analysis)**          | CI pipelines, pull request checks | Lowest | None (uses trace files) |
| **[Central collector](#quick-start-central-collector)** | Production, multi-service         | Low    | None (YAML config only) |
| **[Sidecar](#quick-start-sidecar)**                     | Dev/staging, single-service debug | Low    | None (Docker only)      |
| **[Direct daemon](#quick-start-direct-daemon)**         | Local dev, quick experiments      | Medium | Per-language env vars   |

---

## Quick start: CI batch analysis

**Use case:** run perf-sentinel in your CI pipeline to catch N+1 queries before they reach production. No daemon, no Docker, just a binary that reads a trace file and exits with code 1 if the quality gate fails.

### Step 1: Install

```bash
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64
sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel
```

### Step 2: Configure thresholds

Create `.perf-sentinel.toml` at your project root:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # zero tolerance for N+1 SQL
io_waste_ratio_max = 0.30          # max 30% avoidable I/O

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500

[green]
enabled = true
default_region = "eu-west-3"       # optional: enables gCO2eq estimates
# per-service overrides for multi-region deployments
# [green.service_regions]
# "api-us"   = "us-east-1"
# "api-asia" = "ap-southeast-1"
```

> CO₂ output is structured: `green_summary.co2.total.{low,mid,high}` plus an SCI v1.0 methodology tag, with a 2× multiplicative uncertainty interval (`low = mid/2`, `high = mid×2`). Multi-region scoring is automatic when OTel spans carry the `cloud.region` attribute. See `docs/CONFIGURATION.md` and `docs/LIMITATIONS.md#carbon-estimates-accuracy` for details.

### Step 3: Collect traces

Export traces from your integration tests. If your tests run with OTel instrumentation, save the output to a JSON file. You can also export from Jaeger UI or Zipkin UI, perf-sentinel auto-detects the format.

### Step 4: Analyze

```bash
perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
```

The process prints a JSON report to stdout and exits with code 0 (pass) or 1 (fail). Add this to your CI job:

```yaml
# GitLab CI example
perf:sentinel:
  stage: quality
  script:
    - perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
  artifacts:
    paths: [perf-sentinel-report.json]
    when: always
  allow_failure: true   # start with warning-only, remove once thresholds are calibrated
```

### Step 5: Investigate findings

```bash
# Colored terminal report
perf-sentinel analyze --input traces.json --config .perf-sentinel.toml

# Tree view of a specific trace
perf-sentinel explain --input traces.json --trace-id <trace-id>

# Interactive TUI
perf-sentinel inspect --input traces.json

# SARIF for GitHub/GitLab code scanning
perf-sentinel analyze --input traces.json --format sarif > results.sarif

# Single-file HTML dashboard for post-mortem browser exploration
perf-sentinel report --input traces.json --output report.html
```

---

### HTML dashboard report

`perf-sentinel report --input traces.json --output report.html` produces a single-file HTML dashboard. Double-click to open in any browser, works offline, no external resources. Target audience: developers exploring a CI artifact who prefer clicking over typing. The dashboard shows findings, trace trees and `GreenOps` metrics with cross-navigation between them (click a finding to see its trace tree, with the offending span highlighted in the tree view).

Flags:
- `--input <FILE>` or `--input -`: trace file or stdin (same format auto-detection as `analyze`: native JSON, Jaeger, Zipkin v2).
- `--output <FILE>`: required, overwritten if it already exists.
- `--config <PATH>`: optional `.perf-sentinel.toml`, same semantics as `analyze --config`.
- `--max-traces-embedded <N>`: cap on embedded Explain traces. When unset, the sink trims lowest-IIS traces to target a ~5 MB HTML file size. A banner in the Findings tab surfaces the trim ratio when it kicks in.
- `--pg-stat <FILE>`: cross-reference a `pg_stat_statements` CSV or JSON export. Enables a pg_stat tab and the Explain-to-pg_stat cross-navigation for SQL spans whose normalized template matches a pg_stat row.
- `--pg-stat-prometheus <URL>`: one-shot HTTP GET against a `postgres_exporter` instance, same effect as `--pg-stat` without the intermediate file. Mutually exclusive with `--pg-stat`.
- `--pg-stat-auth-header "Name: Value"`: optional auth header attached to the `--pg-stat-prometheus` request (same `"Name: Value"` format as `--auth-header` on the `tempo` and `jaeger-query` subcommands). The environment variable `PERF_SENTINEL_PGSTAT_AUTH_HEADER` takes precedence over the flag. Prefer the env var in production to avoid exposing the credential through the process argument list or shell history. When the value is supplied via the flag and the env var is not, a startup warning nudges you toward the env var. Required for Grafana Cloud, Grafana Mimir or any Prometheus ingress enforcing bearer/basic auth. The value is marked `sensitive` so hyper redacts it from debug output and HPACK tables. Sending it over plain `http://` emits a `tracing::warn!`, prefer `https://` in production.
- `--before <FILE>`: baseline report JSON (the output of `analyze --format json`). Enables a Diff tab showing new findings, resolved findings, severity changes, and per-endpoint I/O deltas relative to the baseline.

Exit codes differ from `analyze --ci`: `report` always exits 0, even when the quality gate fails. The gate status is rendered as a badge in the HTML top bar. Use `analyze --ci` when you need the CI exit-code signal.

Example invocations:

```bash
# Basic post-mortem report
perf-sentinel report --input traces.json --output report.html

# With SQL hotspot cross-reference
perf-sentinel report --input traces.json \
    --pg-stat pg_stat_statements.csv \
    --output report.html

# With scraped Prometheus hotspots instead of a file
perf-sentinel report --input traces.json \
    --pg-stat-prometheus http://prometheus:9090 \
    --output report.html

# PR regression view: diff against a baseline run
perf-sentinel report --input after.json \
    --before before.json \
    --output report.html

# Everything at once
perf-sentinel report --input traces.json \
    --pg-stat pg_stat_statements.csv \
    --before baseline.json \
    --output report.html
```

Keyboard inside the dashboard: `j`/`k` move the Findings selection, `enter` opens the current finding in Explain, `esc` walks back a four-tier priority ladder (close the cheatsheet, close the search bar, leave the Explain tab, clear active filter chips). `/` opens a substring filter on the active tab, scoped to Findings, pg_stat, Diff or Correlations. Press `?` for the full cheatsheet with every shortcut listed, including vim-style `g f` / `g e` / `g p` / `g d` / `g c` / `g r` to jump between tabs.

Large result sets: the Findings list renders the first 500 matching rows initially and exposes a `Show N more findings (remaining M)` button below the list to reveal the next chunk. Filter chip clicks, search edits and deep-link hash applies reset the visible count so the user never ends up paginated into rows that no longer match.

Sharing and export: every listable tab (Findings, pg_stat, Diff, Correlations) has an **Export CSV** button that downloads the currently filtered view as a standards-compliant CSV (RFC 4180 escaping, so templates with commas or quotes round-trip safely, plus an OWASP formula-injection guard that prefixes an apostrophe on cells starting with `=`, `+`, `-`, `@`, or a tab). The URL fragment reflects the active tab plus search and filter chips, so sending a teammate a link like `report.html#pgstat&ranking=mean_time&search=payment` restores the exact same view. Theme and last-active pg_stat ranking persist in `sessionStorage`, scoped to the current browser tab.

This is a post-mortem view over a completed trace set. For live inspection of a running daemon, use `perf-sentinel query inspect` (TUI) or the `/api/*` endpoints directly. Tempo-backed workflows compose via the shell: `perf-sentinel tempo --endpoint http://tempo:3200 --search "..." --output traces.json && perf-sentinel report --input traces.json --output report.html`.

#### Live daemon snapshot

When a daemon is running, `GET /api/export/report` emits its current state as a `Report` JSON, shape-identical to `analyze --format json`. Pipe it straight into the dashboard for a live-ish snapshot (still post-mortem semantically, just short-lived):

```bash
curl -s http://daemon.internal:4318/api/export/report \
    | perf-sentinel report --input - --output report.html
```

`report --input` auto-detects the JSON shape: an array at the top level is treated as trace events and pipelined through normalize/detect/score, an object is treated as a pre-computed Report and embedded as-is. Only daemon-produced Reports carry `correlations`, so the Correlations tab lights up automatically on the dashboard when this path is used. Cold-start daemons return `503` with `{"error": "daemon has not yet processed any events"}` until the first OTLP batch lands.

---

## Quick start: central collector

**Use case:** production deployment where services already send traces to an OpenTelemetry Collector (or you want to add one). Zero code changes, just YAML configuration.

### Step 1: Start perf-sentinel + collector

```bash
docker compose -f examples/docker-compose-collector.yml up -d
```

This starts:
- An **OTel Collector** listening on ports 4317 (gRPC) and 4318 (HTTP)
- **perf-sentinel** in watch mode, receiving traces from the collector

### Step 2: Point your services at the collector

Set these environment variables in your application containers:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

If your services already export to a collector, add perf-sentinel as an additional exporter in your existing `otel-collector-config.yaml`:

```yaml
exporters:
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      exporters: [otlp/perf-sentinel, otlp/your-existing-backend]
```

### Step 3: Generate traffic

Use your application normally. After the trace TTL expires (default 30 seconds), perf-sentinel emits findings as NDJSON to stdout:

```bash
docker compose -f examples/docker-compose-collector.yml logs -f perf-sentinel
```

### Step 4: Monitor with Prometheus + Grafana

perf-sentinel exposes Prometheus metrics at `http://localhost:14318/metrics` with OpenMetrics exemplars (click-through from Grafana to your trace backend):

```bash
curl -s http://localhost:14318/metrics | grep perf_sentinel
```

Add it as a Prometheus scrape target:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: perf-sentinel
    static_configs:
      - targets: ['perf-sentinel:4318']
```

Key metrics:
- `perf_sentinel_findings_total{type, severity}`: findings with exemplar `trace_id` for click-through
- `perf_sentinel_io_waste_ratio`: current I/O waste ratio with exemplar `trace_id`
- `perf_sentinel_events_processed_total`: total spans ingested
- `perf_sentinel_traces_analyzed_total`: total traces completed
- `perf_sentinel_slow_duration_seconds{type}`: histogram of slow span durations (use `histogram_quantile()` for global percentiles across sharded instances)

See [`examples/otel-collector-config.yaml`](../examples/otel-collector-config.yaml) for the full config with sampling and filtering options.

---

## Quick start: sidecar

**Use case:** debug a single service in dev/staging. perf-sentinel runs alongside the service, sharing its network namespace.

### Step 1: Start the sidecar

```bash
docker compose -f examples/docker-compose-sidecar.yml up -d
```

### Step 2: Configure your app

Your app sends traces to `localhost:4318` (HTTP), no network hop since perf-sentinel shares the same network namespace:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

### Step 3: View findings

```bash
docker compose -f examples/docker-compose-sidecar.yml logs -f perf-sentinel
```

See [`examples/docker-compose-sidecar.yml`](../examples/docker-compose-sidecar.yml) for the full configuration.

---

## Quick start: direct daemon

**Use case:** local development. Run perf-sentinel on your host machine and point services at it.

### Step 1: Start the daemon

```bash
perf-sentinel watch
```

By default, it listens on `127.0.0.1:4317` (gRPC) and `127.0.0.1:4318` (HTTP). For Docker containers to reach the host, use:

```toml
# .perf-sentinel.toml
[daemon]
listen_address = "0.0.0.0"
```

### Step 2: Instrument your service

Set the OTLP endpoint in your service (see [per-language guides](./INSTRUMENTATION.md#devstaging-per-language-instrumentation) below):

```bash
# For services running on the host
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317

# For services running in Docker
OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4317
```

### Step 3: View findings

Findings stream to stdout as NDJSON. Prometheus metrics are available at `http://localhost:4318/metrics`.

---

## Going further

The four quick starts above land you on a working setup. Two companion guides cover the next steps:

- **[INSTRUMENTATION.md](./INSTRUMENTATION.md)**: how to send data to perf-sentinel. Per-language instrumentation (Java, Quarkus, .NET, Rust), the OTel Collector production path with sampling guidance, cloud provider integrations, Kubernetes manifests.
- **[CI.md](./CI.md)**: how to wire perf-sentinel into CI. Batch-mode invocation, copy-pasteable recipes for GitHub Actions / GitLab CI / Jenkins, the quality-gate philosophy, the interactive HTML report deployment path per provider, and the `diff` subcommand for PR regression detection.

The reference sections below stay in this document because they apply across all topologies (input/output formats, the daemon HTTP API, advanced carbon scoring, Tempo and Jaeger ingestion, troubleshooting).

## Ingestion formats

perf-sentinel auto-detects the input format when using `perf-sentinel analyze --input`:

| Format                          | Detection                                             | Example                 |
|---------------------------------|-------------------------------------------------------|-------------------------|
| **Native** (perf-sentinel JSON) | Array of objects with `"type"` field                  | Default format          |
| **Jaeger JSON**                 | Object with `"data"` key containing `"spans"`         | Exported from Jaeger UI |
| **Zipkin JSON v2**              | Array of objects with `"traceId"` + `"localEndpoint"` | Exported from Zipkin UI |

No `--format` flag is needed for input: the format is detected automatically from the first few bytes of the file.

```bash
# Jaeger export
perf-sentinel analyze --input jaeger-export.json --ci

# Zipkin export
perf-sentinel analyze --input zipkin-traces.json --ci
```

## Explain mode

To debug a specific trace, use the `explain` subcommand:

```bash
perf-sentinel explain --input traces.json --trace-id abc123-def456
```

This produces a tree view of the trace with findings annotated inline. Use `--format json` for structured output.

## SARIF export

For GitHub or GitLab code scanning integration, export findings as SARIF v2.1.0:

```bash
perf-sentinel analyze --input traces.json --format sarif > results.sarif
```

Upload the SARIF file to your code scanning dashboard. Each finding maps to a SARIF result with `ruleId`, `level`, `logicalLocations` (service + endpoint), a custom `properties.confidence` tag and a standard SARIF `rank` value (0-100) derived from the confidence.

## Finding confidence field

Every finding emitted in JSON or SARIF carries a `confidence` field indicating the source context of the detection. The field is designed for downstream consumers such as perf-lint, a planned companion IDE integration that will boost or reduce the severity shown in the IDE depending on how much trust to place in the finding. Any custom tooling that consumes perf-sentinel's JSON or SARIF output can use the same field the same way.

Values:

| Value                 | When emitted                                                            | SARIF `rank` | Interpretation                                                                      |
|-----------------------|-------------------------------------------------------------------------|--------------|-------------------------------------------------------------------------------------|
| `"ci_batch"`          | `perf-sentinel analyze` (batch mode, always)                            | `30`         | Low confidence: the trace came from a controlled CI run with limited traffic shapes |
| `"daemon_staging"`    | `perf-sentinel watch` with `[daemon] environment = "staging"` (default) | `60`         | Medium confidence: real traffic patterns observed on a staging deployment           |
| `"daemon_production"` | `perf-sentinel watch` with `[daemon] environment = "production"`        | `90`         | Highest confidence: real traffic, real scale, real users                            |

**Example JSON finding:**

```json
{
  "type": "n_plus_one_sql",
  "severity": "warning",
  "trace_id": "abc123",
  "service": "order-svc",
  "source_endpoint": "POST /api/orders/{id}/submit",
  "pattern": { "template": "SELECT * FROM order_item WHERE order_id = ?", "occurrences": 6, "window_ms": 250, "distinct_params": 6 },
  "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
  "first_timestamp": "2026-04-08T03:14:01.050Z",
  "last_timestamp": "2026-04-08T03:14:01.300Z",
  "confidence": "daemon_production"
}
```

**Example SARIF result fragment:**

```json
{
  "ruleId": "n_plus_one_sql",
  "level": "warning",
  "message": { "text": "n_plus_one_sql in order-svc on POST /api/orders/{id}/submit..." },
  "properties": { "confidence": "daemon_production" },
  "rank": 90
}
```

**How to configure the value in the daemon:**

```toml
[daemon]
# "staging" (default) → confidence = daemon_staging, rank = 60
# "production"        → confidence = daemon_production, rank = 90
environment = "production"
```

The value is stamped on every finding emitted by that daemon instance. Invalid values (anything other than `staging`/`production`, case-insensitive) are rejected at config load with a clear error. Batch `analyze` mode ignores this field and always emits `ci_batch`.

**Planned perf-lint interop.** perf-lint (planned as a companion IDE integration, not yet published) will read the `confidence` field on imported runtime findings and apply a severity multiplier: `ci_batch` findings shown as hints, `daemon_staging` as warnings, `daemon_production` as errors. This way a finding that has been observed on real production traffic will surface louder in the IDE than one observed only in a CI fixture.

---

## Daemon query API

The daemon exposes an HTTP query API on the same port as OTLP HTTP and `/metrics` (default `4318`). It lets external systems pull recent findings, trace explanations, cross-trace correlations and daemon liveness without parsing NDJSON logs. Useful for Prometheus alerting, custom Grafana panels or SRE runbooks.

```bash
# Daemon liveness
curl -sS http://127.0.0.1:4318/api/status

# Recent critical findings
curl -sS "http://127.0.0.1:4318/api/findings?severity=critical&limit=10"
```

See [`docs/QUERY-API.md`](./QUERY-API.md) for the full per-endpoint reference, real captured response examples, use cases (Prometheus alerting, Grafana dashboard, SRE runbook) and the stability contract.

---

## Advanced carbon scoring setup

### Multi-region scoring

If your services span multiple cloud regions, perf-sentinel can apply per-region carbon intensity coefficients. The primary mechanism is the OTel `cloud.region` resource attribute, which most cloud-hosted OTel SDKs emit automatically. When this attribute is absent (e.g., Jaeger/Zipkin ingestion), use the `[green.service_regions]` table to map services to regions:

```toml
[green]
default_region = "eu-west-3"

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc"  = "ap-southeast-1"
"auth-svc"  = "eu-west-3"
```

The region resolution chain is: span `cloud.region` attribute > `service_regions[service]` > `default_region` > synthetic `"unknown"` bucket. The JSON report includes a `regions[]` array sorted by CO2 descending, with each row showing the region name, grid intensity, PUE, I/O op count and operational CO2.

### Scaphandre integration (on-premise / bare metal)

For on-premise or bare-metal servers with Intel RAPL support, perf-sentinel can scrape [Scaphandre's](https://github.com/hubblo-org/scaphandre) per-process power metrics to replace the I/O proxy model with measured energy data.

**Prerequisites:**
- Scaphandre installed and running on each host, exposing a Prometheus `/metrics` endpoint.
- RAPL access available (bare metal or VM with RAPL passthrough).

**Configuration:**

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5
process_map = { "order-svc" = "java", "game-svc" = "game", "chat-svc" = "dotnet" }
```

The `process_map` maps perf-sentinel service names to the `exe` label in Scaphandre's `scaph_process_power_consumption_microwatts` metric. The daemon scrapes this endpoint every `scrape_interval_secs` and computes a per-service energy-per-op coefficient using the formula: `energy_kwh = (power_watts * interval) / ops / 3_600_000`.

Services not present in `process_map` or when the endpoint is unreachable, fall back to the proxy model transparently. The model tag flips to `"scaphandre_rapl"` for services using measured energy. Only the `watch` daemon mode uses Scaphandre; the `analyze` batch command always uses the proxy model.

#### Authenticated Scaphandre endpoint

If the Scaphandre exporter sits behind a reverse proxy enforcing basic auth or a bearer-token ingress, add an `auth_header` entry:

```toml
[green.scaphandre]
endpoint = "https://scaphandre.my-cluster.example/metrics"
scrape_interval_secs = 5
auth_header = "Authorization: Basic <base64>"
```

The value follows the same `"Name: Value"` format as the `--auth-header` flag on the `tempo` and `jaeger-query` subcommands. The parsed value is marked `sensitive`, hyper redacts it from debug output and HTTP/2 HPACK tables, and the struct's manual `Debug` impl prevents it leaking through any `tracing::debug!(?config)` call.

The environment variable `PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER` takes precedence over the config file. Prefer the env var in production to avoid committing secrets to version control. When the value is set in the config file and the env var is not, a startup warning nudges you toward the env var.

Sending an auth header over plain `http://` emits a `tracing::warn!` once at scraper startup, prefer `https://` in production. A malformed header disables the scraper subsystem with a `tracing::error!` rather than retrying silently.

### Cloud-native energy estimation (AWS / GCP / Azure)

For cloud VMs that do not expose RAPL (most non-bare-metal instances), perf-sentinel can estimate per-service energy using CPU utilization metrics from a Prometheus endpoint and the SPECpower model.

**Prerequisites:**
- A Prometheus-compatible endpoint with CPU utilization metrics (via cloudwatch_exporter, stackdriver-exporter, azure-metrics-exporter or node_exporter).
- perf-sentinel does NOT query cloud provider APIs directly.

**Configuration:**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"
default_instance_type = "c5.xlarge"
cpu_metric = "node_cpu_seconds_total"

[green.cloud.services.api-us]
provider = "aws"
region = "us-east-1"
instance_type = "c5.4xlarge"

[green.cloud.services.analytics]
provider = "azure"
region = "westeurope"
instance_type = "Standard_D8s_v3"
```

The daemon interpolates power consumption as `watts = idle_watts + (max_watts - idle_watts) * (cpu% / 100)` using SPECpower data embedded in the binary (~60 common instance types across AWS, GCP, Azure). The model tag is `"cloud_specpower"`. Like Scaphandre, this is a daemon-only feature.

**Energy source precedence.** When both Scaphandre and cloud energy are configured for the same service, Scaphandre wins (direct RAPL measurement is more precise than CPU% interpolation). The full chain: `electricity_maps_api` > `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.

#### Authenticated Prometheus endpoint

If your Prometheus sits behind basic auth, a bearer-token proxy, or a hosted service like Grafana Cloud or Grafana Mimir, add an `auth_header` entry:

```toml
[green.cloud]
prometheus_endpoint = "https://prometheus.grafana-cloud.example/api/prom"
auth_header = "Authorization: Bearer ${GRAFANA_CLOUD_TOKEN}"
```

The value follows the same `"Name: Value"` format as the `--auth-header` flag on the `tempo` and `jaeger-query` subcommands. The parsed value is marked `sensitive`, hyper redacts it from debug output and HTTP/2 HPACK tables, and the struct's manual `Debug` impl prevents it leaking through any `tracing::debug!(?config)` call.

The environment variable `PERF_SENTINEL_CLOUD_AUTH_HEADER` takes precedence over the config file. Prefer the env var in production to avoid committing secrets to version control. When the value is set in the config file and the env var is not, a startup warning nudges you toward the env var.

Sending an auth header over plain `http://` emits a `tracing::warn!` once at scraper startup, prefer `https://` in production. A malformed header disables the scraper subsystem with a `tracing::error!` rather than retrying silently.

### Calibrate the proxy model from on-site measurements

When neither Scaphandre nor cloud energy are available but you have reference energy measurements from an external source (power meter, RAPL export, datacenter monitoring), the `perf-sentinel calibrate` subcommand tunes the I/O-to-energy proxy coefficients per service. The three-step workflow:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow_dark.svg">
  <img alt="Calibration workflow" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow.svg">
</picture>

**1. Measure.** Run a reference workload and collect both traces (standard perf-sentinel JSON format) and energy measurements (CSV with `timestamp,service,power_watts` or `timestamp,service,energy_kwh` columns, auto-detected from the header).

**2. Calibrate.** Run `perf-sentinel calibrate --traces traces.json --measured-energy energy.csv --output calibration.toml`. The subcommand correlates I/O ops with energy readings per service and time window, computes `factor = measured_per_op / default_proxy` and writes a TOML file. Factors > 10x or < 0.1x emit warnings (likely measurement error).

**3. Use.** Load the calibration file at config time via `[green] calibration_file = ".perf-sentinel-calibration.toml"`. The scoring loop multiplies the proxy energy by the per-service factor and the model tag gets a `+cal` suffix (e.g. `io_proxy_v2+cal`). Calibration only applies to the proxy model: Scaphandre/cloud measured energy still overrides.

---

## Tempo integration

If your infrastructure uses Grafana Tempo as the trace backend, you can query it directly with `perf-sentinel tempo` instead of exporting traces to files.

> **Post-mortem workflow.** When a trace is older than the daemon's 30-second live window, Tempo becomes the replay source for `perf-sentinel tempo --trace-id …`. The full incident workflow (Grafana alert → exemplar → trace_id → replay) is documented in [RUNBOOK.md](RUNBOOK.md).

### Single trace analysis

```bash
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123def456
```

### Service-based search

```bash
# Analyze the last hour of traces for order-svc
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 1h

# CI mode with quality gate
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 30m --ci
```

### Requirements

- Tempo must expose its HTTP API (default port 3200).
- The `--endpoint` flag points to the Tempo API base URL.
- Traces are fetched as OTLP protobuf and run through the standard analysis pipeline. The output is identical to `perf-sentinel analyze`.

### Tempo in microservices mode (`tempo-distributed`)

If your Tempo is deployed via the `tempo-distributed` Helm chart rather than the monolithic single-binary image, the HTTP query API is exposed by **`tempo-query-frontend`**, not by `tempo-querier`. `tempo-querier` is an internal worker with no public-facing API, so pointing `--endpoint` at it returns HTTP 404 on every `/api/search` request. Resolve the query-frontend hostname the way your environment does it (Kubernetes Service name, Docker Compose service name, or an explicit host for bare-metal):

```bash
perf-sentinel tempo --endpoint http://tempo-query-frontend:3200 \
  --service order-svc --lookback 1h
```

A 404 from a wrong endpoint now surfaces as `Tempo returned HTTP 404 for https://.../api/search?...` (the failing URL is included in the message) so this misconfiguration is diagnosable at a glance.

### Alternative: Tempo generic forwarding

Instead of querying Tempo, you can configure Tempo to forward a copy of traces to perf-sentinel via [generic forwarding](https://grafana.com/docs/tempo/latest/operations/manage-advanced-systems/generic_forwarding/). This avoids querying Tempo and works in real-time with `perf-sentinel watch`.

## Jaeger query API integration (Jaeger and Victoria Traces)

If your infrastructure uses Jaeger upstream or [Victoria Traces](https://docs.victoriametrics.com/victoriatraces/) as the trace backend, both speak the Jaeger query HTTP API and are covered by a single subcommand, `perf-sentinel jaeger-query`. Unlike Tempo's `/api/search` (ID-only), Jaeger's `/api/traces` returns full traces in one HTTP round trip, so the CLI does not parallelize per-trace fetches.

### Single trace analysis

```bash
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --trace-id abc123def456
```

### Service-based search

```bash
# Analyze the last hour of traces for order-svc
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc --lookback 1h

# Same recipe against Victoria Traces (API-compatible)
perf-sentinel jaeger-query --endpoint http://victoria-traces:10428 --service order-svc --lookback 1h

# CI mode with quality gate
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc --lookback 30m --ci
```

### Requirements

- The backend must expose the Jaeger query HTTP API (`/api/traces?service=...&lookback=...&limit=...` and `/api/traces/<id>`). Jaeger upstream (all recent versions) and Victoria Traces both qualify out of the box.
- The `--endpoint` flag points to the query API base URL (typically port 16686 for Jaeger, port 10428 for Victoria Traces).
- Traces are fetched as JSON, parsed through the same `{"data": [...]}` path as the file-mode Jaeger ingestion, then run through the standard analysis pipeline. The output is identical to `perf-sentinel analyze`.
- `--lookback` accepts the same `1h / 30m / 2h30m` format as the `tempo` subcommand.
- `--max-traces` maps to the backend's `limit` query parameter, which caps the number of traces returned per search.

### Caveats

- Backend search lookback is bounded by the backend's retention (Jaeger defaults to 48h, Victoria Traces is configurable). A `--lookback` larger than retention silently trims to the retained window.
- A `limit=N` search returns up to N full traces in a single response body. perf-sentinel caps the response at 256 MiB, which covers typical production workloads but might need adjusting if you routinely search hundreds of large traces at once. Lower `--max-traces` if you hit the body limit. `--max-traces` is itself bounded to 10 000 by the CLI.
- **Auth header via `--auth-header`.** Pass a single curl-style header line (`"Name: Value"`) to attach it to every backend request. Handles Bearer tokens, Basic Auth, or custom API-key headers. The parsed value is marked `sensitive` so it never shows in logs. See `docs/LIMITATIONS.md` for the full usage notes (one header max per invocation, value visible in `ps`).
- **`--endpoint` is trusted input.** The validator rejects non-http schemes and credential-embedded URLs, but it accepts loopback, RFC 1918, and link-local targets. In CI contexts where the endpoint value could come from an external PR, sanitize it upstream before invoking the subcommand.

---

## Troubleshooting

### No events received (`events_processed_total = 0`)

1. **Check connectivity.** From inside the container: `curl http://host.docker.internal:4318/metrics`. If it fails, perf-sentinel is not reachable.
2. **Check bind address.** perf-sentinel defaults to `127.0.0.1`. For Docker access, configure `listen_address = "0.0.0.0"` in `.perf-sentinel.toml` or run natively on the host.
3. **Check protocol.** The Java Agent defaults to gRPC (port 4317). Ensure `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` matches the port you are targeting.

### Events received but no findings

1. **Check span attributes.** perf-sentinel only processes spans with `db.statement`/`db.query.text` (SQL) or `http.url`/`url.full` (HTTP). Other spans are skipped.
2. **Check detection thresholds.** The default N+1 threshold is 5 occurrences of the same normalized template within the same trace. If your trace has fewer than 5 repeated calls, no finding is generated.
3. **Check URL normalization.** perf-sentinel replaces numeric path segments with `{id}` and UUIDs with `{uuid}`. If your repeated URLs differ only by a string identifier (e.g., `/account/alice`, `/account/bob`), they will not be grouped into the same template.

### AOT cache error with Java Agent

The Java Agent (`-javaagent:`) is incompatible with JEP 483 AOT caches. If you see `Unable to map shared spaces` or `Mismatched values for property jdk.module.addmods`, bypass the AOT cache when the agent is active (see the Java section above).

### Spring Boot starter does not capture outbound HTTP calls

The `spring-boot-starter-opentelemetry` (Spring Boot 4) bridges Micrometer metrics to OTel but does not fully instrument outbound `WebClient` or `RestTemplate` calls with trace context propagation. Use the Java Agent for complete instrumentation.
