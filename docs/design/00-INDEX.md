# Design documentation index

This directory contains deep design documentation for perf-sentinel. These documents explain **why** each decision was made, not just what the code does. They are intended for contributors and maintainers who need to understand the rationale behind the implementation.

For user-facing documentation, see the parent `docs/` directory:
- [ARCHITECTURE.md](../ARCHITECTURE.md): pipeline overview and module responsibilities
- [CONFIGURATION.md](../CONFIGURATION.md): full `.perf-sentinel.toml` reference
- [LIMITATIONS.md](../LIMITATIONS.md): known trade-offs
- [INTEGRATION.md](../INTEGRATION.md): topology overview and quick starts
- [INSTRUMENTATION.md](../INSTRUMENTATION.md): OTLP setup guides (Java, Quarkus, .NET, Rust)
- [CI.md](../CI.md): CI mode, GitHub Actions / GitLab CI / Jenkins recipes

## Table of contents

| Document                                                         | Topics                                                                                                                                                                                             |
|------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [01: Pipeline and Types](01-PIPELINE-AND-TYPES.md)               | Pipeline vs hexagonal architecture, type chain, workspace split, deterministic output, quality gate                                                                                                |
| [02: Normalization](02-NORMALIZATION.md)                         | SQL state machine, HTTP normalizer, micro-optimizations (batch push, IN-list skip, hand-coded UUID)                                                                                                |
| [03: Correlation and Streaming](03-CORRELATION-AND-STREAMING.md) | Batch HashMap grouping, LRU cache, ring buffer, TTL eviction, memory budget                                                                                                                        |
| [04: Detection](04-DETECTION.md)                                 | N+1, redundant and slow detection algorithms, borrowed keys, iterator-based window, cross-trace correlation                                                                                        |
| [05: GreenOps and Carbon](05-GREENOPS-AND-CARBON.md)             | IIS formula, waste ratio dedup, CO2 conversion, SCI alignment                                                                                                                                      |
| [06: Ingestion and Daemon](06-INGESTION-AND-DAEMON.md)           | OTLP conversion, daemon event loop, sampling, security hardening, query API, Prometheus pg_stat                                                                                                    |
| [07: CLI, Config and Release](07-CLI-CONFIG-RELEASE.md)          | Bench, query, report, diff subcommands. HTML dashboard sink, CSV export, deep-link hash, cheatsheet modal, vim-style tab shortcuts. Config parsing, release profile, distribution, source location |
| [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)             | Schema v1.0 determinism, G1/G2 granularity, collect-all validator, per-service attribution, daemon archive writer, `disclose` CLI dispatcher                                                       |
| [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)               | Per-service energy + carbon at scoring time, region attribution, model precedence, aggregator runtime-vs-proxy branching                                                                           |
| [10: Sigstore and SLSA](10-SIGSTORE-ATTESTATION.md)              | In-toto v1 predicate, Sigstore cosign signature flow, SLSA Build L3 build provenance, `verify-hash` chain, privacy on Rekor public                                                                 |

## Source file mapping

| Source File                    | Design Doc                                                                                       |
|--------------------------------|--------------------------------------------------------------------------------------------------|
| `lib.rs`                       | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `event.rs`                     | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `pipeline.rs`                  | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `quality_gate.rs`              | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `normalize/sql.rs`             | [02: Normalization](02-NORMALIZATION.md)                                                         |
| `normalize/http.rs`            | [02: Normalization](02-NORMALIZATION.md)                                                         |
| `normalize/mod.rs`             | [02: Normalization](02-NORMALIZATION.md)                                                         |
| `correlate/mod.rs`             | [03: Correlation](03-CORRELATION-AND-STREAMING.md)                                               |
| `correlate/window.rs`          | [03: Correlation](03-CORRELATION-AND-STREAMING.md)                                               |
| `detect/mod.rs`                | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/n_plus_one.rs`         | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/redundant.rs`          | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/slow.rs`               | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/correlate_cross.rs`    | [04: Detection](04-DETECTION.md)                                                                 |
| `score/mod.rs`                 | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/carbon.rs`              | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/carbon_compute.rs`      | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/region_breakdown.rs`    | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `ingest/mod.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/json.rs`               | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/otlp.rs`               | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/pg_stat.rs`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/mod.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/event_loop.rs`         | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/listeners.rs`          | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/tls.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/json_socket.rs`        | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/sampling.rs`           | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/findings_store.rs`     | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/query_api.rs`          | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `config.rs`                    | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md), [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md) |
| `report/mod.rs`, `json.rs`     | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `report/metrics.rs`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `report/periodic/*`            | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `daemon/archive.rs`            | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `sentinel-cli/src/main.rs`     | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `sentinel-cli/src/disclose.rs` | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `sentinel-cli/src/tui.rs`      | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
