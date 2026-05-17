<p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frobintra%2Fperf-sentinel%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&suffix=%20stable&label=rust%202024&color=D34516&logo=rust" alt="Rust" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml/badge.svg" alt="Security Audit" /></a>
    <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
    <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/release.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/release.yml/badge.svg" alt="Release" /></a>
    <a href="https://artifacthub.io/packages/helm/perf-sentinel/perf-sentinel"><img src="https://img.shields.io/endpoint?url=https://artifacthub.io/badge/repository/perf-sentinel" alt="Artifact Hub" /></a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-dark-horizontal.svg">
  <img alt="perf-sentinel" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-horizontal.svg">
</picture>

**A lightweight, polyglot CLI that turns OpenTelemetry traces into a CI quality gate against I/O anti-patterns (N+1 queries, redundant calls, slow SQL/HTTP, excessive fanout, ...), with an I/O intensity score that doubles as a *directional* GreenOps signal.**

> **Read this first**
> - **Prerequisite:** your services must emit **OpenTelemetry traces** (SQL + HTTP spans). If they don't, perf-sentinel has nothing to analyze. See [docs/INSTRUMENTATION.md](docs/INSTRUMENTATION.md) for language-specific setup (Java/Quarkus/.NET/Rust).
> - **What it is:** a self-hosted, single-binary (`<15 MB RSS`) anti-pattern detector, runnable in batch mode on captured traces (local exploration, post-mortem, or a CI quality gate that exits 1 on threshold breach) or as a long-running daemon (OTLP ingestion, query API, live dashboard, Prometheus metrics).
> - **What it is *not*:** a full APM, a continuous profiler, or a standalone regulatory carbon accounting platform. See [What perf-sentinel is not](#what-perf-sentinel-is-not).

---

## Quick look

Terminal:

```bash
perf-sentinel analyze --input traces.json
```

![demo](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/demo.gif)

HTML dashboard (single offline file):

```bash
perf-sentinel report --input traces.json --output report.html
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/dashboard_dark.gif">
  <img alt="dashboard tour" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/dashboard_light.gif">
</picture>

## Why perf-sentinel?

Performance anti-patterns like N+1 queries exist in any application that does I/O, monoliths and microservices alike. In distributed architectures, a single user request cascades across multiple services, each with its own I/O, and nobody has visibility on the full path.

Existing tools each solve part of the problem: Hypersistence Utils covers JPA only, Datadog and New Relic are heavy proprietary agents you may not want in every pipeline, Sentry's detectors are solid but tied to its SDK and backend. None of them give you a **protocol-level CI gate you can self-host**.

perf-sentinel observes the traces your application already emits (SQL queries, HTTP calls) regardless of language or ORM. It doesn't need to understand JPA, EF Core or SeaORM, it sees the queries they generate.

## What it detects

Ten finding types, plus cross-trace correlations in daemon mode:

| Pattern             | Trigger                                                          |
|---------------------|------------------------------------------------------------------|
| N+1 SQL             | Same query template fired ≥ N times in a single trace            |
| N+1 HTTP            | Same URL template called ≥ N times in a single trace             |
| Redundant SQL       | Identical query with identical params, same trace                |
| Redundant HTTP      | Identical call with identical params, same trace                 |
| Slow SQL            | Query duration above configured threshold                        |
| Slow HTTP           | Request duration above configured threshold                      |
| Excessive fanout    | One span starts ≥ N children in parallel                         |
| Chatty service      | Service A → B repeatedly within one user request                 |
| Pool saturation     | Concurrent in-flight queries exceed configured pool size         |
| Serialized calls    | Sequential I/O that could be parallelized                        |

Each finding carries: type, severity, normalized template, occurrences, source endpoint, suggestion, source location (when OTel spans carry `code.*` attributes), and GreenOps impact (see below). For per-detector severity rules and tunable thresholds, see [docs/design/04-DETECTION.md](docs/design/04-DETECTION.md).

## Output formats

- **`text`** (default): severity-grouped colored terminal output. Available on `analyze`, `diff`, `pg-stat`, `query`, `explain`, `ack`.
- **`json`**: structured report. Available on `analyze`, `diff`, `pg-stat`, `query`, `explain`, `ack`. Full schema in [docs/SCHEMA.md](docs/SCHEMA.md), example fixtures in [docs/schemas/examples/](docs/schemas/examples/).
- **`sarif`** (SARIF v2.1.0): GitHub/GitLab code scanning with inline PR annotations via `physicalLocations`. Available on `analyze` and `diff`. See [docs/SARIF.md](docs/SARIF.md).
- **HTML dashboard**: single-file offline report from `perf-sentinel report`, click-through trace trees, dark/light theme, CSV export from Findings / pg_stat / Diff / Correlations tabs. See [docs/HTML-REPORT.md](docs/HTML-REPORT.md).
- **Interactive TUI**: 3-panel keyboard-driven view from `perf-sentinel inspect` (or `query inspect` for live daemon data). See [docs/INSPECT.md](docs/INSPECT.md).
- **Live daemon**: NDJSON findings on stdout, Prometheus `/metrics` with Grafana Exemplars, `/health` probe, HTTP query API. See [docs/METRICS.md](docs/METRICS.md) and [docs/QUERY-API.md](docs/QUERY-API.md).
- **Periodic disclosure (optional)**: hash-verifiable `perf-sentinel-report/v1.0` JSON from `perf-sentinel disclose`, signable via Sigstore. See [docs/REPORTING.md](docs/REPORTING.md).

The JSON `io_intensity_band` / `io_waste_ratio_band` enum values (`healthy` / `moderate` / `high` / `critical`) are stable across versions, numeric thresholds behind them may evolve. Reference table and rationale in [docs/LIMITATIONS.md#score-interpretation](docs/LIMITATIONS.md#score-interpretation).

## Install

```bash
# from crates.io
cargo install perf-sentinel

# or download a prebuilt binary (Linux amd64/arm64, macOS arm64, Windows amd64)
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64 && sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel

# or run via Docker
docker run --rm -p 4317:4317 -p 4318:4318 \
  ghcr.io/robintra/perf-sentinel:latest watch --listen-address 0.0.0.0
```

Linux binaries target musl (fully static, run on any distro and `FROM scratch` images). A Helm chart is available under [`charts/perf-sentinel/`](charts/perf-sentinel/). See [docs/HELM-DEPLOYMENT.md](docs/HELM-DEPLOYMENT.md).

## Deployment

Four environments, three deployment models. Full setup in [docs/INTEGRATION.md](docs/INTEGRATION.md), CI recipes in [docs/CI.md](docs/CI.md), Prometheus metrics in [docs/METRICS.md](docs/METRICS.md), sidecar example in [`examples/docker-compose-sidecar.yml`](examples/docker-compose-sidecar.yml).

Models: **CI batch** (`analyze --ci` on captured traces, exits 1 on threshold breach), **central collector** (OTel Collector forwards to `watch` daemon, Prometheus metrics and query API), **sidecar** (one daemon per service for isolated debugging).

<details>
<summary><b>Local dev</b></summary>

![Local dev zoom-in: batch on captured trace, local daemon at 127.0.0.1, inspect TUI, report HTML](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-local-dev.svg)

</details>

<details>
<summary><b>CI/CD</b></summary>

![CI zoom-in: perf integration tests + analyze --ci quality gate, SARIF for code scanning, optional Tempo / jaeger-query nightly](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-CI.svg)

</details>

<details>
<summary><b>Staging</b></summary>

![Staging zoom-in: focus-service pod with sidecar daemon, /api/findings polled by QA / SRE](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-staging.svg)

</details>

<details>
<summary><b>Production</b></summary>

![Production zoom-in: centralized daemon ingesting via OTel Collector and direct OTLP, /api/* + /metrics + NDJSON](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-production.svg)

</details>

<details>
<summary><b>GreenOps (cross-cutting)</b></summary>

![GreenOps integration: external real-time sources (Scaphandre kWh, Electricity Maps gCO2/kWh) plus internal cold sources (Cloud SPECpower kWh, embodied carbon gCO2e/req via Boavizta + HotCarbon 2024, network transport kWh/GB via Mytton 2024) feeding perf-sentinel in batch or daemon mode, emitting energy and carbon alongside traces](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-GreenOps.svg)

</details>

<details>
<summary>End-to-end view: how the four environments fit together</summary>

![Global perf-sentinel integration across local dev, CI, staging and prod](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/global-integration.svg)

</details>

The companion repo [perf-sentinel-simulation-lab](https://github.com/robintra/perf-sentinel-simulation-lab/blob/main/docs/SCENARIOS.md) validates eight operational modes end to end on a real Kubernetes cluster, each shipping a Mermaid diagram, the exact inputs/outputs, and the gotchas hit during validation.

## Quickstart

```bash
# 1. Try the bundled demo (no setup required)
perf-sentinel demo

# 2. Analyze a captured trace file
perf-sentinel analyze --input traces.json

# 3. Use as a CI quality gate (exits 1 on threshold breach)
perf-sentinel analyze --input traces.json --ci --config .perf-sentinel.toml

# 4. Stream traces from your apps (daemon mode)
perf-sentinel watch
```

Minimal `.perf-sentinel.toml` at the repo root:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # zero tolerance for N+1 SQL
io_waste_ratio_max = 0.30          # max 30% avoidable I/O

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500
```

Full subcommand reference: `perf-sentinel <cmd> --help`, or [docs/CLI.md](docs/CLI.md).

<details>
<summary>Map of the perf-sentinel subcommands and the artifacts they consume or produce</summary>

<img alt="CLI commands overview" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands.svg">

</details>

<details>
<summary>One-liner cheat sheet for the rest of the surface</summary>

```bash
perf-sentinel explain --input traces.json --trace-id abc123        # tree view of one trace
perf-sentinel inspect --input traces.json                          # interactive TUI
perf-sentinel diff --before base.json --after head.json            # PR regression diff
perf-sentinel pg-stat --input pg_stat.csv --traces traces.json     # PostgreSQL hotspots
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id <id>   # pull from Grafana Tempo
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc
perf-sentinel calibrate --traces traces.json --measured-energy rapl.csv
perf-sentinel completions zsh > ~/.zfunc/_perf-sentinel            # shell completions
perf-sentinel query findings --service order-svc                   # talk to a running daemon
```

</details>

## GreenOps: I/O intensity score (directional)

Every finding carries an **I/O intensity score (IIS)**, total I/O ops for an endpoint divided by invocations, and an **I/O waste ratio** (avoidable ops / total ops). Reducing N+1 queries and redundant calls improves response times *and* energy use; these are not competing goals.

`co2.total` is reported as the [Software Carbon Intensity v1.0 / ISO/IEC 21031:2024](https://github.com/Green-Software-Foundation/sci) numerator `(E × I) + M`, summed over analyzed traces. Multi-region scoring is automatic when OTel spans carry `cloud.region`. In daemon mode, energy can be refined via Scaphandre RAPL or cloud-native CPU% + SPECpower, and grid intensity pulled live from Electricity Maps.

> **perf-sentinel is a specialized carbon calculator for software / compute emissions**, with an activity-based methodology, region-hourly grid intensity (Electricity Maps, ENTSO-E, RTE, National Grid ESO, EIA, ...), bottom-up embodied carbon (Boavizta + HotCarbon 2024) and Sigstore-signed, hash-verifiable disclosures.
>
> It is **suitable as a primary data source** for a horizontal carbon accounting platform, or **as an internal controlling tool** for software-emissions KPIs and RGESN conformance.
>
> It is **not yet third-party verified** for standalone CSRD / GHG Protocol Scope 2/3 inventory reporting, which requires audit by a qualified body and integration with non-IT scopes. CO₂ figures carry a `~2×` uncertainty bracket in the default proxy mode (tighter with Scaphandre RAPL or cloud SPECpower + calibration). Methodology, sources and bounds: [docs/LIMITATIONS.md#carbon-estimates-accuracy](docs/LIMITATIONS.md#carbon-estimates-accuracy) and [docs/METHODOLOGY.md](docs/METHODOLOGY.md).

Concrete pairings: pass the I/O counts and per-region energy estimates to **Watershed**, **Sweep**, **Greenly** or **Persefoni** as activity data ; or use perf-sentinel directly to demonstrate **RGESN** (Référentiel Général d'Écoconception de Services Numériques, ARCEP/Ademe/DINUM 2024) software-optimization conformance, where N+1 detection, redundant calls, caching and fanout reduction map onto the corresponding criteria.

For organisations who still want to publish a *non-regulatory* periodic efficiency disclosure (quarterly/yearly JSON, optional Sigstore signature), the optional `perf-sentinel disclose` workflow is documented in [docs/REPORTING.md](docs/REPORTING.md). It is intentionally kept off the main quickstart path.

## How does it compare?

perf-sentinel's niche is being **lightweight, protocol-agnostic, CI/CD-native and carbon-aware**, not replacing a full observability suite.

| Capability                  | [Hypersistence Optimizer](https://vladmihalcea.com/hypersistence-optimizer/) | [Datadog APM + DBM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Sentry](https://sentry.io/for/performance/) | [Digma](https://digma.ai/)   | [Grafana Pyroscope](https://grafana.com/oss/pyroscope/) | **perf-sentinel**                     |
|-----------------------------|------------------------------------------------------------------------------|-------------------------------------------------------------|-----------------------------------------------------------------------|----------------------------------------------|------------------------------|---------------------------------------------------------|---------------------------------------|
| N+1 SQL detection           | JPA only, test-time                                                          | Yes, automatic (DBM)                                        | Yes, automatic                                                        | Yes, automatic OOTB                          | Yes, IDE-centric (JVM/.NET)  | No (CPU/memory profiler, not a query analyzer)          | Yes, protocol-level, any OTel runtime |
| N+1 HTTP detection          | No                                                                           | Yes, service maps                                           | Yes, trace correlation                                                | Yes, N+1 API Call detector                   | Partial                      | No                                                      | Yes                                   |
| Polyglot support            | Java only                                                                    | Per-language agents                                         | Per-language agents                                                   | Per-SDK, most languages                      | JVM + .NET (Rider beta)      | eBPF host-wide + per-language SDKs                      | Any OTel-instrumented runtime         |
| Cross-service correlation   | No                                                                           | Yes                                                         | Yes                                                                   | Yes                                          | Limited (local IDE)          | Trace-to-profile via OTel exemplars                     | Via trace ID                          |
| GreenOps / SCI v1.0 scoring | No                                                                           | No                                                          | No                                                                    | No                                           | No                           | No                                                      | Built-in (directional)                |
| Runtime footprint           | Library (no overhead)                                                        | Agent (~100-150 MB RSS)                                     | Agent (~100-150 MB RSS)                                               | SDK + backend                                | Local backend (Docker)       | Agent + backend (~50-100 MB RSS depending on language)  | Standalone binary (<15 MB RSS)        |
| Native CI/CD quality gate   | Manual test assertions                                                       | Alerts, no build gate                                       | Alerts, no build gate                                                 | Alerts, no build gate                        | No                           | No                                                      | Yes (exit 1 on threshold breach)      |
| License                     | Commercial (Optimizer)                                                       | Proprietary SaaS                                            | Proprietary SaaS                                                      | FSL (converts to Apache-2 after 2y)          | Freemium, proprietary        | AGPL-3.0                                                | AGPL-3.0                              |
| Pricing / self-hostable     | One-time license fee                                                         | Usage-based SaaS (no self-host)                             | Usage-based SaaS (no self-host)                                       | Free tier + SaaS plans (no self-host)        | Freemium SaaS (no self-host) | Free, fully self-hostable                               | Free, fully self-hostable             |

Agent footprint figures for commercial APMs are order-of-magnitude estimates from public deployment reports; actual overhead depends on instrumentation scope.

### What perf-sentinel is not

A fair comparison requires naming what perf-sentinel does **not** do:

- **Not a full APM replacement.** No dashboards, no alerting UI, no RUM, no log aggregation, no distributed profiling. If you need those, Datadog, New Relic and Sentry remain the right tools.
- **Not a continuous profiler.** It observes I/O patterns at the protocol level; it does not sample on-CPU time, allocations or stack traces. For flame graphs and language-aware CPU/memory profiling, [Grafana Pyroscope](https://grafana.com/oss/pyroscope/) is the open-source counterpart and pairs well: pyroscope tells you where compute time goes, perf-sentinel tells you which I/O patterns drive that time.
- **Not a real-time monitoring solution.** Daemon mode streams findings, but the project's center of gravity is CI quality gates and post-hoc trace analysis, not live prod observability.
- **Not a standalone regulatory carbon accounting platform.** perf-sentinel computes activity-based software-emissions numbers from audit-quality sources, but standalone CSRD or GHG Protocol Scope 2/3 reporting requires third-party verification and integration with non-IT scopes it does not cover. Pair it with a horizontal carbon platform (Watershed, Sweep, Greenly, Persefoni, ...) or use it directly for RGESN conformance and internal software-emissions KPIs.
- **Not a replacement for measured energy.** The I/O-to-energy model is an approximation. For accurate per-process power use Scaphandre (supported as an input) or cloud provider energy APIs.
- **Not zero-config.** Protocol-level detection requires OTel instrumentation in your apps. If your stack does not emit traces, perf-sentinel has nothing to analyze.
- **Not an IDE plugin.** For in-IDE feedback on JVM/.NET code as you type, [Digma](https://digma.ai/) offers a well-integrated JetBrains experience.

## Acknowledging known findings

Drop `.perf-sentinel-acknowledgments.toml` at your repo root to suppress findings the team has accepted; they are filtered from `analyze` / `report` / `inspect` / `diff` and do not count toward the quality gate. Runtime acks against a live daemon are exposed via the `ack` CLI, the live HTML dashboard, and the TUI. Full reference: [docs/ACKNOWLEDGMENTS.md](docs/ACKNOWLEDGMENTS.md) and [docs/ACK-WORKFLOW.md](docs/ACK-WORKFLOW.md).

## Documentation

| Topic                                        | Document                                                                               |
|----------------------------------------------|----------------------------------------------------------------------------------------|
| CLI subcommand reference                     | [docs/CLI.md](docs/CLI.md)                                                             |
| Architecture and pipeline                    | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)                                           |
| Integration topologies (CI / prod / sidecar) | [docs/INTEGRATION.md](docs/INTEGRATION.md)                                             |
| OTel instrumentation per language            | [docs/INSTRUMENTATION.md](docs/INSTRUMENTATION.md)                                     |
| CI recipes and PR regression diff            | [docs/CI.md](docs/CI.md)                                                               |
| Full configuration reference                 | [docs/CONFIGURATION.md](docs/CONFIGURATION.md)                                         |
| JSON report schema                           | [docs/SCHEMA.md](docs/SCHEMA.md)                                                       |
| SARIF output                                 | [docs/SARIF.md](docs/SARIF.md)                                                         |
| HTML dashboard                               | [docs/HTML-REPORT.md](docs/HTML-REPORT.md)                                             |
| Interactive TUI                              | [docs/INSPECT.md](docs/INSPECT.md)                                                     |
| Daemon HTTP query API                        | [docs/QUERY-API.md](docs/QUERY-API.md)                                                 |
| Acknowledgments workflow                     | [docs/ACKNOWLEDGMENTS.md](docs/ACKNOWLEDGMENTS.md)                                     |
| GreenOps methodology and limitations         | [docs/METHODOLOGY.md](docs/METHODOLOGY.md), [docs/LIMITATIONS.md](docs/LIMITATIONS.md) |
| Periodic efficiency disclosures (optional)   | [docs/REPORTING.md](docs/REPORTING.md)                                                 |
| Helm deployment                              | [docs/HELM-DEPLOYMENT.md](docs/HELM-DEPLOYMENT.md)                                     |
| Operational runbook                          | [docs/RUNBOOK.md](docs/RUNBOOK.md)                                                     |
| Supply-chain provenance (SLSA, Sigstore)     | [docs/SUPPLY-CHAIN.md](docs/SUPPLY-CHAIN.md)                                           |
| Design notes (deep dive)                     | [docs/design/](docs/design/00-INDEX.md)                                                |

## Supply chain

Every GitHub Action is pinned to a 40-character commit SHA; the production image is `FROM scratch`; `Cargo.lock` is committed and audited daily by `cargo audit`; workflow `GITHUB_TOKEN` permissions default to `contents: read`. Dependabot opens weekly grouped PRs. Release binaries ship SLSA Build L3 provenance (Sigstore + Rekor). Full policy and verification commands: [docs/SUPPLY-CHAIN.md](docs/SUPPLY-CHAIN.md).

## Releasing

Releases follow a documented procedure with a mandatory simulation-lab validation gate. Step-by-step in [docs/RELEASE-PROCEDURE.md](docs/RELEASE-PROCEDURE.md) ([FR](docs/FR/RELEASE-PROCEDURE-FR.md)).

## License

[GNU Affero General Public License v3.0](LICENSE).
