<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-stable-orange?logo=rust" alt="Rust" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
  <a href="https://github.com/robintra/perf-sentinel/blob/main/LICENSE"><img src="https://img.shields.io/github/license/robintra/perf-sentinel?color=blue" alt="License" /></a>
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

## Getting Started

> Coming soon.

## Roadmap

| Phase | Description                                          | Status        |
|-------|------------------------------------------------------|---------------|
| **0** | Scaffolding — compilable workspace, CI, stubs        | ✅ Done        |
| **1** | N+1 SQL + HTTP detection, normalization, correlation | ⏳ In progress |
| **2** | GreenOps scoring, OTLP ingestion, CI quality gate    | Not started   |
| **3** | Polish, benchmarks, v0.1.0 release                   | Not started   |

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).

