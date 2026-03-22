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

Analyzes runtime traces (SQL queries, HTTP calls) to detect N+1 queries, redundant calls, and scores I/O intensity per endpoint (GreenOps).

## Why perf-sentinel?

Performance anti-patterns like N+1 queries exist in any application that does I/O — monoliths and microservices alike. In distributed architectures, a single user request cascades across multiple services, each with its own I/O, and nobody has visibility on the full path. Existing tools are either runtime-specific (Hypersistence Utils -> JPA only), heavy and proprietary (Datadog, New Relic), or limited to unit tests without cross-service visibility.

perf-sentinel takes a different approach: **protocol-level analysis**. It observes the traces your application produces (SQL queries, HTTP calls) regardless of language or ORM. It doesn't need to understand JPA, EF Core, or SeaORM — it sees the queries they generate.

## GreenOps: built-in carbon-aware scoring

Every finding includes an **I/O Intensity Score (IIS)**: the number of I/O operations generated per user request for a given endpoint. Reducing unnecessary I/O (N+1 queries, redundant calls) improves response times *and* reduces energy consumption — these are not competing goals.

- **I/O Intensity Score** = total I/O ops for an endpoint / number of invocations
- **I/O Waste Ratio** = avoidable I/O ops (from findings) / total I/O ops

Aligned with the **Energy** component of the [SCI model (ISO/IEC 21031:2024)](https://github.com/Green-Software-Foundation/sci) from the Green Software Foundation.

## How does it compare?

| Criteria           | [Hypersistence Utils](https://github.com/vladmihalcea/hypersistence-utils) | [Datadog APM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Digma](https://digma.ai/) | **perf-sentinel** |
|--------------------|----------------------------------------------------------------------------|-------------------------------------------------------|-----------------------------------------------------------------------|----------------------------|-------------------|
| N+1 SQL detection  | ✅ JPA only                                                                 | ✅ (runtime)                                           | ✅ (runtime)                                                           | ✅ (JVM)                    | ✅ Polyglot        |
| N+1 HTTP detection | ❌                                                                          | ✅                                                     | ✅                                                                     | ⚠️ Partial                 | ✅                 |
| Polyglot           | ❌ Java/JPA                                                                 | ✅ (per-language agents)                               | ✅ (per-language agents)                                               | ❌ JVM                      | ✅ Protocol-level  |
| Cross-service      | ❌                                                                          | ✅                                                     | ✅                                                                     | ⚠️ Partial                 | ✅ Trace ID        |
| GreenOps / SCI     | ❌                                                                          | ❌                                                     | ❌                                                                     | ❌                          | ✅ Built-in        |
| Lightweight        | N/A (lib)                                                                  | ❌ (~150 MB)                                           | ❌ (~150 MB)                                                           | ❌ (~100 MB)                | ✅ (<10 MB RSS)    |
| Open-source        | ✅ MIT                                                                      | ❌                                                     | ⚠️ Limited free tier                                                  | ⚠️ Freemium                | ✅ AGPL v3         |
| CI/CD quality gate | ⚠️ (manual assertions)                                                     | ❌                                                     | ⚠️ (alerts, no native gate)                                           | ⚠️                         | ✅ Native          |

## What does it report?

For each detected anti-pattern, perf-sentinel reports:

- **Type:** N+1 SQL, N+1 HTTP, or redundant query
- **Normalized template:** the query or URL with parameters replaced by placeholders (`?`, `{id}`)
- **Occurrences:** how many times the pattern fired within the detection window
- **Source endpoint:** which application endpoint triggered it (e.g. `GET /api/orders`)
- **Suggestion:** e.g. "batch this query" or "use a batch endpoint"
- **GreenOps impact:** estimated avoidable I/O ops and I/O Intensity Score

```
$ perf-sentinel demo

=== perf-sentinel demo ===
Analyzed 14 events across 2 traces in 1ms

Found 2 issue(s):

  [WARNING] #1 N+1 SQL
    Trace:    trace-demo-game
    Service:  game
    Endpoint: POST /api/game/42/start
    Template: SELECT * FROM player WHERE game_id = ?
    Hits:     6 occurrences, 6 distinct params, 250ms window
    Suggestion: Use WHERE ... IN (?) to batch 6 queries into one
    Extra I/O: 5 avoidable ops
    IIS:      12.0

  [WARNING] #2 N+1 HTTP
    Trace:    trace-demo-game
    Service:  game
    Endpoint: POST /api/game/42/start
    Template: GET /api/account/{id}
    Hits:     6 occurrences, 6 distinct params, 250ms window
    Suggestion: Use batch endpoint with ?ids=... to batch 6 calls into one
    Extra I/O: 5 avoidable ops
    IIS:      12.0

--- GreenOps Summary ---
  Total I/O ops:     14
  Avoidable I/O ops: 10
  I/O waste ratio:   71.4%

  Top offenders:
    - POST /api/game/42/start: IIS 12.0, 12.0 I/O ops/req (service: game)
    - GET /api/users/1: IIS 2.0, 2.0 I/O ops/req (service: user-svc)

Quality gate: PASSED
```

In batch/CI mode (`perf-sentinel analyze`), the output is a structured JSON report:

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
        "io_ops_per_request": 6.0
      }
    ]
  },
  "quality_gate": {
    "passed": true,
    "rules": []
  }
}
```

</details>

## Getting Started

> Coming soon.

## Roadmap

| Phase | Description                                          | Status        |
|-------|------------------------------------------------------|---------------|
| **0** | Scaffolding: compilable workspace, CI, stubs         | ✅ Done        |
| **1** | N+1 SQL + HTTP detection, normalization, correlation | ✅ Done        |
| **2** | GreenOps scoring, OTLP ingestion, CI quality gate    | ⏳ In progress |
| **3** | Polish, benchmarks, v0.1.0 release                   | Not started   |

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).

