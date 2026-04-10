<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frobintra%2Fperf-sentinel%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&suffix=%20stable&label=rust%202024&color=D34516&logo=rust" alt="Rust" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml/badge.svg" alt="Security Audit" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-dark-horizontal.svg">
  <img alt="perf-sentinel" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-horizontal.svg">
</picture>

Analyzes runtime traces (SQL queries, HTTP calls) to detect N+1 queries, redundant calls and scores I/O intensity per endpoint (GreenOps).

## Why perf-sentinel?

Performance anti-patterns like N+1 queries exist in any application that does I/O: monoliths and microservices alike. In distributed architectures, a single user request cascades across multiple services, each with its own I/O and nobody has visibility on the full path. Existing tools are either runtime-specific (Hypersistence Utils -> JPA only), heavy and proprietary (Datadog, New Relic), or limited to unit tests without cross-service visibility.

perf-sentinel takes a different approach: **protocol-level analysis**. It observes the traces your application produces (SQL queries, HTTP calls) regardless of language or ORM. It doesn't need to understand JPA, EF Core, or SeaORM, it sees the queries they generate.

## GreenOps: built-in carbon-aware scoring

Every finding includes an **I/O Intensity Score (IIS)**: the number of I/O operations generated per user request for a given endpoint. Reducing unnecessary I/O (N+1 queries, redundant calls) improves response times *and* reduces energy consumption, these are not competing goals.

- **I/O Intensity Score** = total I/O ops for an endpoint / number of invocations
- **I/O Waste Ratio** = avoidable I/O ops (from findings) / total I/O ops

