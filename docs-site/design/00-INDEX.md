# Design documentation index

Deep design documentation for perf-sentinel. These documents explain **why** each decision was made, not just what the code does. They are intended for contributors and maintainers who need to understand the rationale behind the implementation.

For user-facing documentation, see the [Documentation index](../00-INDEX.md).

## Table of contents

| Document                                                         | Topics                                                                                                                                                                                             |
|------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [01: Pipeline and Types](01-PIPELINE-AND-TYPES.md)               | Pipeline vs hexagonal architecture, type chain, workspace split, deterministic output, quality gate                                                                                                |
| [02: Normalization](02-NORMALIZATION.md)                         | SQL state machine, HTTP normalizer, micro-optimizations (batch push, IN-list skip, hand-coded UUID)                                                                                                |
| [03: Correlation and Streaming](03-CORRELATION-AND-STREAMING.md) | Batch HashMap grouping, LRU cache, ring buffer, TTL eviction, memory budget                                                                                                                        |
| [04: Detection](04-DETECTION.md)                                 | N+1, redundant and slow detection algorithms, borrowed keys, iterator-based window, cross-trace correlation, suggested fixes keyed by framework or broker                                          |
| [05: GreenOps and Carbon](05-GREENOPS-AND-CARBON.md)             | IIS formula, waste ratio dedup, CO2 conversion, SCI alignment, database and broker energy attribution                                                                                              |
| [06: Ingestion and Daemon](06-INGESTION-AND-DAEMON.md)           | OTLP conversion, messaging admission and producer-link resolution, daemon event loop, sampling, security hardening, query API, Prometheus pg_stat                                                  |
| [07: CLI, Config and Release](07-CLI-CONFIG-RELEASE.md)          | Bench, query, report, diff subcommands. HTML dashboard sink, CSV export, deep-link hash, cheatsheet modal, vim-style tab shortcuts. Config parsing, release profile, distribution, source location |
| [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)             | Schema determinism through v1.5, G1/G2 granularity, collect-all validator, per-service attribution, measured/declared/estimated provenance, daemon archive writer, `disclose` CLI dispatcher       |
| [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)               | Per-service energy + carbon at scoring time, region attribution, model precedence, aggregator runtime-vs-proxy branching                                                                           |
| [10: Sigstore and SLSA](10-SIGSTORE-ATTESTATION.md)              | In-toto v1 predicate, Sigstore cosign signature flow, SLSA Build L3 build provenance, `verify-hash` chain, privacy on Rekor public                                                                 |

## Source file mapping

| Source File                    | Design Doc                                                                                       |
|--------------------------------|--------------------------------------------------------------------------------------------------|
| `lib.rs`                       | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `event.rs`                     | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `pipeline.rs`                  | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `quality_gate.rs`              | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `acknowledgments.rs`           | [04: Detection](04-DETECTION.md)                                                                 |
| `calibrate.rs`                 | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `shutdown.rs`                  | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `text_safety.rs`               | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `explain.rs`                   | [06: Ingestion](06-INGESTION-AND-DAEMON.md), [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)          |
| `diff.rs`                      | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `synth.rs`                     | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `http_client.rs`               | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `time.rs`                      | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
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
| `detect/fanout.rs`             | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/chatty.rs`             | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/pool_saturation.rs`    | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/serialized.rs`         | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/sanitizer_aware.rs`    | [04: Detection](04-DETECTION.md)                                                                 |
| `detect/suggestions/`          | [04: Detection](04-DETECTION.md)                                                                 |
| `score/mod.rs`                 | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/carbon.rs`              | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/carbon_compute.rs`      | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/region_breakdown.rs`    | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/carbon_profiles.rs`     | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/energy_state.rs`        | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/prom_parser.rs`         | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/ops_snapshot_diff.rs`   | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/broker_static.rs`       | [05: GreenOps](05-GREENOPS-AND-CARBON.md)                                                        |
| `score/canonical.rs`           | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `score/alumet/`                | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/scaphandre/`            | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/kepler/`                | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/redfish/`               | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/cloud_energy/`          | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `score/electricity_maps/`      | [05: GreenOps](05-GREENOPS-AND-CARBON.md), [09: Carbon Attribution](09-CARBON-ATTRIBUTION.md)    |
| `ingest/mod.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/json.rs`               | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/otlp/`                 | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/pg_stat.rs`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/jaeger.rs`             | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/zipkin.rs`             | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/tempo.rs`              | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/jaeger_query.rs`       | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/auth_header.rs`        | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/mysql_stat.rs`         | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/lookback.rs`           | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `ingest/url_enc.rs`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/mod.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/event_loop.rs`         | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/listeners.rs`          | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/tls.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/json_socket.rs`        | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/sampling.rs`           | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/findings_store.rs`     | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/query_api/`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/ack.rs`                | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `daemon/mem_pressure.rs`       | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `config/` (mod, raw, validate) | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md), [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md) |
| `report/mod.rs`, `json.rs`     | [01: Pipeline](01-PIPELINE-AND-TYPES.md)                                                         |
| `report/html/`                 | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `report/metrics.rs`            | [06: Ingestion](06-INGESTION-AND-DAEMON.md)                                                      |
| `report/interpret.rs`          | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `report/warnings.rs`           | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `report/sarif.rs`              | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `report/periodic/*`            | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `daemon/archive.rs`            | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `sentinel-cli/src/main.rs`     | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
| `sentinel-cli/src/disclose.rs` | [08: Periodic Disclosure](08-PERIODIC-DISCLOSURE.md)                                             |
| `sentinel-cli/src/tui/`        | [07: CLI/Config](07-CLI-CONFIG-RELEASE.md)                                                       |
