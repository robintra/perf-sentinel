# Architecture

perf-sentinel is a polyglot performance anti-pattern detector built as a Rust workspace with two crates:

- **sentinel-core** : library containing all pipeline logic
- **sentinel-cli** : binary providing the CLI entry point (`perf-sentinel`)

## Pipeline overview

```
                         +-----------+
                         |   Input   |
                         | JSON/OTLP |
                         +-----+-----+
                               |
                         SpanEvent[]
                               |
                        +------v------+
                        |  Normalize  |
                        |  sql / http |
                        +------+------+
                               |
                       NormalizedEvent[]
                               |
                        +------v------+
                        |  Correlate  |
                        | by trace_id |
                        +------+------+
                               |
                           Trace[]
                               |
                        +------v------+
                        |   Detect    |
                        | n+1 / dup / |
                        | slow/fanout |
                        +------+------+
                               |
                          Finding[]
                               |
                        +------v------+
                        |    Score    |
                        |  GreenOps   |
                        |    CO2      |
                        +------+------+
                               |
                   Finding[] + GreenSummary
                               |
                        +------v-------+
                        |   Report     |
                        |JSON/CLI/SARIF|
                        | / Prometheus |
                        +--------------+
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="diagrams/svg/pipeline_dark.svg">
  <img alt="Pipeline architecture" src="diagrams/svg/pipeline.svg">
</picture>

## Operating modes

### Batch mode (`perf-sentinel analyze`)

Processes a complete set of events and produces a single report with quality gate evaluation.

```
Vec<SpanEvent>
  -> normalize::normalize_all()        -> Vec<NormalizedEvent>
  -> correlate::correlate()            -> Vec<Trace>
  -> detect::detect()                  -> Vec<Finding>
  -> score::score_green()              -> (Vec<Finding>, GreenSummary)
  -> quality_gate::evaluate()          -> QualityGate
  -> Report { analysis, findings, green_summary, quality_gate }
```

In CI mode (`--ci`), the process exits with code 1 if the quality gate fails.

### Streaming mode (`perf-sentinel watch`)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="diagrams/svg/daemon_dark.svg">
  <img alt="Daemon architecture" src="diagrams/svg/daemon.svg">
</picture>

Runs as a daemon, receiving events in real time and emitting findings as they are detected.

```
OTLP gRPC (port 4317)  \
OTLP HTTP (port 4318)   +---> mpsc channel ---> TraceWindow (LRU + TTL)
JSON unix socket       /                               |
                                              +--------+--------+
                                              |                 |
                                        LRU eviction    TTL eviction
                                              |                 |
                                              +--------+--------+
                                                       |
                                          normalize -> detect -> score
                                                       |
                                              NDJSON findings (stdout)
                                              Prometheus /metrics
