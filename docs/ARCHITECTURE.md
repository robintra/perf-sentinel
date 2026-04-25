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
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/pipeline_dark.svg">
  <img alt="Pipeline architecture" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/pipeline.svg">
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
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/daemon_dark.svg">
  <img alt="Daemon architecture" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/daemon.svg">
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

### Daemon query API

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/query-api_dark.svg">
  <img alt="Daemon query API architecture" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/query-api.svg">
</picture>

In `watch` mode, the daemon exposes its internal state via HTTP endpoints on port 4318 alongside `/v1/traces` and `/metrics`:

- `GET /api/findings` (filterable, capped at 1000)
- `GET /api/findings/{trace_id}`
- `GET /api/explain/{trace_id}` (tree with findings inline, from daemon memory)
- `GET /api/correlations` (cross-trace correlations, capped at 1000)
- `GET /api/status` (uptime, active traces, stored findings count)

A `FindingsStore` ring buffer retains recent findings for the API (sized by `[daemon] max_retained_findings`, default 10k). The companion CLI subcommand `perf-sentinel query` renders these endpoints in colored terminal output. Gated by `[daemon] api_enabled` (default true); see [`docs/LIMITATIONS.md`](LIMITATIONS.md#daemon-query-api) for the no-auth threat model.

## Module responsibilities

| Module           | Path              | Responsibility                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
|------------------|-------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **event**        | `event.rs`        | Core `SpanEvent` type (SQL and HTTP variants) with timestamp, trace/span IDs, service, operation, target, duration                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| **ingest**       | `ingest/`         | Input sources: JSON parser with auto-format detection (`json.rs`), Jaeger JSON import (`jaeger.rs`), Zipkin JSON v2 import (`zipkin.rs`), OTLP gRPC+HTTP receiver (`otlp.rs`). Implements `IngestSource` trait. PostgreSQL `pg_stat_statements` parser (`pg_stat.rs`) for hotspot analysis                                                                                                                                                                                                                                                                                                                              |
| **normalize**    | `normalize/`      | Produces `NormalizedEvent` with template + extracted params. SQL tokenizer (`sql.rs`): replaces literals, UUIDs, IN lists. HTTP normalizer (`http.rs`): replaces numeric/UUID path segments, strips query params                                                                                                                                                                                                                                                                                                                                                                                                        |
| **correlate**    | `correlate/`      | Groups events by `trace_id`. Batch mode (`mod.rs`): HashMap aggregation. Streaming mode (`window.rs`): LRU cache with per-trace ring buffer and TTL eviction                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| **detect**       | `detect/`         | Pattern detection on correlated traces. N+1 (`n_plus_one.rs`): same template, different params, within window. Redundant (`redundant.rs`): same template and params. Slow (`slow.rs`): duration above threshold with recurring template. Fanout (`fanout.rs`): parent span with excessive children. Chatty service (`chatty.rs`): too many HTTP outbound calls per trace. Pool saturation (`pool_saturation.rs`): concurrent SQL spans exceeding threshold via sweep-line algorithm. Serialized calls (`serialized.rs`): sequential independent sibling calls that could be parallelized                                |
| **score**        | `score/`          | GreenOps scoring (`mod.rs`): IIS per endpoint, waste ratio, top offenders, green_impact per finding. Carbon pipeline split across `carbon.rs` (embedded grid intensity table, profiles, SCI constants), `carbon_compute.rs` (per-span accumulation loop), `region_breakdown.rs` (region folding, model-tag selection, `CarbonReport` finalization). Optional Scaphandre per-process energy integration (`scaphandre.rs`), cloud-native energy via CPU% + SPECpower interpolation (`cloud_energy/`). Multi-region SCI v1.0 via OTel `cloud.region` attribute, confidence intervals, hourly profiles for FR/DE/GB/US-East |
| **report**       | `report/`         | Output formatting. JSON report (`json.rs`), SARIF v2.1.0 export (`sarif.rs`), colored CLI output (`mod.rs`), Prometheus metrics with OpenMetrics exemplars (`metrics.rs`), single-file HTML dashboard (`html.rs` + bundled `html_template.html`) with Findings, Explain, pg_stat, Diff, Correlations and Green tabs                                                                                                                                                                                                                                                                                                     |
| **quality_gate** | `quality_gate.rs` | Evaluates configurable threshold rules against findings and green summary                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| **pipeline**     | `pipeline.rs`     | Wires all stages together for batch mode: normalize -> correlate -> detect -> score -> quality_gate -> Report                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| **daemon**       | `daemon/`         | Streaming mode: `mod.rs` hosts `DaemonError` and the `run()` orchestrator. Responsibilities split across `event_loop.rs` (tokio::select loop, TraceWindow eviction, detect/score flush), `listeners.rs` (OTLP gRPC/HTTP spawn, optional energy scrapers), `tls.rs` (cert loading, `MaybeTlsStream`, HTTPS serve loop with handshake-concurrency cap), `json_socket.rs` (Unix NDJSON ingestion, cfg(unix)), `sampling.rs` (trace-id hash sampling), `findings_store.rs` + `query_api.rs` (retained findings and HTTP query endpoints)                                                                                    |
| **config**       | `config.rs`       | Parses `.perf-sentinel.toml` with sectioned format ([thresholds], [detection], [green], [daemon]) and legacy flat format backward compatibility                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| **time**         | `time.rs`         | Shared timestamp conversion helpers (`nanos_to_iso8601`, `micros_to_iso8601`). Used by OTLP, Jaeger and Zipkin ingestion                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| **explain**      | `explain.rs`      | Trace tree viewer: builds span tree from `parent_span_id`, annotates findings inline (suggestion, framework-specific fix, code location). Text and JSON output                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| **text_safety**  | `text_safety.rs`  | Pure helpers shared by every text renderer that prints attacker-controlled strings to a terminal: `sanitize_for_terminal` strips ASCII control characters to neutralise ANSI / OSC 8 / cursor injection; `safe_url` gates `suggested_fix.reference_url` to HTTPS without control chars                                                                                                                                                                                                                                                                                                                                  |

## Key types

| Type              | Module           | Description                                                                                                                                           |
|-------------------|------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------|
| `SpanEvent`       | event            | Raw I/O event (SQL query or HTTP call) with metadata and optional parent_span_id                                                                      |
| `NormalizedEvent` | normalize        | SpanEvent enriched with normalized template and extracted parameters                                                                                  |
| `Trace`           | correlate        | Collection of NormalizedEvents sharing the same trace_id                                                                                              |
| `Finding`         | detect           | Detected anti-pattern with type, severity, pattern details, timestamps, green_impact and `confidence` (CI batch / daemon staging / daemon production) |
| `GreenSummary`    | score            | Aggregate I/O stats: total ops, avoidable ops, waste ratio, top offenders, optional CO2                                                               |
| `QualityGate`     | quality_gate     | Pass/fail result with individual rule evaluations                                                                                                     |
| `Report`          | report           | Complete analysis output: analysis metadata, findings, green summary, quality gate                                                                    |
| `Config`          | config           | Parsed configuration with all sections and validated fields                                                                                           |
| `TraceWindow`     | correlate/window | LRU cache of active traces for streaming mode with TTL eviction                                                                                       |

## Crate boundaries

```
sentinel-cli (binary)
  |
  +-- clap CLI: analyze / explain / watch / demo / bench / inspect / pg-stat / tempo / calibrate subcommands
  |
  +-- depends on sentinel-core (library)
        |
        +-- All pipeline logic
        +-- Traits at borders only: IngestSource, ReportSink
        +-- Pure functions between stages
```

The CLI crate is intentionally thin: it parses arguments, loads config and delegates to sentinel-core functions. All business logic lives in sentinel-core.
