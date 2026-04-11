# Design documentation index

This directory contains deep design documentation for perf-sentinel. These documents explain **why** each decision was made, not just what the code does. They are intended for contributors and maintainers who need to understand the rationale behind the implementation.

For user-facing documentation, see the parent `docs/` directory:
- [ARCHITECTURE.md](../ARCHITECTURE.md): pipeline overview and module responsibilities
- [CONFIGURATION.md](../CONFIGURATION.md): full `.perf-sentinel.toml` reference
- [LIMITATIONS.md](../LIMITATIONS.md): known trade-offs
- [INTEGRATION.md](../INTEGRATION.md): OTLP setup guides (Java, .NET, Rust)

## Table of contents

| Document                                                         | Topics                                                                                              |
|------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------|
| [01: Pipeline and Types](01-PIPELINE-AND-TYPES.md)               | Pipeline vs hexagonal architecture, type chain, workspace split, deterministic output, quality gate |
| [02: Normalization](02-NORMALIZATION.md)                         | SQL state machine, HTTP normalizer, micro-optimizations (batch push, IN-list skip, hand-coded UUID) |
| [03: Correlation and Streaming](03-CORRELATION-AND-STREAMING.md) | Batch HashMap grouping, LRU cache, ring buffer, TTL eviction, memory budget                         |
| [04: Detection](04-DETECTION.md)                                 | N+1, redundant and slow detection algorithms, borrowed keys, iterator-based window                  |
| [05: GreenOps and Carbon](05-GREENOPS-AND-CARBON.md)             | IIS formula, waste ratio dedup, CO2 conversion, SCI alignment                                       |
| [06: Ingestion and Daemon](06-INGESTION-AND-DAEMON.md)           | OTLP conversion, daemon event loop, sampling, security hardening                                    |
| [07: CLI, Config and Release](07-CLI-CONFIG-RELEASE.md)          | Bench command, config parsing, release profile, distribution                                        |

## Source file mapping

| Source File                | Design Doc                                         |
|----------------------------|----------------------------------------------------|
| `lib.rs`                   | [01: Pipeline](01-PIPELINE-AND-TYPES.md)           |
| `event.rs`                 | [01: Pipeline](01-PIPELINE-AND-TYPES.md)           |
| `pipeline.rs`              | [01: Pipeline](01-PIPELINE-AND-TYPES.md)           |
| `quality_gate.rs`          | [01: Pipeline](01-PIPELINE-AND-TYPES.md)           |
| `normalize/sql.rs`         | [02: Normalization](02-NORMALIZATION.md)           |
| `normalize/http.rs`        | [02: Normalization](02-NORMALIZATION.md)           |
| `normalize/mod.rs`         | [02: Normalization](02-NORMALIZATION.md)           |
| `correlate/mod.rs`         | [03: Correlation](03-CORRELATION-AND-STREAMING.md) |
| `correlate/window.rs`      | [03: Correlation](03-CORRELATION-AND-STREAMING.md) |
| `detect/mod.rs`            | [04: Detection](04-DETECTION.md)                   |
| `detect/n_plus_one.rs`     | [04: Detection](04-DETECTION.md)                   |
| `detect/redundant.rs`      | [04: Detection](04-DETECTION.md)                   |
| `detect/slow.rs`           | [04: Detection](04-DETECTION.md)                   |
| `score/mod.rs`             | [05: GreenOps](05-GREENOPS-AND-CARBON.md)          |
| `score/carbon.rs`          | [05: GreenOps](05-GREENOPS-AND-CARBON.md)          |
| `ingest/mod.rs`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)        |
| `ingest/json.rs`           | [06: Ingestion](06-INGESTION-AND-DAEMON.md)        |
| `ingest/otlp.rs`           | [06: Ingestion](06-INGESTION-AND-DAEMON.md)        |
| `ingest/pg_stat.rs`        | [06: Ingestion](06-INGESTION-AND-DAEMON.md)        |
| `daemon.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)        |
| `config.rs`                | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)         |
| `report/mod.rs`, `json.rs` | [01: Pipeline](01-PIPELINE-AND-TYPES.md)           |
| `report/metrics.rs`        | [06: Ingestion](06-INGESTION-AND-DAEMON.md)        |
| `sentinel-cli/src/main.rs` | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)         |
| `sentinel-cli/src/tui.rs`  | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)         |