```

- Events are normalized outside the TraceWindow lock to minimize lock hold time.
- Traces are evicted when the LRU cache is full (`max_active_traces`) or when TTL expires (`trace_ttl_ms`).
- On eviction, the trace is analyzed through detect and score stages.
- Findings are emitted as newline-delimited JSON to stdout.
- Prometheus metrics are exposed on the same HTTP port (4318) at `/metrics`.

## Module responsibilities

| Module           | Path              | Responsibility                                                                                                                                                                                                                                                                                                                                                                                            |
|------------------|-------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **event**        | `event.rs`        | Core `SpanEvent` type (SQL and HTTP variants) with timestamp, trace/span IDs, service, operation, target, duration                                                                                                                                                                                                                                                                                        |
| **ingest**       | `ingest/`         | Input sources: JSON parser with auto-format detection (`json.rs`), Jaeger JSON import (`jaeger.rs`), Zipkin JSON v2 import (`zipkin.rs`), OTLP gRPC+HTTP receiver (`otlp.rs`). Implements `IngestSource` trait. PostgreSQL `pg_stat_statements` parser (`pg_stat.rs`) for hotspot analysis                                                                                                                |
| **normalize**    | `normalize/`      | Produces `NormalizedEvent` with template + extracted params. SQL tokenizer (`sql.rs`): replaces literals, UUIDs, IN lists. HTTP normalizer (`http.rs`): replaces numeric/UUID path segments, strips query params                                                                                                                                                                                          |
| **correlate**    | `correlate/`      | Groups events by `trace_id`. Batch mode (`mod.rs`): HashMap aggregation. Streaming mode (`window.rs`): LRU cache with per-trace ring buffer and TTL eviction                                                                                                                                                                                                                                              |
| **detect**       | `detect/`         | Pattern detection on correlated traces. N+1 (`n_plus_one.rs`): same template, different params, within window. Redundant (`redundant.rs`): same template and params. Slow (`slow.rs`): duration above threshold with recurring template. Fanout (`fanout.rs`): parent span with excessive children                                                                                                        |
| **score**        | `score/`          | GreenOps scoring (`mod.rs`): IIS per endpoint, waste ratio, top offenders, green_impact per finding. Multi-region SCI v1.0 carbon (`carbon.rs`): per-region operational CO₂ via OTel `cloud.region` attribute, embodied carbon term, structured `co2` report with confidence intervals, hourly carbon profiles for FR/DE/GB/US-East, optional Scaphandre per-process energy integration (`scaphandre.rs`), cloud-native energy via CPU% + SPECpower interpolation (`cloud_energy/`) |
| **report**       | `report/`         | Output formatting. JSON report (`json.rs`), SARIF v2.1.0 export (`sarif.rs`), colored CLI output (`mod.rs`), Prometheus metrics with OpenMetrics exemplars (`metrics.rs`)                                                                                                                                                                                                                                 |
| **quality_gate** | `quality_gate.rs` | Evaluates configurable threshold rules against findings and green summary                                                                                                                                                                                                                                                                                                                                 |
| **pipeline**     | `pipeline.rs`     | Wires all stages together for batch mode: normalize -> correlate -> detect -> score -> quality_gate -> Report                                                                                                                                                                                                                                                                                             |
| **daemon**       | `daemon.rs`       | Event loop for streaming mode: ingestion servers, mpsc channel, TraceWindow management, eviction processing                                                                                                                                                                                                                                                                                               |
| **config**       | `config.rs`       | Parses `.perf-sentinel.toml` with sectioned format ([thresholds], [detection], [green], [daemon]) and legacy flat format backward compatibility                                                                                                                                                                                                                                                           |
| **time**         | `time.rs`         | Shared timestamp conversion helpers (`nanos_to_iso8601`, `micros_to_iso8601`). Used by OTLP, Jaeger and Zipkin ingestion                                                                                                                                                                                                                                                                                  |
| **explain**      | `explain.rs`      | Trace tree viewer: builds span tree from `parent_span_id`, annotates findings inline. Text and JSON output                                                                                                                                                                                                                                                                                                |

## Key types

| Type              | Module           | Description                                                                                                                                            |
|-------------------|------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| `SpanEvent`       | event            | Raw I/O event (SQL query or HTTP call) with metadata and optional parent_span_id                                                                       |
| `NormalizedEvent` | normalize        | SpanEvent enriched with normalized template and extracted parameters                                                                                   |
| `Trace`           | correlate        | Collection of NormalizedEvents sharing the same trace_id                                                                                               |
| `Finding`         | detect           | Detected anti-pattern with type, severity, pattern details, timestamps, green_impact, and `confidence` (CI batch / daemon staging / daemon production) |
| `GreenSummary`    | score            | Aggregate I/O stats: total ops, avoidable ops, waste ratio, top offenders, optional CO2                                                                |
| `QualityGate`     | quality_gate     | Pass/fail result with individual rule evaluations                                                                                                      |
| `Report`          | report           | Complete analysis output: analysis metadata, findings, green summary, quality gate                                                                     |
| `Config`          | config           | Parsed configuration with all sections and validated fields                                                                                            |
| `TraceWindow`     | correlate/window | LRU cache of active traces for streaming mode with TTL eviction                                                                                        |

## Crate boundaries

```
sentinel-cli (binary)
  |
  +-- clap CLI: analyze / explain / watch / demo / bench subcommands
  |
  +-- depends on sentinel-core (library)
        |
        +-- All pipeline logic
        +-- Traits at borders only: IngestSource, ReportSink
        +-- Pure functions between stages
```

The CLI crate is intentionally thin: it parses arguments, loads config and delegates to sentinel-core functions. All business logic lives in sentinel-core.