Aligned with the **Software Carbon Intensity** model ([SCI v1.0 / ISO/IEC 21031:2024](https://github.com/Green-Software-Foundation/sci)) from the Green Software Foundation. The `co2.total` field holds the **SCI numerator** `(E × I) + M` summed over analyzed traces, not the per-request intensity score. Multi-region scoring is automatic when OTel spans carry the `cloud.region` attribute. **30+ cloud regions** have embedded hourly carbon intensity profiles, with monthly x hourly seasonal variation for FR, DE, GB, and US-East. In daemon mode, energy estimation can be refined via **Scaphandre RAPL** (bare metal) or **cloud-native CPU% + SPECpower** (AWS/GCP/Azure), with automatic fallback to the I/O proxy model. Users can also supply their own hourly profiles via `[green] hourly_profiles_file`.

> **Note:** CO₂ estimates are **directional**, not regulatory-grade. Every estimate carries a `~2×` multiplicative uncertainty bracket (`low = mid/2`, `high = mid×2`) because the I/O proxy model is rough. perf-sentinel is a **waste counter**, not a carbon-accounting tool. Do not use it for CSRD or GHG Protocol Scope 3 reporting. See [docs/LIMITATIONS.md](docs/LIMITATIONS.md#carbon-estimates-accuracy) for the full methodology.

## How does it compare?

| Criteria           | [Hypersistence Optimizer](https://vladmihalcea.com/hypersistence-optimizer/) | [Datadog APM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Digma](https://digma.ai/) | **perf-sentinel** |
|--------------------|------------------------------------------------------------------------------|-------------------------------------------------------|-----------------------------------------------------------------------|----------------------------|-------------------|
| N+1 SQL detection  | ✅ JPA only                                                                   | ⚠️ Manual (trace view)                                | ⚠️ Manual (trace view)                                                | ✅ (JVM)                    | ✅ Polyglot        |
| N+1 HTTP detection | ❌                                                                            | ⚠️ Manual (trace view)                                | ⚠️ Manual (trace view)                                                | ⚠️ Partial                 | ✅                 |
| Polyglot           | ❌ Java/JPA                                                                   | ✅ (per-language agents)                               | ✅ (per-language agents)                                               | ⚠️ JVM + .NET              | ✅ Protocol-level  |
| Cross-service      | ❌                                                                            | ✅                                                     | ✅                                                                     | ⚠️ Partial                 | ✅ Trace ID        |
| GreenOps / SCI     | ❌                                                                            | ❌                                                     | ❌                                                                     | ❌                          | ✅ Built-in        |
| Lightweight        | N/A (lib)                                                                    | ❌ (~150 MB)                                           | ❌ (~150 MB)                                                           | ❌ (~100 MB)                | ✅ (<10 MB RSS)    |
| Open-source        | ❌ Commercial                                                                 | ❌                                                     | ⚠️ Limited free tier                                                  | ⚠️ Freemium                | ✅ AGPL v3         |
| CI/CD quality gate | ⚠️ (manual assertions)                                                       | ❌                                                     | ⚠️ (alerts, no native gate)                                           | ⚠️                         | ✅ Native          |

## What does it report?

For each detected anti-pattern, perf-sentinel reports:

- **Type:** N+1 SQL, N+1 HTTP, redundant query, slow SQL, slow HTTP, excessive fanout, chatty service, pool saturation, or serialized calls
- **Normalized template:** the query or URL with parameters replaced by placeholders (`?`, `{id}`)
- **Occurrences:** how many times the pattern fired within the detection window
- **Source endpoint:** which application endpoint triggered it (e.g. `GET /api/orders`)
- **Suggestion:** e.g. "batch this query", "use a batch endpoint", "consider adding an index"
- **GreenOps impact:** estimated avoidable I/O ops, I/O Intensity Score, structured `co2` object (`low`/`mid`/`high`, SCI v1.0 operational + embodied terms), per-region breakdown when multi-region scoring is active

![demo](docs/img/demo.gif)

<details>
<summary>Still frames</summary>

**Configuration** (`.perf-sentinel.toml`):

![config](docs/img/demo-config.png)

**Analysis report:**

![report](docs/img/demo-report.png)

</details>

In CI mode (`perf-sentinel analyze --ci`), the output is a structured JSON report:

<details>
<summary>Example JSON report</summary>

```json
{
  "analysis": {
    "duration_ms": 1,
    "events_processed": 6,
    "traces_analyzed": 1
  },
  "findings": [
    {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "game",
      "source_endpoint": "POST /api/game/42/start",
      "pattern": {
        "template": "SELECT * FROM player WHERE game_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0
      }
    }
  ],
  "green_summary": {
    "total_io_ops": 6,
    "avoidable_io_ops": 5,
    "io_waste_ratio": 0.833,
    "top_offenders": [
      {
        "endpoint": "POST /api/game/42/start",
        "service": "game",
        "io_intensity_score": 6.0,
        "co2_grams": 0.000054
      }
    ],
    "co2": {
      "total":     { "low": 0.000519, "mid": 0.001038, "high": 0.002076, "model": "io_proxy_v1", "methodology": "sci_v1_numerator" },
      "avoidable": { "low": 0.000016, "mid": 0.000032, "high": 0.000064, "model": "io_proxy_v1", "methodology": "sci_v1_operational_ratio" },
      "operational_gco2": 0.000038,
      "embodied_gco2":    0.001
    },
    "regions": [
      { "region": "eu-west-3", "grid_intensity_gco2_kwh": 56.0, "pue": 1.135, "io_ops": 6, "co2_gco2": 0.000038 }
    ]
  },
  "quality_gate": {
    "passed": false,
    "rules": [
      { "rule": "n_plus_one_sql_critical_max", "threshold": 0.0, "actual": 0.0, "passed": true },
      { "rule": "n_plus_one_http_warning_max", "threshold": 3.0, "actual": 0.0, "passed": true },
      { "rule": "io_waste_ratio_max", "threshold": 0.3, "actual": 0.833, "passed": false }
    ]
  }
}
```

</details>

## Getting Started

### Install from crates.io

```bash
cargo install sentinel-cli
```

### Download a prebuilt binary

Binaries for Linux (amd64, arm64), macOS (arm64) and Windows (amd64) are available on the [GitHub Releases](https://github.com/robintra/perf-sentinel/releases) page. macOS Intel users can run the arm64 binary via Rosetta 2.

```bash
# Example: Linux amd64
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64
sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel
```

### Run with Docker

```bash
docker run --rm -p 4317:4317 -p 4318:4318 ghcr.io/robintra/perf-sentinel:latest
```

### Quick demo

```bash
perf-sentinel demo
```

### Batch analysis (CI)

```bash
perf-sentinel analyze --input traces.json --ci
```

### Explain a trace

```bash
perf-sentinel explain --input traces.json --trace-id abc123
```

### SARIF export (GitHub/GitLab code scanning)

```bash
perf-sentinel analyze --input traces.json --format sarif
```

### Import from Jaeger or Zipkin

```bash
# Jaeger JSON export (auto-detected)
perf-sentinel analyze --input jaeger-export.json

# Zipkin JSON v2 (auto-detected)
perf-sentinel analyze --input zipkin-traces.json
```

### pg_stat_statements analysis

```bash
# Analyze PostgreSQL pg_stat_statements export for SQL hotspots
perf-sentinel pg-stat --input pg_stat.csv

# Cross-reference with trace findings
perf-sentinel pg-stat --input pg_stat.csv --traces traces.json
```

### Interactive inspection (TUI)

```bash
perf-sentinel inspect --input traces.json
```

### Streaming mode (daemon)

```bash
perf-sentinel watch
```

## Architecture

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/diagrams/svg/pipeline_dark.svg">
  <img alt="Pipeline architecture" src="docs/diagrams/svg/pipeline.svg">
</picture>

## Deployment topologies

perf-sentinel supports three deployment models. Pick the one that fits your environment.

### 1. CI batch analysis (recommended starting point)

Analyze pre-collected trace files in your CI/CD pipeline. The process exits with code 1 if the quality gate fails.

```bash
# In your CI job:
perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
```

Create a `.perf-sentinel.toml` at your project root:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # zero tolerance for N+1 SQL
io_waste_ratio_max = 0.30          # max 30% avoidable I/O

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500

[green]
enabled = true
default_region = "eu-west-3"                  # optional: enables gCO2eq conversion
embodied_carbon_per_request_gco2 = 0.001      # SCI v1.0 M term, default 0.001 g/req

# Optional per-service overrides for multi-region deployments
# (used when OTel cloud.region is absent from spans):
# [green.service_regions]
# "order-svc" = "us-east-1"
# "chat-svc"  = "ap-southeast-1"
```

Output formats: `--format text` (colored, default), `--format json` (structured), `--format sarif` (GitHub/GitLab code scanning).

### 2. Central collector (recommended for production)

An [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) receives traces from all services and forwards them to perf-sentinel. Zero code changes in your services.

```
app-1 --\
app-2 ---+--> OTel Collector --> perf-sentinel (watch)
app-3 --/
```

Ready-to-use files are provided in [`examples/`](examples/):

```bash
# Start the collector + perf-sentinel
docker compose -f examples/docker-compose-collector.yml up -d

# Point your apps at the collector:
#   OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317
```

perf-sentinel streams findings as NDJSON to stdout and exposes Prometheus metrics with [Grafana Exemplars](docs/INTEGRATION.md) at `/metrics` (port 4318).

See [`examples/otel-collector-config.yaml`](examples/otel-collector-config.yaml) for the full collector config with sampling and filtering options.

### 3. Sidecar (per-service diagnostics)

perf-sentinel runs alongside a single service, sharing its network namespace. Useful for isolated debugging.

```bash
docker compose -f examples/docker-compose-sidecar.yml up -d
```

The app sends traces to `localhost:4317` (no network hop). See [`examples/docker-compose-sidecar.yml`](examples/docker-compose-sidecar.yml).

---

For language-specific OTLP instrumentation (Java, .NET, Rust), see [docs/INTEGRATION.md](docs/INTEGRATION.md). For the full configuration reference, see [docs/CONFIGURATION.md](docs/CONFIGURATION.md). For in-depth design documentation, see [docs/design/](docs/design/00-INDEX.md).

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).

