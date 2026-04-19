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

Performance anti-patterns like N+1 queries exist in any application that does I/O: monoliths and microservices alike. In distributed architectures, a single user request cascades across multiple services, each with its own I/O and nobody has visibility on the full path. Existing tools are either runtime-specific (Hypersistence Utils -> JPA only), heavy and proprietary (Datadog, New Relic) or limited to unit tests without cross-service visibility.

perf-sentinel takes a different approach: **protocol-level analysis**. It observes the traces your application produces (SQL queries, HTTP calls) regardless of language or ORM. It doesn't need to understand JPA, EF Core or SeaORM, it sees the queries they generate.

## Quick look

```bash
perf-sentinel analyze --input traces.json
```

![demo](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/demo.gif)

## GreenOps: built-in carbon-aware scoring

Every finding includes an **I/O Intensity Score (IIS)**: the number of I/O operations generated per user request for a given endpoint. Reducing unnecessary I/O (N+1 queries, redundant calls) improves response times *and* reduces energy consumption, these are not competing goals.

- **I/O Intensity Score** = total I/O ops for an endpoint / number of invocations
- **I/O Waste Ratio** = avoidable I/O ops (from findings) / total I/O ops

Aligned with the **Software Carbon Intensity** model ([SCI v1.0 / ISO/IEC 21031:2024](https://github.com/Green-Software-Foundation/sci)) from the Green Software Foundation. The `co2.total` field holds the **SCI numerator** `(E × I) + M` summed over analyzed traces, not the per-request intensity score. Multi-region scoring is automatic when OTel spans carry the `cloud.region` attribute. **30+ cloud regions** have embedded hourly carbon intensity profiles, with monthly x hourly seasonal variation for FR, DE, GB and US-East. In daemon mode, energy estimation can be refined via **Scaphandre RAPL** (bare metal) or **cloud-native CPU% + SPECpower** (AWS/GCP/Azure) and grid intensity can be pulled live from the **Electricity Maps API**, with automatic fallback to the I/O proxy model. Users can supply their own hourly profiles via `[green] hourly_profiles_file` or tune the proxy coefficients from on-site measurements via `perf-sentinel calibrate`.

> **Note:** CO₂ estimates are **directional**, not regulatory-grade. Every estimate carries a `~2×` multiplicative uncertainty bracket (`low = mid/2`, `high = mid×2`) because the I/O proxy model is rough. perf-sentinel is a **waste counter**, not a carbon-accounting tool. Do not use it for CSRD or GHG Protocol Scope 3 reporting. See [docs/LIMITATIONS.md](docs/LIMITATIONS.md#carbon-estimates-accuracy) for the full methodology.

## How does it compare?

Trace-based performance anti-pattern detection exists in mature APMs and in several open-source tools. perf-sentinel's niche is being lightweight, protocol-agnostic, CI/CD-native and carbon-aware, not replacing a full observability suite.

| Capability                  | [Hypersistence Optimizer](https://vladmihalcea.com/hypersistence-optimizer/) | [Datadog APM + DBM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Sentry](https://sentry.io/for/performance/) | [Digma](https://digma.ai/)  | **perf-sentinel**                     |
|-----------------------------|------------------------------------------------------------------------------|-------------------------------------------------------------|-----------------------------------------------------------------------|----------------------------------------------|-----------------------------|---------------------------------------|
| N+1 SQL detection           | JPA only, test-time                                                          | Yes, automatic (DBM)                                        | Yes, automatic                                                        | Yes, automatic OOTB                          | Yes, IDE-centric (JVM/.NET) | Yes, protocol-level, any OTel runtime |
| N+1 HTTP detection          | No                                                                           | Yes, service maps                                           | Yes, trace correlation                                                | Yes, N+1 API Call detector                   | Partial                     | Yes                                   |
| Polyglot support            | Java only                                                                    | Per-language agents                                         | Per-language agents                                                   | Per-SDK, most languages                      | JVM + .NET (Rider beta)     | Any OTel-instrumented runtime         |
| Cross-service correlation   | No                                                                           | Yes                                                         | Yes                                                                   | Yes                                          | Limited (local IDE)         | Via trace ID                          |
| GreenOps / SCI v1.0 scoring | No                                                                           | No                                                          | No                                                                    | No                                           | No                          | Built-in (directional)                |
| Runtime footprint           | Library (no overhead)                                                        | Agent (~100-150 MB RSS)                                     | Agent (~100-150 MB RSS)                                               | SDK + backend                                | Local backend (Docker)      | Standalone binary (<10 MB RSS)        |
| Native CI/CD quality gate   | Manual test assertions                                                       | Alerts, no build gate                                       | Alerts, no build gate                                                 | Alerts, no build gate                        | No                          | Yes (exit 1 on threshold breach)      |
| License                     | Commercial (Optimizer)                                                       | Proprietary SaaS                                            | Proprietary SaaS                                                      | FSL (converts to Apache-2 after 2y)          | Freemium, proprietary       | AGPL-3.0                              |

Agent footprint figures for commercial APMs are order-of-magnitude estimates from public deployment reports; actual overhead depends on instrumentation scope.

### What perf-sentinel is not

A fair comparison requires naming what perf-sentinel does not do:

- **Not a full APM replacement.** No dashboards, no alerting UI, no RUM, no log aggregation, no distributed profiling. If you need those, Datadog, New Relic and Sentry remain the right tools.
- **Not a real-time monitoring solution.** Daemon mode streams findings but the project's center of gravity is CI/CD quality gates and post-hoc trace analysis, not live prod observability.
- **Not a regulatory carbon accounting tool.** Use it to spot waste, not to file CSRD or GHG Protocol Scope 3 reports. See the GreenOps note above for methodology bounds.
- **Not a replacement for measured energy.** The I/O-to-energy model is an approximation. For accurate per-process power, use Scaphandre (supported as an input) or cloud provider energy APIs.
- **Not zero-config.** Protocol-level detection requires OTel instrumentation in your apps. If your stack does not emit traces, perf-sentinel has nothing to analyze.
- **Not an IDE plugin.** For in-IDE feedback on JVM/.NET code as you type, [Digma](https://digma.ai/) offers a well-integrated JetBrains experience.

perf-sentinel is a complementary tool focused on one specific problem: detecting I/O anti-patterns in traces, scoring their impact (including carbon) and enforcing thresholds in CI. Use it alongside your existing observability stack, not in place of it.

## What does it report?

For each detected anti-pattern, perf-sentinel reports:

- **Type:** N+1 SQL, N+1 HTTP, redundant query, slow SQL, slow HTTP, excessive fanout, chatty service, pool saturation or serialized calls. Cross-trace correlations are also surfaced in daemon mode
- **Normalized template:** the query or URL with parameters replaced by placeholders (`?`, `{id}`)
- **Occurrences:** how many times the pattern fired within the detection window
- **Source endpoint:** which application endpoint triggered it (e.g. `GET /api/orders`)
- **Suggestion:** e.g. "batch this query", "use a batch endpoint", "consider adding an index"
- **Source location:** when OTel spans carry `code.function`, `code.filepath`, `code.lineno` attributes, findings display the originating source file and line. SARIF reports include `physicalLocations` for inline GitHub/GitLab annotations
- **GreenOps impact:** estimated avoidable I/O ops, I/O Intensity Score, structured `co2` object (`low`/`mid`/`high`, SCI v1.0 operational + embodied terms), per-region breakdown when multi-region scoring is active

You can also drill into a single trace with the `explain` tree view, which annotates findings inline next to the offending spans:

![explain tree view](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/explain/demo.gif)

Or browse traces, findings and span trees interactively with the `inspect` TUI (3-panel layout, keyboard navigation):

![inspect TUI](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/demo.gif)

Or rank SQL hotspots from a PostgreSQL `pg_stat_statements` export with `pg-stat`. Three rankings (by total time, by call count, by mean latency) help you spot queries that dominate the DB without being visible in your traces, a sign of instrumentation gaps:

![pg-stat hotspots](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/pg-stat/demo.gif)

Finally, tune the I/O-to-energy coefficients to your real infrastructure with `calibrate`, which correlates a trace file with measured energy readings (Scaphandre, cloud monitoring, etc.) and emits a TOML file loaded via `[green] calibration_file`:

![calibrate workflow](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/demo.gif)

<details>
<summary>Still frames</summary>

**Configuration** (`.perf-sentinel.toml`):

![config](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/config.png)

**Analysis report** (the first GIF above scrolls through the full report; the four still frames below cover it page by page, with a small overlap so every finding appears fully on at least one page):

![page 1: N+1 SQL, N+1 HTTP, redundant SQL](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-1.png)

![page 2: redundant HTTP, slow SQL, slow HTTP](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-2.png)

![page 3: excessive fanout, chatty service, pool saturation](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-3.png)

![page 4: serialized calls, GreenOps summary, quality gate](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-4.png)

**Explain mode** (tree view of a single trace, `perf-sentinel explain --trace-id <id>`). Span-anchored findings (N+1, redundant, slow, fanout) are rendered inline next to the offending spans; trace-level findings (chatty service, pool saturation, serialized calls) are surfaced in a dedicated header above the tree:

![explain tree view with excessive fanout annotation on the parent span](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/explain/tree.png)

![explain trace-level header with chatty service warning](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/explain/trace-level.png)

**Inspect mode** (interactive TUI, `perf-sentinel inspect`). The findings panel header colors findings by severity; below are five still frames walking the demo fixture across the three severity levels plus a detail-panel view with its scroll feature:

![inspect TUI, initial view: chatty service warning (yellow)](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/main.png)

![inspect TUI, detail panel active: top of the excessive fanout span tree](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/detail.png)

![inspect TUI, detail panel scrolled down: bottom half of the fanout tree](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/detail-scrolled.png)

![inspect TUI, N+1 SQL critical (red): 10 occurrences, batch suggestion](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/critical.png)

![inspect TUI, redundant HTTP info (cyan): 3 identical token validations](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/info.png)

**pg-stat mode** (`perf-sentinel pg-stat --input <pg_stat_statements.csv>`): ranks SQL queries three ways (by total execution time, by call count, by mean latency). Cross-reference with your traces via `--traces` to spot queries that dominate the DB without showing up in instrumentation:

![pg-stat: top hotspots by total time, calls and mean latency](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/pg-stat/hotspots.png)

**Calibrate mode** (`perf-sentinel calibrate --traces <traces.json> --measured-energy <energy.csv>`):

![calibrate input: CSV with per-service power readings](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/csv.png)

![calibrate run: warnings and per-service factors printed](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/run.png)

![calibrate output: generated TOML with calibration factors](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/output.png)

</details>

In CI mode (`perf-sentinel analyze --ci`), the output is a structured JSON report:

<details>
<summary>Example JSON report</summary>

```json
{
  "analysis": {
    "duration_ms": 0,
    "events_processed": 10,
    "traces_analyzed": 1
  },
  "findings": [
    {
      "type": "n_plus_one_sql",
      "severity": "critical",
      "trace_id": "trace-demo-nplus-sql",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "SELECT * FROM order_item WHERE order_id = ?",
        "occurrences": 10,
        "window_ms": 450,
        "distinct_params": 10
      },
      "suggestion": "Use WHERE ... IN (?) to batch 10 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.450Z",
      "green_impact": {
        "estimated_extra_io_ops": 9,
        "io_intensity_score": 10.0,
        "io_intensity_band": "critical"
      },
      "confidence": "ci_batch"
    }
  ],
  "green_summary": {
    "total_io_ops": 10,
    "avoidable_io_ops": 9,
    "io_waste_ratio": 0.9,
    "io_waste_ratio_band": "critical",
    "top_offenders": [
      {
        "endpoint": "POST /api/orders/42/submit",
        "service": "order-svc",
        "io_intensity_score": 10.0,
        "io_intensity_band": "critical"
      }
    ],
    "co2": {
      "total":     { "low": 0.000512, "mid": 0.001024, "high": 0.002048, "model": "io_proxy_v3", "methodology": "sci_v1_numerator" },
      "avoidable": { "low": 0.000011, "mid": 0.000021, "high": 0.000043, "model": "io_proxy_v3", "methodology": "sci_v1_operational_ratio" },
      "operational_gco2": 0.000024,
      "embodied_gco2":    0.001
    },
    "regions": [
      {
        "status": "known",
        "region": "eu-west-3",
        "grid_intensity_gco2_kwh": 42.0,
        "pue": 1.135,
        "io_ops": 10,
        "co2_gco2": 0.000024,
        "intensity_source": "monthly_hourly"
      }
    ]
  },
  "quality_gate": {
    "passed": false,
    "rules": [
      { "rule": "n_plus_one_sql_critical_max", "threshold": 0.0, "actual": 1.0, "passed": false },
      { "rule": "n_plus_one_http_warning_max", "threshold": 3.0, "actual": 0.0, "passed": true },
      { "rule": "io_waste_ratio_max", "threshold": 0.1, "actual": 0.9, "passed": false }
    ]
  }
}
```

</details>

### How to read the report

The CLI renders a `(healthy / moderate / high / critical)` qualifier next to I/O Intensity Score and I/O waste ratio. The same classification ships as sibling fields in the JSON report (`io_intensity_band`, `io_waste_ratio_band`), so downstream tools like SARIF converters, Grafana panels or IDE plugins can consume our heuristics or apply their own on the raw numbers.

| IIS       | Band       | Anchor                                            |
|-----------|------------|---------------------------------------------------|
| < 2.0     | `healthy`  | simple CRUD baseline (≤ 2 I/O per request)        |
| 2.0 - 4.9 | `moderate` | above baseline, worth watching (heuristic)        |
| 5.0 - 9.9 | `high`     | N+1 detector's flag threshold (5 occurrences)     |
| ≥ 10.0    | `critical` | N+1 detector's CRITICAL severity escalation       |

| I/O waste ratio | Band       | Anchor                                       |
|-----------------|------------|----------------------------------------------|
| < 10%           | `healthy`  |                                              |
| 10 - 29%        | `moderate` |                                              |
| 30 - 49%        | `high`     | default `[thresholds] io_waste_ratio_max`    |
| ≥ 50%           | `critical` | majority of analyzed I/O is waste            |

**JSON stability contract:** the enum values above (`healthy` / `moderate` / `high` / `critical`) are stable across versions. The numeric thresholds behind them are versioned with the binary and may evolve. Consumers who want a version-independent classification should read the raw `io_intensity_score` and `io_waste_ratio` fields and apply their own bands.

For per-finding severity (`Critical` / `Warning` / `Info` on each detector type), see [`docs/design/04-DETECTION.md`](docs/design/04-DETECTION.md). For the full rationale behind the interpretation bands, see [`docs/LIMITATIONS.md`](docs/LIMITATIONS.md#score-interpretation).

## Getting Started

### Install from crates.io

```bash
cargo install perf-sentinel
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

# Scrape pg_stat_statements metrics from a postgres_exporter Prometheus endpoint
perf-sentinel pg-stat --prometheus http://prometheus:9090
```

### Interactive inspection (TUI)

```bash
perf-sentinel inspect --input traces.json
```

### Tempo trace ingestion

```bash
# Fetch and analyze a single trace from Grafana Tempo
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123

# Search and analyze recent traces by service name
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 1h
```

### Calibrate energy coefficients

```bash
# Tune I/O-to-energy coefficients from real measurements
perf-sentinel calibrate --traces traces.json --measured-energy rapl.csv --output calibration.toml
```

### Query a running daemon

All query sub-actions default to colored terminal output. Use `--format json` for scripting.

```bash
# List recent findings (colored text by default)
perf-sentinel query findings
perf-sentinel query findings --service order-svc --severity critical

# Explain a trace tree with inline findings
perf-sentinel query explain --trace-id abc123

# Interactive TUI with live daemon data
perf-sentinel query inspect

# View cross-trace correlations
perf-sentinel query correlations

# Check daemon health
perf-sentinel query status

# JSON output for scripting
perf-sentinel query findings --format json
perf-sentinel query status --format json
```

### Streaming mode (daemon)

```bash
perf-sentinel watch
```

## Architecture

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/pipeline_dark.svg">
  <img alt="Pipeline architecture" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/pipeline.svg">
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

perf-sentinel streams findings as NDJSON to stdout and exposes Prometheus metrics with [Grafana Exemplars](docs/INTEGRATION.md) at `/metrics` (port 4318). A `GET /health` liveness endpoint is also exposed on the same port for Kubernetes or load-balancer probes.

See [`examples/otel-collector-config.yaml`](examples/otel-collector-config.yaml) for the full collector config with sampling and filtering options.

### 3. Sidecar (per-service diagnostics)

perf-sentinel runs alongside a single service, sharing its network namespace. Useful for isolated debugging.

```bash
docker compose -f examples/docker-compose-sidecar.yml up -d
```

The app sends traces to `localhost:4317` (no network hop). See [`examples/docker-compose-sidecar.yml`](examples/docker-compose-sidecar.yml).

---

For language-specific OTLP instrumentation (Java, .NET, Rust), see [docs/INTEGRATION.md](docs/INTEGRATION.md). For the full configuration reference, see [docs/CONFIGURATION.md](docs/CONFIGURATION.md). For the daemon HTTP query API (findings, explain, correlations, status), see [docs/QUERY-API.md](docs/QUERY-API.md). For the post-mortem workflow when a trace is older than the daemon's live window, see [docs/RUNBOOK.md](docs/RUNBOOK.md). For in-depth design documentation, see [docs/design/](docs/design/00-INDEX.md).

## Standards and data sources

perf-sentinel's carbon estimates rest on an auditable chain of public standards, reference datasets and peer-reviewed methodology. The authoritative per-reference citation list lives in [`crates/sentinel-core/src/score/carbon.rs`](crates/sentinel-core/src/score/carbon.rs) (module docstring) and in [`crates/sentinel-core/src/score/carbon_profiles.rs`](crates/sentinel-core/src/score/carbon_profiles.rs) (per-region source comments on every profile entry). This section is the narrative companion.

### Standard / specification

- [Software Carbon Intensity v1.0 (ISO/IEC 21031:2024)](https://sci-guide.greensoftware.foundation/), Green Software Foundation. `co2.total` is the SCI v1.0 numerator `(E × I) + M + T`, not the per-R intensity. Full discussion in [docs/design/05-GREENOPS-AND-CARBON.md](docs/design/05-GREENOPS-AND-CARBON.md).

### Reference datasets

- [Cloud Carbon Footprint (CCF)](https://www.cloudcarbonfootprint.org/): annual grid intensity per cloud region, per-provider PUE values (AWS 1.135, GCP 1.10, Azure 1.185, generic 1.2) and the SPECpower coefficient tables (~180 instance types) that feed the `cloud_specpower` energy backend.
- [Electricity Maps](https://www.electricitymaps.com/): annual average intensities for 30+ regions (2023-2024) used as the `io_proxy_v1` baseline, plus the real-time API (`electricity_maps_api` backend, opt-in via `[green.electricity_maps]`).
- [ENTSO-E Transparency Platform](https://transparency.entsoe.eu/): hourly generation and load data used to derive the monthly x hourly profiles for European bidding zones (FR, DE, GB, IE, NL, SE, BE, FI, IT, ES, PL, NO).
- National TSOs and grid operators: [RTE eCO2mix](https://www.rte-france.com/en/eco2mix) (France), [Fraunhofer ISE energy-charts.info](https://www.energy-charts.info/?l=en&c=DE) (Germany), [National Grid ESO Carbon Intensity API](https://carbonintensity.org.uk/) (UK), [EIA Open Data API](https://www.eia.gov/opendata/) for US balancing authorities (PJM, CAISO, BPA), [Hydro-Quebec annual reports](https://www.hydroquebec.com/sustainable-development/) (Canada), [AEMO NEM](https://www.aemo.com.au/) / [OpenNEM](https://opennem.org.au/) (Australia).
- [Scaphandre](https://github.com/hubblo-org/scaphandre): per-process Intel / AMD RAPL power measurement, scraped via its Prometheus endpoint when the `[green.scaphandre]` section is configured.

### Academic methodology

- Xu et al., *Energy-Efficient Query Processing*, VLDB 2010. Foundational DBMS per-operation energy benchmark that motivated the `SELECT 0.5x` / `INSERT 1.5x` / `UPDATE 1.5x` / `DELETE 1.2x` multipliers on the proxy model.
- Tsirogiannis et al., *Analyzing the Energy Efficiency of a Database Server*, SIGMOD 2010. Companion benchmark establishing verb-level coefficients.
- Siddik et al., *DBJoules: Towards Understanding the Energy Consumption of Database Management Systems*, 2023. Confirms 7-38% inter-operation variance across verbs, cross-validation for the `per_operation_coefficients` feature.
- Guo et al., *Energy-efficient Database Systems: A Systematic Survey*, ACM Computing Surveys 2022. Overview of the field.
- IDEAS 2025 framework: real-time energy estimation model for SQL queries, referenced as the direction of travel for future `calibrate` improvements.
- Mytton, Lunden & Malmodin, *Estimating electricity usage of data transmission networks*, Journal of Industrial Ecology 2024. Source for the 0.04 kWh/GB default on the optional `include_network_transport` term; the paper's 0.03-0.06 kWh/GB range is the origin of the configurable `network_energy_per_byte_kwh` field.
- [Boavizta API](https://www.boavizta.org/en/) / HotCarbon 2024: bottom-up server lifecycle embodied carbon model, referenced for the `embodied_per_request_gco2` default calibration.

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).

