# Known limitations and trade-offs

## Contents

- [OTLP capture reliability](#otlp-capture-reliability): why perf-sentinel may miss spans as a passive listener.
- [Instrumentation quality bounds findings](#instrumentation-quality-bounds-findings): why a thin report can mean missing instrumentation, not a clean service.
- [Non-SQL datastores are not analyzed](#non-sql-datastores-are-not-analyzed): why Redis, MongoDB and similar spans are dropped at ingestion.
- [SQL tokenizer](#sql-tokenizer): regex-based normalizer trade-offs.
- [ORM bind parameters and N+1 vs redundant classification](#orm-bind-parameters-and-n1-vs-redundant-classification): how named bind placeholders affect classification.
- [HTTP query-string redaction and N+1 visibility](#http-query-string-redaction-and-n1-visibility): why query-parameter N+1 loops are invisible under instrumentations that redact the query string.
- [Slow findings and waste ratio](#slow-findings-and-waste-ratio): why slow findings do not contribute to the I/O waste ratio.
- [Score interpretation](#score-interpretation): the healthy / moderate / high / critical bands for `io_intensity_score` and `io_waste_ratio`.
- [Fanout detection requires `parent_span_id`](#fanout-detection-requires-parent_span_id): instrumentation prerequisite.
- [`rss_peak_bytes` on Windows](#rss_peak_bytes-on-windows): why bench RSS is null on Windows.
- [Upstream sampling and detection accuracy](#upstream-sampling-and-detection-accuracy): why 1-10% head-based sampling hides rare patterns and quiets cross-trace correlation.
- [Sampling in daemon mode](#sampling-in-daemon-mode): consequences of `sampling_rate < 1.0`.
- [Maximum events per trace](#maximum-events-per-trace): per-trace ring-buffer cap.
- [Long-running traces and TTL eviction in daemon mode](#long-running-traces-and-ttl-eviction-in-daemon-mode): why sparse-burst traces undercount in streaming mode.
- [Analysis backpressure and load shedding](#analysis-backpressure-and-load-shedding): why a slow analysis worker sheds whole batches, explicitly metered.
- [Daemon state model: in-memory, single-process, no shared state](#daemon-state-model-in-memory-single-process-no-shared-state): why replicas do not share state and an ungraceful kill drops the in-flight window.
- [Field length limits at ingestion](#field-length-limits-at-ingestion): per-field byte caps applied at the ingestion boundary.
- [Binary size](#binary-size): release-binary target and what contributes to it.
- [HTML dashboard: CSV formula-injection guard](#html-dashboard-csv-formula-injection-guard): OWASP CSV-injection neutralization in exported CSVs.
- [No authentication (TLS available, auth not built-in)](#no-authentication-tls-available-auth-not-built-in): network access policy for ingestion endpoints.
- [Query-API subcommands: endpoint value must be trusted](#query-api-subcommands-endpoint-value-must-be-trusted): SSRF surface on `tempo` and `jaeger-query`.
- [Carbon estimates accuracy](#carbon-estimates-accuracy): I/O to energy to CO₂ proxy methodology and its uncertainty.
- [Chatty service detection](#chatty-service-detection): per-trace HTTP-only scope.
- [Connection pool saturation detection](#connection-pool-saturation-detection): heuristic based on SQL span overlap, not pool metrics.
- [Serialized calls detection](#serialized-calls-detection): info-severity heuristic on sequential sibling spans.
- [Cross-trace correlation](#cross-trace-correlation): statistical co-occurrence, not causality.
- [OTel source code attributes](#otel-source-code-attributes): the `code.*` attributes required for `code_location`.
- [Daemon query API](#daemon-query-api): no built-in auth, gate via network policy or reverse proxy.
- [Automated pg_stat ingestion from Prometheus](#automated-pg_stat-ingestion-from-prometheus): prerequisites for the `--prometheus` flag.
- [Secrets and credentials](#secrets-and-credentials): env-var-preferred pattern for scrapers.
- [Electricity Maps API](#electricity-maps-api): API-key handling and caveats.
- [Tempo ingestion](#tempo-ingestion): protobuf format requirement.
- [gCO2eq energy constant (legacy section)](#gco2eq-energy-constant-legacy-section-kept-for-cross-references): cross-reference to Carbon estimates accuracy.
- [pg_stat_statements ingestion](#pg_stat_statements-ingestion): no trace correlation, complementary hotspot signal.

## OTLP capture reliability

perf-sentinel is a passive listener: it receives traces forwarded by OpenTelemetry SDKs or collectors and cannot guarantee that every span is captured. Spans may be lost to network issues, SDK or collector sampling, or application crashes before flush.

For critical CI pipelines, use batch mode (`perf-sentinel analyze`) on pre-collected trace files rather than live capture.

## Instrumentation quality bounds findings

Every finding is derived from a normalized span. perf-sentinel reads a closed list of carrying attributes (the query text `db.statement` / `db.query.text`, the target URL `http.url` / `url.full`, plus the enrichment attributes listed in [Required span attributes](./INSTRUMENTATION.md#required-span-attributes)). A span that carries none of them is not an I/O operation and is skipped. A SQL span that *is* an I/O operation but ships without query text, or an HTTP span without a URL, is also skipped: there is nothing to normalize, so no finding can be produced. Detection is bounded by the quality of the upstream instrumentation, the same way any software-only tool is bounded by its measurement source.

The skip emits no per-span warning or error, so a missing attribute does not surface as a problem, it surfaces as the *absence* of a finding. Since 0.8.7 the daemon counts the filtering in aggregate: `perf_sentinel_otlp_spans_received_total` and `perf_sentinel_otlp_spans_filtered_total{reason}` expose the retention ratio on `/metrics` (see [METRICS.md](./METRICS.md#otlp-ingestion-metrics)), so a fleet whose spans all filter out is visible without per-span noise. The common real-world cause is an instrumentation that omits the statement by default: .NET needs `SetDbStatementForText = true`, and several libraries redact statements for security unless statement capture is explicitly enabled. See [Required span attributes](./INSTRUMENTATION.md#required-span-attributes) for the per-language settings.

The operational consequence: a thin or empty report is not evidence that a service is clean. It can equally mean the spans never carried what perf-sentinel needs. Audit your own tracing before trusting a low score. Run `perf-sentinel inspect --input <events.json>` (or `query --daemon <URL> inspect` against a live daemon) and confirm that SQL and HTTP spans appear with their query text and URLs. A sparse or empty span tree is the signal that the entry cost is instrumentation work, not a green light.

One statement-quality gap is repaired automatically, within bounds: layered instrumentations that split a query across a statement-bearing ~0 ms span and a statement-less duration span (PHP Doctrine + PDO) are re-joined into one event at OTLP conversion. The stitch applies only on the OTLP path (Jaeger and Zipkin JSON imports keep the split blind spot), never to non-SQL datastores, only to statement-less spans whose name suggests query execution (`execute`, `query`), and only within one `ResourceSpans` block, so a pair split across collector batches falls back to the plain `missing_db_statement` filtering described above. A statement-less span carrying no `db.system` (the Doctrine layer emits the engine only on its pdo child) is stitched only when it has a statement-bearing sibling, so a wrapper span over its own SQL child does not adopt a descendant's statement. Merged spans are counted under the `merged_db_span` reason.

Because the stitch is scoped to one export request, slow queries are its worst case: a slow query's prepare span ends immediately (exported in an early batch) while its execute span ends hundreds of milliseconds to seconds later (a later batch), so the exporter's batching splits the pair across requests and the slow execute falls back to `missing_db_statement`. The slowness *is* the time gap that splits the batch. Fast queries (the N+1 pattern) do not split, so they stitch reliably. On split-layer emitters (PHP Doctrine) under load, enough pairs coincidentally co-batch that `slow_sql` still fires, but its occurrence count under-reports relative to a single-layer emitter running the same fault. An operator who needs full `slow_sql` yield for such an emitter can widen the exporter's batching window past the slowest query so a query's prepare and execute leave in the same export request: raise the OpenTelemetry Collector batch processor `timeout` (and `send_batch_size`), or the SDK `BatchSpanProcessor` `scheduledDelayMillis`. The cost is higher finding latency and larger export batches (keep them under `[daemon] max_payload_size`, 16 MiB by default).

## Non-SQL datastores are not analyzed

perf-sentinel models two I/O kinds, relational SQL and outbound calls (HTTP and RPC such as gRPC). A span whose `db.system` names a non-SQL datastore (`redis`, `memcached`, `mongodb`, `cassandra`, `dynamodb`, `couchbase`, `couchdb`, `elasticsearch`, `opensearch`, `neo4j`, `hbase`, `geode`, `influxdb`) is dropped at ingestion, because its `db.statement` is not relational SQL and the SQL tokenizer would mangle it (a Redis `GET user:123` is not a query template). The drop is gated on `db.system` alone, so a non-SQL span is dropped whether or not it carries a statement or a URL, consistently across the OTLP, Jaeger and Zipkin paths. Dropping is the harm-reduction choice: it avoids false N+1 and redundant findings on cache or document traffic. A `db.system` that is absent or names a SQL engine (`postgresql`, `mysql`, `mssql`, `oracle`, `clickhouse`, `cockroachdb`, ...) is always treated as SQL, so no relational traffic is dropped by mistake.

Because the span never enters the pipeline, a dropped non-SQL call is also invisible to the structural detectors (excessive fanout, serialized calls): a request that fans out to many cache or document-store calls will not raise those findings. On the OTLP path the drop is counted under the dedicated `non_sql_datastore` reason in `perf_sentinel_otlp_spans_filtered_total`, kept separate from `not_io` so a cache-only fleet does not trip the daemon zero-retention warning, which counts only genuine instrumentation-gap reasons.

## SQL tokenizer

The SQL normalizer uses a homemade regex-based tokenizer rather than a full SQL parser. Intentional trade-off: small binary, no heavy deps, works across dialects.

- No semantic parsing: literals and UUIDs are replaced positionally, no AST.
- Query length: 64 KB cap, truncated at a character boundary before normalization to bound adversarial input memory.
- CTEs (`WITH ... AS (...)`) supported including nested.
- Quoted identifiers are preserved verbatim and digits inside them are not mistaken for literals: ANSI `"MyTable"` and MySQL `` `table` ``. The scan closes on the first delimiter, so a doubled-delimiter escape (`""`, `` ` `` `` ` ``) is not decoded.
- Dollar-quoted strings (`$$body$$`, `$tag$body$tag$`) collapse to `?`, including in function bodies.
- `CALL` statements normalize literal params, SQL expressions like `NOW()` and `INTERVAL '...'` are handled.
- SQL Server `[...]` identifiers are not special-cased (`[` is a normal character). Common ones like `[Order Details]` or `[Col1]` survive intact, while an all-numeric bracketed identifier such as `[123]` has its digits replaced with `?`. Treating `[` as a normal character keeps PostgreSQL array literals and subscripts (`ARRAY['a', 'b']`, `arr[1]`) correctly redacted.

If a query normalizes incorrectly, open an issue with the raw SQL anonymized.

**Complementarity with pg_stat_statements.** perf-sentinel sees per-trace patterns (N+1, redundant) that pg_stat_statements cannot. pg_stat_statements provides aggregate server-side stats (total calls, mean time) that perf-sentinel does not track. Use both for full coverage.

## ORM bind parameters and N+1 vs redundant classification

ORMs that use named bind parameters (Entity Framework Core with `@__param_0`, Hibernate with `?1`) produce SQL spans where the parameter values are not visible in the `db.statement`/`db.query.text` attribute. perf-sentinel sees the template with the bind placeholders but not the actual values.

This means that N+1 patterns (same query, different values) may be classified as `redundant_sql` (same query, same visible params) instead of `n_plus_one_sql` (same query, different params). Both findings correctly identify the repeated query pattern and the suggestion to batch or cache remains valid.

ORMs that inline literal values (SeaORM with raw statements, JDBC without prepared statements) produce spans with visible parameter values, enabling accurate N+1 vs redundant classification.

The same limitation applies to RPC (gRPC, Dubbo). A gRPC request payload lives in the protobuf message body, not in a span attribute, so N distinct calls to one method share a single empty-parameter template and read as `redundant_http` rather than `n_plus_one_http`. RPC spans are also modeled as outbound HTTP calls (they carry no statement or URL), so their findings surface under the `_http` types with HTTP-flavored remediation text, and only the outbound `SpanKind::Client` side is admitted (the inbound handler span carries the same `rpc.*` keys but is inbound work). See [INSTRUMENTATION.md](./INSTRUMENTATION.md#required-span-attributes).

## HTTP query-string redaction and N+1 visibility

N+1 HTTP detection depends on the varying request parameter being visible in the span. An N+1 loop that varies a path segment (`GET /api/orders/1`, `/api/orders/2`, ...) normalizes to `GET /api/orders/{id}` with distinct extracted params, and is detected. An N+1 loop that varies a query parameter (`GET /api/mock?seq=1`, `?seq=2`, ...) is only detected if the query string survives to the span.

Some instrumentations redact the query string before export. OpenTelemetry .NET `System.Net.Http` redacts to `?*` by default (disable with `OTEL_DOTNET_EXPERIMENTAL_HTTPCLIENT_DISABLE_URL_QUERY_REDACTION=true`). When the query is redacted, every call in the loop carries a byte-identical `url.full`, so perf-sentinel sees the pattern as `redundant_http` (same URL repeated), not `n_plus_one_http` (same URL, different parameter). The varying parameter was destroyed upstream, so no trace consumer (Jaeger, Tempo, or any OTLP backend) can recover it, not just perf-sentinel.

Both verdicts identify the repeated-call pattern and the suggestion to batch remains valid. To get `n_plus_one_http` on .NET specifically, either disable query redaction via the env var above, or model the varying identifier as a path segment rather than a query parameter.

## HTTP host grouping trade-off

The outbound-HTTP template keeps the callee host (`GET user-svc/api/x`), so two calls to the same path on different backend services stay separate groups instead of collapsing into a false `redundant_http`. The reverse cost is that a genuine N+1 can split when one backend is addressed with inconsistent host spelling within a trace. IPv4/IPv6 literals are dropped (load-balanced pod replicas keep deduping), a trailing DNS root dot is canonicalized (`svc.` == `svc`), and the host is lowercased, but a short name and its fully-qualified form (`user-svc` vs `user-svc.default.svc.cluster.local`) are not reconciled, so a loop that mixes both spellings against one backend can split into sub-threshold groups and go unreported. Likewise a per-item loop that fans out to distinctly-named backends on the same path (`GET shard-1/lookup`, `GET shard-2/lookup`, ...) is treated as distinct operations rather than one N+1, since those calls cannot be batched into a single endpoint. Emitting one consistent host spelling per callee keeps N+1 grouping intact.

## Slow findings and waste ratio

Slow findings (`slow_sql`, `slow_http`) represent operations that are **necessary but slow**, they are not avoidable I/O. Therefore, slow findings do **not** contribute to the I/O waste ratio or the `avoidable_io_ops` count in the GreenOps summary. They still appear in the findings list with `green_impact.estimated_extra_io_ops: 0`.

This is by design: the waste ratio measures how much I/O could be eliminated (N+1, redundant), while slow findings highlight operations that need optimization (indexing, caching) rather than elimination.

## Score interpretation

The CLI renders a `(healthy / moderate / high / critical)` qualifier next to `io_intensity_score` and `io_waste_ratio` and the same classification ships in the JSON report as sibling fields `io_intensity_band` and `io_waste_ratio_band`. Reference tables live in the main README.

### Why these thresholds

| Band              | Anchor |
|-------------------|--------|
| IIS_MODERATE 2.0  | Rule of thumb, typical CRUD endpoint does 1-2 I/O ops |
| IIS_HIGH 5.0      | Default `n_plus_one_threshold`, the point where `detect_n_plus_one` starts emitting findings |
| IIS_CRITICAL 10.0 | The `indices.len() >= 10` severity escalation in `detect::n_plus_one`, same number tags `Severity::Critical` |
| WASTE_HIGH 0.30   | Matches the default `io_waste_ratio_max`. The gate is user policy, the interpretation is a fixed heuristic, they stay independent on purpose so a relaxed gate does not silently mute the signal |
| WASTE_CRITICAL 0.50 | At least half of analyzed I/O is avoidable waste |

### Stability contract

Enum values (`healthy`, `moderate`, `high`, `critical`) are stable across versions, downstream consumers can branch on them. Numeric thresholds are versioned with the binary and may evolve. Consumers needing version-independent classification (e.g. a Grafana alert) should read the raw `io_intensity_score` / `io_waste_ratio` fields and apply their own bands.

### Per-detector severity

`Critical` / `Warning` / `Info` rules per detector live in [`docs/design/04-DETECTION.md`](design/04-DETECTION.md), with the per-detector thresholds (some config-tunable: `max_fanout × 3`, `chatty_service_min_calls × 3`).

## Fanout detection requires `parent_span_id`

Fanout detection (`excessive_fanout`) relies on the `parent_span_id` field to build parent-child relationships between spans. If the tracing instrumentation does not propagate parent span IDs (some older OTel SDKs or custom instrumentations), fanout detection will not produce findings.

Fanout findings, like slow findings, are **not** counted as avoidable I/O in the waste ratio. They represent a structural concern (too many child operations per parent) rather than eliminable I/O.

## `rss_peak_bytes` on Windows

The `perf-sentinel bench` command reports peak RSS (Resident Set Size) using platform-specific APIs. On Windows, this metric is reported as `null` because the current implementation uses Unix-only `getrusage()`. The throughput and latency metrics work on all platforms.

## Upstream sampling and detection accuracy

This section is about sampling applied **before** perf-sentinel, in the SDK or the collector. It is separate from the daemon's own `sampling_rate` knob, covered below.

Head-based sampling keeps or drops a whole trace at its root. Traces that are kept arrive complete, so the per-trace detectors (`n_plus_one`, `chatty_service`, `excessive_fanout`, `pool_saturation`, `serialized_calls`) stay correct on the traces perf-sentinel does see: an N+1 inside a kept trace is still fully visible. What aggressive head-based sampling (the common 1% to 10% used in production, for cost) degrades is coverage, not per-trace structure:

- **Rare patterns may never appear.** A pattern that only occurs in a small fraction of traffic can be sampled out entirely and never reach detection.
- **Aggregates are computed over a non-representative subset.** The I/O waste ratio and the Prometheus counters reflect only the sampled traces, so they cannot be read as whole-traffic figures.
- **Cross-trace correlation effectively stops firing.** The [cross-trace correlator](#cross-trace-correlation) needs a finding pair to recur (`min_co_occurrences`, default 5) inside its window. At low sampling rates the repeated co-occurrences rarely survive, so the correlator stays quiet even when the underlying coupling is real.

perf-sentinel does not inspect the W3C `sampled` flag and cannot tell a complete trace from a head-sampled survivor. It treats whatever arrives as the full trace.

Recommendations:

- For CI quality gates, run batch mode (`perf-sentinel analyze`) on fully captured traces. A gate that decides on 1% of traffic is not a gate.
- In the daemon, if you must sample for cost, prefer **tail-based** sampling at the collector. Tail-based keeps whole traces too, but lets you bias retention toward slow or error traces, which is where structural waste concentrates.

## Sampling in daemon mode

This is perf-sentinel's own sampling knob, applied after ingestion, distinct from the upstream sampling described above. When `sampling_rate` is set below 1.0 in the `[daemon]` configuration, perf-sentinel randomly drops traces to reduce resource usage. This means:

- Some N+1 or redundant patterns may go undetected
- The waste ratio is computed only over sampled traces and may not represent the full traffic
- Prometheus metrics (`perf_sentinel_traces_analyzed_total`) reflect only sampled traces

For accurate detection, use `sampling_rate = 1.0` (the default) or sample at the collector level where you have more control.

## Maximum events per trace

In streaming mode, each trace holds at most `max_events_per_trace` events (default: 1000) in a ring buffer. If a trace generates more events, the oldest are dropped. This can cause:

- Missed N+1 patterns if the repeated operations fall outside the retained window
- Undercounted occurrences in findings

For traces with very high event counts, increase `max_events_per_trace` or investigate why a single trace generates so many operations.

## Long-running traces and TTL eviction in daemon mode

The streaming detector window evicts a trace when it has been inactive for `trace_ttl_ms` (default 30s). "Inactive" means no span event for that `trace_id` was ingested within the TTL. The active TTL is reset on every span ingest, so a trace that emits a span every <30s stays alive indefinitely.

But a trace that emits sparse, gap-heavy spans (e.g. a long batch job emitting one span every 60s, or a long-polling websocket) will be evicted between bursts. A late span with the same `trace_id` arriving after eviction creates a **new** trace bucket; the previous events are gone. Threshold-driven detections that rely on co-located spans within one trace (`n_plus_one`, `chatty_service`, `excessive_fanout`, `pool_saturation`, `serialized_calls`) will silently underreport because each fragment falls below the per-trace threshold.

Mitigations, in order of precision:

- **Increase `trace_ttl_ms`** if you know the maximum expected gap between bursts (`[daemon] trace_ttl_ms = 120000` for 2 minutes). Memory grows with `max_active_traces`, not with TTL, so a longer TTL costs nothing as long as your traffic shape does not blow past the LRU cap.
- **Use batch mode** (`perf-sentinel analyze`) on a captured trace dump for off-line investigation. Batch correlation has no TTL boundary; the entire trace is correlated in a single pass.
- **Shorten the upstream trace.** If a trace is conceptually long because it spans multiple user actions, consider splitting it at the application level (one trace per logical request).

This is a property of the streaming window, not a bug. Real-time detection on a bounded ring buffer always trades trace duration against memory; the daemon picks 30s as a default that fits typical request-response shapes (HTTP API, RPC).

## Analysis backpressure and load shedding

detect+score run on a single dedicated analysis worker, decoupled from the ingestion / eviction `select!` loop by a bounded channel (1024 batches). The loop enqueues evicted and expired batches without blocking, so a slow analysis pass can no longer stall ingestion or delay TTL eviction. The trade-off is that when analysis cannot keep up with sustained load, the queue fills and whole batches are **shed**. Shedding is explicit and metered, not silent:

- `perf_sentinel_analysis_queue_depth` exposes the current backlog. A sustained nonzero value means the worker is falling behind.
- `perf_sentinel_analysis_shed_batches_total` and `perf_sentinel_analysis_shed_traces_total` count what was dropped. Alert on `rate(perf_sentinel_analysis_shed_batches_total[5m]) > 0`.

A shed batch is dropped from detection entirely: its findings are never emitted and the cross-trace correlator never sees it. Because perf-sentinel surfaces *recurring* patterns, a shed N+1 or chatty path is normally re-detected on the next request once the worker catches up. If you see sustained shedding, the daemon is undersized for the trace volume: scale out (shard by `trace_id`), raise `sampling_rate` headroom, or reduce per-trace cost upstream.

Shedding is the response to *overload*, not to *failure*. If the analysis worker itself stops (e.g. a detector panics on a pathological trace), the daemon does not stay up analyzing nothing: it exits with an error so a supervisor (Kubernetes, systemd) restarts the process, the same fail-loud behavior the older inline-detection design had when a panic crashed the whole daemon. Any batch enqueued in the brief window before exit is counted as shed rather than lost silently.

A graceful shutdown does **not** shed: it drains the window and joins the worker so every in-flight batch is analyzed before exit.

Analysis shedding is separate from archive retention. The per-window disclosure archive (`daemon/archive.rs`, the NDJSON that `disclose` later aggregates) has its own bounded channel with an explicit drop-on-full policy. Under sustained load, or if the writer task falls behind on disk I/O, whole windows are dropped from the archive even when their findings were analyzed and served on the live endpoints, and a graceful shutdown drains the analysis worker but does not extend the same delivery guarantee to the archive. This is best-effort by design (public transparency, not regulatory-grade), so treat the archive as a sampled record rather than a complete ledger.

## Daemon state model: in-memory, single-process, no shared state

The daemon's correlation state is entirely in memory: a 30s rolling window (`trace_ttl_ms`) over a 10,000-trace LRU (`max_active_traces`), both tunable under `[daemon]`. There is no persistence layer, no write-ahead log, no snapshot, no disk spill. This shapes three operational properties that matter for a serious production deployment.

**A graceful shutdown drains, an ungraceful kill does not.** On a clean shutdown the daemon drains its window through detection before exiting. Both SIGINT (Ctrl+C) and, on Unix, SIGTERM trigger this drain, so a normal Kubernetes pod termination (rolling update, scale-down) flushes the in-flight window instead of dropping it. An *ungraceful* death still loses it: SIGKILL (the kubelet's forced kill after the termination grace period), an OOM kill, or a process crash skips the drain and drops the in-flight traces, up to one full window, with no recovery.

The practical impact is small. What is in flight at that moment is incomplete traces (they have not hit their TTL, so they may still be receiving spans), and perf-sentinel surfaces *recurring* patterns: an N+1 or a chatty path that the dropped window would have flagged reappears on the next request and is caught by the new process within seconds. The trace data is not lost either, it lives upstream in your collector or trace store. Runtime acknowledgments are file-backed and survive (see [StatefulSet mode](./HELM-DEPLOYMENT.md#statefulset)). The one place a gap is visible is the opt-in per-window NDJSON archive, which misses the window that was in flight at the kill. If that matters, keep `trace_ttl_ms` short, or run gates in batch mode where there is no window to lose.

**Replicas do not share state.** Each daemon instance is independent: its own window, its own metrics, its own correlator. The Helm chart exposes `workload.replicas`, but there is no leader election and no shared store. Two replicas analyzing the same service compute two partial pictures, never a merged one. Prometheus counters are per-replica and must be aggregated at the PromQL layer.

**Correlation assumes co-located spans.** The per-trace detectors and the [cross-trace correlator](#cross-trace-correlation) are a per-process structure. They assume every span of a given trace, and every related trace, lands in the same daemon. The consequences for scaling out:

- **Per-trace detectors** (`n_plus_one` and friends) are correct under multiple replicas *only* if the collector load-balances by `trace_id` so all spans of a trace reach the same instance. The OTel Collector `loadbalancingexporter` with `routing_key: traceID` does this, and the [sharded topology](./HELM-DEPLOYMENT.md#deployment-default) relies on it.
- **Cross-service correlation** has no distributed answer today. It only sees what one process buffers, so it must run on a single daemon that receives all the relevant services, or you accept partial correlation. Routing by `trace_id` does not help here, because cross-service correlation spans *different* traces.

## Field length limits at ingestion

All ingestion boundaries (OTLP, JSON, Jaeger, Zipkin) truncate string fields to prevent unbounded memory growth from oversized attributes. Limits: `service` 256 bytes, `operation` 256 bytes, `target` 64 KB, `source.endpoint` 512 bytes, `source.method` 512 bytes, `timestamp` 64 bytes, `trace_id`/`span_id` 128 bytes. Truncation preserves UTF-8 char boundaries. Fields within the limit are untouched (zero-copy fast path).

## Binary size

The release binary targets < 15 MB with `lto = "thin"`, `strip = true` and `panic = "abort"`. The embedded carbon intensity table and OTLP protobuf support contribute to binary size. If you need a smaller binary and do not use OTLP ingestion, building with feature flags (future work) could reduce size.

## HTML dashboard: CSV formula-injection guard

Every cell in the CSVs exported by the HTML dashboard's per-tab **Export CSV** button is checked against OWASP CSV injection. If the first character of a cell is `=`, `+`, `-`, `@`, or a horizontal tab (`\t`), a single apostrophe is prefixed so Excel, LibreOffice Calc and Google Sheets display the literal text rather than evaluate it as a formula on open. The prefix is invisible in the spreadsheet view and does not alter the data for consumers that parse the CSV as plain text. Triggers are only neutralized at position 0, so a legitimate template like `abc=def` still exports unchanged.

## No authentication (TLS available, auth not built-in)

perf-sentinel does **not** implement authentication on its ingestion endpoints. By default, the daemon binds to `127.0.0.1` (loopback only), which is safe for single-machine deployments.

**TLS is supported** on the OTLP gRPC and HTTP listeners via the `[daemon] tls_cert_path` and `tls_key_path` configuration fields. When both are set, the daemon serves OTLP and `/metrics` over TLS. The JSON unix socket and Prometheus `/metrics` scraping are not separately configurable: `/metrics` shares the HTTP port and inherits its TLS setting. See [`docs/CONFIGURATION.md`](CONFIGURATION.md) for the full reference.

If you expose perf-sentinel to a network:

- **Enable TLS** via `tls_cert_path` and `tls_key_path` to encrypt traffic in transit
- Use network policies (Kubernetes `NetworkPolicy`, Docker network isolation, firewall rules) to restrict access
- For **authentication**, place perf-sentinel behind a reverse proxy (nginx, envoy) that handles bearer tokens or mTLS client certificates
- Route traces through an OpenTelemetry Collector with its own auth extensions and forward to perf-sentinel on a trusted internal network

Never expose perf-sentinel directly to untrusted networks without at minimum TLS enabled and network-level access controls in place.

### JSON socket hardening

The Unix-domain JSON socket (`[daemon] json_socket`) defends against local-user attacks on a multi-tenant host with two mechanisms:

- **Permissions `0o600`** are applied right after `bind()`. Other local users cannot connect to inject events.
- **Symlink pre-check**: before the daemon unlinks any stale socket file at the configured path, it calls `symlink_metadata()` and refuses to proceed if the path is a symlink. This prevents a local attacker with write access to the socket's parent directory from pointing `json_socket` at a victim file (e.g., `/etc/passwd`) and having the daemon's startup `remove_file()` delete it.

Both defenses only matter when `json_socket` lives in a directory writeable by other local users. If you put the socket in a daemon-owned directory (`/var/run/perf-sentinel/` with `0o700`), the surface is already closed at the filesystem level.

### JSON socket per-connection payload budget

`[daemon] max_payload_size` (default 1 MiB) caps individual NDJSON batches submitted through the JSON socket. A single connection may stream multiple batches before closing and the daemon tolerates up to **16× `max_payload_size`** per connection before truncating the stream. At the default setting this means one connection can transfer up to 16 MiB of trace data.

The factor is deliberate: it accommodates clients that emit many small batches over a single long-lived connection (e.g. a sidecar shipping a buffered queue after a flush), without exposing the daemon to memory exhaustion from an attacker. A client that needs more than 16× the configured batch size should open a new connection. The cap cannot be disabled.

### TLS handshake concurrency cap

Each TLS listener (OTLP gRPC and OTLP HTTP) caps concurrent in-flight handshakes and live HTTPS connections at **128**. Handshakes run in dedicated tasks so a single stalled peer cannot block the accept loop and the cap bounds fds, rustls buffers and task slots against a handshake flood. A 10s handshake timeout (`TLS_HANDSHAKE_TIMEOUT`) drops peers that complete TCP but never send a `ClientHello`. The cap is not configurable; it mirrors the Unix JSON socket listener budget.

## Query-API subcommands: endpoint value must be trusted

The `tempo` and `jaeger-query` subcommands both make outbound HTTP requests to a user-supplied backend endpoint. One constraint to know:

- **`--endpoint` is trusted input.** The validator rejects non-`http(s)` schemes and credential-embedded URLs (`user:pass@host`), but it accepts loopback, RFC 1918, link-local, and cloud-metadata targets (`169.254.169.254`). In a single-user CLI invocation this is the expected behaviour (dev-local setups, port-forwarded backends, etc.). In CI pipelines where the endpoint could be sourced from an external PR or an untrusted environment variable, sanitize the value upstream before invoking the subcommand.

### Auth headers

Both subcommands support an optional `--auth-header "Name: Value"` flag that attaches a single custom header to every backend request. Use it for Bearer tokens, Basic Auth, or custom API-key headers. The parsed value is marked `sensitive` so hyper redacts it from debug output and HTTP/2 HPACK tables, and the subcommand never logs the value. Examples:

```bash
perf-sentinel jaeger-query --endpoint https://jaeger.prod \
  --service order-svc --lookback 1h \
  --auth-header "Authorization: Bearer ${JAEGER_TOKEN}"

perf-sentinel tempo --endpoint https://tempo.prod \
  --service order-svc --lookback 1h \
  --auth-header "X-API-Key: ${TEMPO_KEY}"
```

Validation (rejected at parse time with a dedicated exit code):

- Raw input must be under 8 KiB.
- Name and value must be non-empty after trimming.
- Value must be valid HTTP per RFC 7230 (no CR, LF, or non-visible ASCII).
- Header name must not be `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`, or `Proxy-Connection`. These framing and authority headers are blocked to prevent request smuggling and cache poisoning via an untrusted environment variable expansion.

### `--auth-header-env NAME`: ps-safe alternative

Both subcommands also accept `--auth-header-env NAME`, which reads the header line from the named environment variable instead of from `argv`. This avoids `ps`/`/proc/<pid>/cmdline` exposure. The env var value must already be in `Name: Value` curl format. `--auth-header` and `--auth-header-env` are mutually exclusive at the clap level.

```bash
export JAEGER_AUTH="Authorization: Bearer ${JAEGER_TOKEN}"
perf-sentinel jaeger-query --endpoint https://jaeger.prod \
  --service order-svc --lookback 1h \
  --auth-header-env JAEGER_AUTH
```

Caveats shared by both flags:

- Only one header is supported per invocation. If you need Basic Auth and an additional tenant header, compose the flag with the primary auth scheme and set the secondary one at the proxy layer.
- Setting `--auth-header` together with an `http://` endpoint emits a `tracing::warn!` because the credential would travel in cleartext. Prefer `https://` whenever the backend supports it.

## Carbon estimates accuracy

perf-sentinel uses an **I/O → energy → CO₂ proxy model** to estimate the carbon footprint of analyzed workloads. The chain has three steps and an inherent margin of error at each:

1. **I/O operations → energy**: each detected I/O op (SQL query, HTTP call) is multiplied by a fixed `ENERGY_PER_IO_OP_KWH` constant of `0.0000001 kWh` (~0.1 µWh). This is **not measured**, it is an order-of-magnitude approximation.
2. **Energy → CO₂**: energy is multiplied by a per-region grid carbon intensity (gCO₂eq/kWh) sourced from Electricity Maps and Cloud Carbon Footprint annual averages (2023-2024), with a per-provider PUE applied (AWS 1.15, GCP 1.09, Azure 1.17, Generic 1.2). The three provider PUEs are not strictly comparable in scope: AWS publishes a global fleet average for calendar year 2024, GCP a global fleet trailing-twelve-month average for 2024, Azure an FY25 (July 2024 to June 2025) figure for its owned-and-controlled facilities only (leased and colocation are excluded). The cross-window gap is around 12 months and the scope difference is around a few percent of the fleet.
3. **Embodied carbon (`M` in SCI v1.0)**: hardware manufacturing emissions amortized at a configurable default of `0.001 gCO₂/request`. Region-independent.

### Uncertainty: 2× multiplicative, not ±50%

Every CO₂ estimate is reported as `{ low, mid, high }` where:

```
low  = mid × 0.5   (half the midpoint)
high = mid × 2.0   (twice the midpoint)
```

This is a **log-symmetric multiplicative interval**, not an arithmetic ±50% window. The geometric mean of `low` and `high` equals `mid`; the arithmetic mean does not. The 2× framing is deliberate: the I/O proxy model has order-of-magnitude uncertainty (ENERGY_PER_IO_OP_KWH is rougher than half), so a symmetric ±50% window would understate the real model uncertainty. Read the bounds as "the true value is within a factor of 2 of `mid`, in either direction".

The bounds reflect aggregate model uncertainty, not per-endpoint variance.

**This bracket is a directional indicator of model uncertainty, not a statistical confidence interval.** The true value on unusual I/O workloads (mixed SQL + HTTP, cache-heavy paths, custom storage engines) may fall outside `[low, high]`. Use the range to gauge *order-of-magnitude* plausibility, not as a probabilistic bound.

### SCI v1.0 semantics: numerator vs intensity

The `co2.total` field holds the **SCI v1.0 numerator** `(E × I) + M`, summed over all analyzed traces (a **footprint**, absolute emissions). The per-functional-unit **intensity** score that the SCI specification calls "SCI" is emitted alongside it on `co2.sci_per_trace`:

```
co2.sci_per_trace.mid = co2.total.mid / analysis.traces_analyzed
```

The functional unit R is declared on `co2.functional_unit` (`"trace"`). Both views are kept because they answer different questions: the footprint sizes the absolute impact, the intensity normalizes it per unit of work. The `methodology` field on each `CarbonEstimate` tags the semantic:

- `co2.total.methodology = "sci_v1_numerator"`: the `(E × I) + M` footprint over analyzed traces.
- `co2.sci_per_trace.methodology = "sci_v1_intensity"`: the per-R intensity `((E × I) + M) / R`, R = 1 trace.
- `co2.avoidable.methodology = "sci_v1_operational_ratio"`: `operational × (avoidable_io_ops / accounted_io_ops)`, a region-blind global ratio that excludes embodied carbon by design.

### Positioning: directional waste counter

perf-sentinel is a **directional waste counter** designed to:

- **Detect performance anti-patterns** (N+1, redundant queries, fanout) and quantify their relative carbon impact.
- **Compare runs** before/after optimization to validate that a fix actually reduces I/O.
- **Catch carbon regressions** in CI as a guardrail.

It is **NOT a regulatory carbon accounting tool**. Do **NOT** use it for:

- CSRD (Corporate Sustainability Reporting Directive) reporting.
- GHG Protocol Scope 3 disclosures.
- Audit-grade compliance documents.
- Comparing absolute CO₂ values across different infrastructures (the model assumes a uniform, average server profile).
- Replacing real measured energy data (RAPL, Scaphandre, in-process power meters).

### What works

| Use case                              | Reliability                             |
|---------------------------------------|-----------------------------------------|
| Detect waste (N+1, fanout, redundant) | ✅ deterministic counting                |
| Compare runs (baseline vs. fix)       | ✅ relative deltas are meaningful        |
| Rank endpoints by relative impact     | ✅ within a single deployment            |
| CI carbon regression guardrails       | ✅ via `[thresholds] io_waste_ratio_max` |
| Absolute CO₂ in compliance reports    | ❌ 2× multiplicative uncertainty         |
| Cross-infrastructure comparison       | ❌ assumes uniform server profile        |
| Replacing measured energy             | ❌ proxy only                            |

### Multi-region scoring

When OTel spans carry the `cloud.region` resource attribute, perf-sentinel automatically buckets I/O ops per region and applies the correct grid intensity coefficient. The fallback chain is:

1. `event.cloud_region` from the OTel attribute.
2. `[green.service_regions]` per-service config mapping.
3. `[green] default_region`.

I/O ops with no resolvable region land in a synthetic `"unknown"` bucket and contribute zero operational CO₂ (a `tracing::warn!` is emitted). Embodied carbon is still emitted because hardware emissions are region-independent.

See `docs/design/05-GREENOPS-AND-CARBON.md` for the full methodology, formula and SCI v1.0 alignment notes.

### Hourly carbon profiles

Embedded hourly UTC profiles are available for 30+ cloud regions across all major cloud providers and geographies. Four regions (FR, DE, GB, US-East) have **monthly x hourly** profiles (288 values each) that capture seasonal variation. The remaining regions have **flat-year** profiles (24 values, same shape all year).

**Monthly x hourly regions** (12 months x 24 hours):

- **France (`eu-west-3`)**: nuclear baseload with winter gas peaking. Higher intensity in winter, lower in summer.
- **Germany (`eu-central-1`)**: coal + renewables. Strong seasonal variance: winter coal use increases significantly.
- **UK (`eu-west-2`)**: wind + gas. Winter has more gas heating, summer has more wind.
- **US-East (`us-east-1`)**: gas + coal. Summer AC load and winter heating both push intensity above spring/fall.

**Flat-year hourly regions** (24-hour profile, same all year):

- **Europe (ENTSO-E)**: Ireland (`eu-west-1`), Netherlands (`eu-west-4`), Sweden (`eu-north-1`), Belgium (`europe-west1`), Finland (`europe-north1`), Italy (`eu-south-1`), Spain (`europe-southwest1`), Poland (`europe-central2`), Norway (`europe-north2`).
- **Americas (EIA / IESO / ONS)**: US Ohio (`us-east-2`), US N. California (`us-west-1`), US Oregon (`us-west-2`), Canada Quebec (`ca-central-1`), Brazil (`sa-east-1`).
- **Asia-Pacific (best-effort)**: Japan (`ap-northeast-1`), Singapore (`ap-southeast-1`), India (`ap-south-1`), Australia (`ap-southeast-2`).

Country-code aliases (`fr`, `de`, `gb`, `ie`, `se`, `no`, `jp`, `br`, etc.) and cloud-provider synonyms (`westeurope`, `northeurope`, `uksouth`, `francecentral`, etc.) are supported and resolve to the same profile.

When `[green] use_hourly_profiles = true` (the default), the scoring stage uses the hour-specific (and month-specific when available) intensity for each span based on the span's UTC timestamp. Regions without a profile always use the flat annual value. Reports are tagged with `model = "io_proxy_v3"` (monthly x hourly), `"io_proxy_v2"` (flat-year hourly) or `"io_proxy_v1"` (annual) and each per-region breakdown row carries an `intensity_source` field (`"annual"`, `"hourly"` or `"monthly_hourly"`).

**What this does and doesn't do.** The hourly path captures time-of-day variance (a 3am N+1 in France costs less than a 7pm N+1). Monthly x hourly profiles also capture seasonal variance for the 4 listed regions. It does **not** capture:

- **Weather-dependent fluctuations**: the embedded values are typical averages, not real-time data. A calm windless day in the UK will produce more carbon than the profile suggests.
- **Real-time grid data**: the embedded profiles are static. For live carbon intensity (reported as `intensity_source = "real_time"`), enable the opt-in `[green.electricity_maps]` integration in daemon mode, see `docs/CONFIGURATION.md`.

**Estimated profiles.** The Asia-Pacific and Brazil profiles are estimated from fuel mix composition rather than hourly generation data. They are annotated as such in the source code. The diurnal shapes are approximations based on the known fuel mix (e.g. gas-dominated grids are nearly flat, coal-heavy grids have mild evening peaks).

**Timestamp requirements.** perf-sentinel parses timestamps as UTC and requires the canonical ISO 8601 form `YYYY-MM-DDTHH:MM:SS[.fff]Z` (trailing `Z`) or the space-separated variant. Strings with non-UTC offsets (`+02:00`, `-05:00`) are rejected rather than silently shifted. The carbon table is UTC-anchored, so naive offset handling would systematically skew the estimate. Spans with unparseable timestamps fall back to the flat annual intensity.

**Accuracy improvement (approximate).** Compared to the flat-annual model, the hourly profiles reduce the time-of-day component of the uncertainty budget from ~±50% to ~±20% **for the 4 listed regions only**. The overall 2× multiplicative uncertainty bracket on the CO₂ estimate is unchanged, because the energy-per-op proxy constant remains the dominant source of error.

To pin reports to the flat-annual model (e.g. to compare historical runs without the hourly shift), set `[green] use_hourly_profiles = false` in the config.

#### Germany (`eu-central-1`) hourly profile: divergence resolved in 0.8.7

Up to 0.8.6 the Germany hourly profile carried an arithmetic mean of ~431 g/kWh against an embedded flat annual of 338, a ~28% divergence the docs used to describe as "recent data being higher". An audit against primary sources inverted that story: the German grid got cleaner through 2023-2025 (Electricity Maps consumption-based: 475 in 2022, 379 in 2023, 341 in 2024), so the hourly profile level was the stale side, frozen at the 2022 coal-crisis level, while the annual value 338 was current. As shipped before 0.8.7, enabling hourly profiles inflated Frankfurt CO₂ by ~27% versus current primary data.

Since 0.8.7 the profile is rescaled (shape preserved, level normalized to the Electricity Maps 2024 mean of ~341), so the hourly and flat-annual paths agree within ±5% like every other profiled region.

**What this means for your reports:**

- Reports with `default_region = "eu-central-1"` (or spans carrying `cloud.region = eu-central-1`) and the default `use_hourly_profiles = true` show **CO₂ numbers roughly 21% lower** than 0.8.6.
- If you have CI quality gates (`[thresholds] io_waste_ratio_max` etc.) calibrated on the old DE hourly numbers, recalibrate after the upgrade.

A regression test (`de_flat_annual_numerical_regression`) pins the flat-annual value, and the profile-vs-annual ±5% invariant is now enforced for every region with no exception.

### What software-only attribution covers

perf-sentinel is a software-only attribution tool. The class includes RAPL readers (`intel-rapl` via Powercap) and model-based estimators that derive energy from CPU utilization (Cloud SPECpower, SPECpower coefficients pinned per SKU). On a typical server, neither kind sees the full wall-plug draw. RAPL reports CPU and DRAM packages and misses the storage controller, SSDs, NICs, fans, BMC, and the PSU conversion losses. Model-based estimators inherit the same scope by construction, since their coefficients are calibrated against CPU and DRAM power.

Independent published measurements vary by hardware and load, but the order of magnitude is consistent: on common Intel server parts, RAPL captures roughly half to two thirds of the wall-plug power, with the periphery making up the rest. perf-sentinel is in the same class and the same range. For total server energy on a known SKU, pair it with an external power meter (PDU SNMP, smart plug) or a hardware-level reading. For trace-attributable compute and DRAM energy, the model is what the model is, and the precision discussion is in [Scaphandre precision bounds](#scaphandre-precision-bounds) and [Cloud SPECpower precision bounds](#cloud-specpower-precision-bounds) below.

When reading benchmarks that compare these tools to an external meter, keep two quantities separate. First, the periphery that no software-only signal can cover. Second, how well a given tool attributes the fraction it does see to a container, a process, or a span. Only the second is a property of the tool. The first is a property of the signal.

### Alumet precision bounds

perf-sentinel ships an opt-in integration with [Alumet](https://github.com/alumet-dev/alumet) (INRIA/LIG, EUPL-1.2), scraped through its `prometheus-exporter` output plugin. `alumet_rapl` leads the measured-energy precedence chain.

**Why it outranks Scaphandre.** Both read RAPL. Alumet's sampling is measurably less error-prone, as characterized by its own authors in [Dissecting the software-based measurement of CPU energy consumption](https://hal.science/hal-04420527v2/document) (Raffin et al.), and it attributes per cgroup rather than per process, which matches container workloads more closely. Being ranked first is a statement about attribution fidelity, not about coverage: like Scaphandre, RAPL only sees CPU and DRAM, roughly half to two thirds of wall-plug power. For total server energy, see [Redfish BMC precision bounds](#redfish-bmc-precision-bounds).

**The interval-mismatch failure mode. Read this one.** Alumet's `prometheus-exporter` publishes every measurement as a Prometheus **gauge holding the last flushed value**, and `rapl_consumed_energy` is a `CounterDiff`: the joules consumed during one source `poll_interval`. It is neither a cumulative counter (like Kepler) nor a power reading (like Scaphandre). Two consequences:

- Summing raw readings across scrapes is wrong in both directions. Scraping faster than Alumet flushes double-counts the same value, scraping slower drops whole intervals. perf-sentinel therefore never sums, it divides by `energy_interval_secs` to recover watts and integrates over its own scrape window, exactly as it does for Scaphandre's power gauge.
- **`energy_interval_secs` cannot be verified against the wire.** The interval is nowhere in the exposition. If the declared value drifts from the Alumet-side `poll_interval`, every energy and carbon figure for the mapped services is rescaled linearly and **silently**: declaring `1.0` while Alumet polls at `5s` overstates energy 5x, with no warning, no failed scrape, and a `measured` provenance tag that looks authoritative. This is the single largest correctness risk in the integration. Re-check both files together whenever either changes. The daemon echoes the value it is using in the `Alumet scraper started` log line so the mistake is at least visible at startup.

The stationarity assumption is the same one Scaphandre carries: the sampled interval is taken as representative of the whole scrape window. Alumet's reading is a mean over its `poll_interval` rather than an instantaneous sample, which if anything is the better-behaved of the two.

**Attribution requires plugin composition.** `rapl` alone measures the machine, with no notion of workload. `procfs` identifies consumers by PID only, which is useless for a stable service mapping. Per-service attribution needs `rapl` + `k8s` + `energy-attribution`, and the resulting metric is named after an operator-chosen formula. That is why `metric_name` and `label_key` are mandatory config with no default, see `docs/CONFIGURATION.md`.

**Upstream version.** Alumet is **pre-1.0** (v0.9.5 at time of writing) and ships no wire-conformance CI job yet: GitHub runners expose no powercap tree, so RAPL cannot run there. Metric names and plugin config may change between releases. The runtime net is the zero-sample warn-once, which fires after three consecutive HTTP-200 ticks with no matching samples and names the likely causes. Treat an Alumet upgrade as a reason to re-run `curl <endpoint> | grep -i energy` and compare against `metric_name`.

**Platform requirements.** Linux, and whatever the chosen source plugin requires. The `rapl` source needs Intel or AMD x86_64 with RAPL access (perf-events or a readable `/sys/devices/virtual/powercap/intel-rapl`), so the same bare-metal and RAPL-passthrough constraints as Scaphandre apply. Alumet also ships ARM-relevant sources (`nvidia-jetson`, `grace-hopper`) that perf-sentinel can scrape through the same generic `metric_name` / `label_key` surface. **Be aware that the `alumet_rapl` model tag is applied to every Alumet reading regardless of which source plugin produced it**: perf-sentinel has no way to tell a RAPL series from a Jetson one, since the metric name is operator-chosen. Scraping a non-RAPL Alumet source therefore labels those figures `alumet_rapl` in the report, in `energy_source_models`, and in any published disclosure. Only point `[green.alumet]` at a RAPL-backed series unless you accept that provenance tag.

**Database waste is a lower bound with a count-based ratio.** `green_summary.database_waste` (from `[green.alumet.database]`) multiplies the database cgroup's measured window energy by `avoidable_sql_io_ops / total_sql_io_ops`. Three bounds to keep in mind. First, the energy term is the CPU package share only: Alumet's default attribution formula covers neither DRAM nor disk I/O, and a database is heavy on both, so the figure understates the real consumption (idle power is left unattributed by Alumet, which is the desired behavior for a waste ratio). Second, the ratio counts operations without weighting their cost: DBMS energy benchmarks measure up to 60% CPU power difference between operators at equal utilization (Tsirogiannis et al., SIGMOD 2010), so a mix of cheap avoidable point-SELECTs against heavy legitimate writes skews the split. Third, the interval-mismatch risk above applies to this figure too. Fourth, when analysis batches are shed under load, the shed windows' energy carries over and is delivered to the next scored window, multiplied by that window's ratio: per-window waste is approximate under shedding, only the energy total across windows stays exact. For all these reasons the figure is informational: excluded from `energy_kwh`, from `co2`, and from the public disclosure. It also assumes the declared cgroup serves all the SQL traffic the daemon sees, declare one database and keep multi-database deployments out of this figure unless the databases are homogeneous.

### Scaphandre precision bounds

perf-sentinel ships an opt-in integration with [Scaphandre](https://github.com/hubblo-org/scaphandre) for per-process energy measurement via Intel RAPL counters. When `[green.scaphandre]` is configured, the `watch` daemon scrapes the Scaphandre Prometheus endpoint every few seconds and uses the measured power readings to replace the fixed `ENERGY_PER_IO_OP_KWH` proxy constant for each mapped service.

**Prefer Alumet for new deployments.** [Alumet precision bounds](#alumet-precision-bounds) above explains why `alumet_rapl` outranks `scaphandre_rapl`: same RAPL source, less error-prone sampling as characterized in the Raffin et al. paper, and per-cgroup attribution. Upstream pace points the same way: at the time of writing the latest Scaphandre release is v1.0.2 (February 2025) and the ARM tracking issue below has been open since 2020, while Alumet is under active development. The Scaphandre integration stays supported for existing deployments.

**Upstream version.** The parser is version-agnostic by design: it consumes the standard Prometheus exposition for `scaph_process_power_consumption_microwatts` with `exe` and `cmdline` labels, which has been stable across upstream releases. The wire-conformance CI job (`.github/workflows/upstream-wire-conformance.yml`) pins **Scaphandre v1.0.2** by SHA256 of the upstream `.deb` artifact as the validated reference version. Other recent releases are expected to work, an upstream rename of the metric or labels would trip the zero-sample warn-once net at runtime and the wire-conformance assertion in CI.

**Platform requirements.** Scaphandre works on:

- **Linux only** (no Windows, no macOS, no BSD).
- **Intel or AMD x86_64 CPUs with RAPL support**: most recent server and desktop chips, but notably **NOT ARM64**. Apple Silicon, Ampere, Graviton and similar cloud ARM instances cannot use this integration.
- **Bare metal or VMs with RAPL passthrough.** Most cloud VMs (AWS EC2, GCP GCE, Azure VMs) do **not** expose RAPL counters to guest OSes. Kubernetes pods running on bare-metal nodes can access RAPL if the host exposes `/sys/class/powercap/intel-rapl/` into the container (requires privileged access or explicit mount).

**Why the Scaphandre branch does not cover ARM64.** Scaphandre upstream has tracked ARM support in [issue #35](https://github.com/hubblo-org/scaphandre/issues/35) since 2020 with no implementation. RAPL is an Intel interface adopted by AMD from kernel 5.11. ARM CPUs have no equivalent that exposes per-package energy counters via `/sys/class/powercap/`. The Scaphandre roadmap mentions an "estimation-based sensor" that would work on any architecture, but it remains unimplemented (last upstream activity on the topic: November 2023). On Graviton, Ampere, Apple Silicon and Raspberry Pi, the Scaphandre binary compiles for `aarch64` but the RAPL sensor fails at startup. perf-sentinel now ships two measured-energy sources that work on ARM: [Kepler precision bounds](#kepler-precision-bounds) (per-pod measured energy via eBPF) and [Redfish BMC precision bounds](#redfish-bmc-precision-bounds) (bare-metal wall-plug power). Both sit ahead of `cloud_specpower` in the precedence chain so ARM workloads get a real signal before the stack falls back to the CCF Graviton/Cobalt coefficients (±40%) and then to the I/O proxy.

On unsupported platforms, the `[green.scaphandre]` section is parsed and the scraper spawns, but it will fail to find the endpoint and silently fall back to the proxy model. A single warn-level log line is emitted at first failure so operators notice the misconfiguration.

**What Scaphandre improves.** The integration replaces the fixed proxy coefficient (0.1 µWh per I/O op) with a **service-level measured value** derived from the actual power consumption of the mapped process over the scrape window. Formula:

```
energy_per_op_kwh = (process_power_watts × scrape_interval_secs) / ops_in_window / 3_600_000
```

This captures:

- **Actual process power** (not an averaged approximation).
- **Per-service differences**: Java vs .NET vs Node vs Go will have different energy footprints even for similar I/O work.
- **Workload variance over time**: an idle service and a loaded service get different coefficients as the daemon runs.

Reports where at least one service used a measured coefficient are tagged with `model = "scaphandre_rapl"`. Full precedence chain: `electricity_maps_api` > `alumet_rapl` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`. When calibration factors are active on proxy models, the suffix `+cal` is appended (e.g. `io_proxy_v2+cal`). The `+cal` suffix never applies to a measured tag, the calibration multiplier targets the proxy coefficient and has no meaning once a measured reading replaces it.

**What Scaphandre does NOT do.** This is the critical limitation: **Scaphandre gives per-service coefficients, not per-finding attribution**. Specifically:

1. **RAPL is process-level, not span-level.** The metric `scaph_process_power_consumption_microwatts{exe="java"}` reports the total power draw of the `java` process. It cannot distinguish between two concurrent N+1 findings running in the same process at the same time. They share the coefficient by construction.
2. **Scrape interval is not the precision bottleneck.** A 5-second scrape window averages power over 5 seconds. Going to 1 second would not give you per-finding precision because RAPL itself averages at the 2s-Scaphandre-step granularity. The actual precision floor is "one coefficient per (service, scrape_window)".
3. **Concurrent services on the same process share nothing.** If your architecture runs multiple logical services in the same JVM, Scaphandre's `exe="java"` reading covers all of them together. perf-sentinel attributes the measured energy to whichever service name you mapped, which is a simplification.
4. **OS scheduler noise.** Per-process power attribution via `process_cpu_time / total_cpu_time` is inherently noisy under mixed loads.

**Correct mental model.** Scaphandre gives you a **dynamic, measured, service-level per-op coefficient** instead of a **fixed, proxied, global constant**. It is a meaningful improvement in the energy attribution layer of the carbon estimate stack, but it does not transform perf-sentinel into a regulatory-grade carbon accounting tool. The 2× multiplicative uncertainty bracket still applies.

**Staleness handling.** The daemon drops entries older than 3× the scrape interval when building the per-tick snapshot. A hung scraper or a service that stops emitting events will silently fall back to the proxy model after ~3 scrape intervals. The `perf_sentinel_scaphandre_last_scrape_age_seconds` Prometheus gauge lets operators set up Grafana alerts on scraper health.

**Batch mode.** `analyze` batch mode never spawns the scraper and never uses Scaphandre data. Even if `[green.scaphandre]` is present in the config, the `analyze` command skips it entirely and always uses the proxy model. Only the `watch` daemon integrates Scaphandre.

### Kepler precision bounds

perf-sentinel ships an opt-in integration with [Kepler](https://github.com/sustainable-computing-io/kepler) (CNCF sandbox) for per-container or per-process energy measurement via eBPF + CPU perf counters. When `[green.kepler]` is configured, the `watch` daemon scrapes Kepler's Prometheus `/metrics` endpoint, computes a joules delta per service vs the previous scrape, and publishes a measured per-op coefficient with `model = "kepler_ebpf"`.

**Platform requirements.**

- **Linux only**, any CPU architecture supported by Kepler (x86_64 and ARM64).
- **Kepler installed and exposing `/metrics`.** Production deployments typically run Kepler as a Kubernetes `DaemonSet` per node; in that case point the endpoint at the node-local pod or, more robustly, at an upstream Prometheus that scrapes the whole DaemonSet (Prometheus-mediated mode is reserved as a follow-up, this release ships direct-scrape only).
- **Kernel eBPF support** (kernel 5.4+ in practice).

**Why this branch covers ARM64 where Scaphandre does not.** Kepler does not depend on RAPL. On x86_64 with RAPL access it uses the same counters Scaphandre does, on ARM64 it falls back to an eBPF + perf-counter model that produces a real (degraded-precision) signal. The ARM eBPF model is less accurate than the x86 RAPL path, see [Kepler issue #1556](https://github.com/sustainable-computing-io/kepler/issues/1556) for the upstream tracking of known caveats (tracepoint failures, weaker DRAM model). For ARM workloads the alternative was the `cloud_specpower` proxy at ±40%, Kepler at degraded precision is still a meaningful improvement.

**What Kepler improves vs the proxy.** Same shape as Scaphandre: replaces the fixed `ENERGY_PER_IO_OP_KWH` constant with a measured per-service coefficient derived from the eBPF energy reading and the per-service op delta of the current scrape window. The reading flows through the precedence chain as `kepler_ebpf`, sitting between `scaphandre_rapl` (x86 RAPL, more precise) and `cloud_specpower` (CCF ±40%).

**What Kepler does NOT do.**

1. **Container / process granularity, not per-finding attribution.** Two N+1 findings in the same container during the same scrape window share the same coefficient by construction.
2. **The ARM eBPF model is meaningfully less accurate than the x86 RAPL path.** Treat ARM Kepler readings as a stronger signal than the proxy, not as a replacement for an external meter.
3. **DRAM coverage is partial on ARM.** Upstream Kepler does not yet expose per-process DRAM joules on every ARM SoC. Plan for periphery loss on top of the standard "RAPL captures roughly half to two thirds of wall-plug" caveat.
4. **No process-collector cross-pod sharing.** Services co-located in the same container share a coefficient. Map each service to its own `container_name` (or its `comm` value if you select `metric_kind = "process"`).

**Staleness handling.** Same `3 × scrape_interval` rule as Scaphandre, with a Prometheus gauge `perf_sentinel_kepler_last_scrape_age_seconds` for hung-scraper detection. The gauge is seeded at scraper-start time so a Kepler endpoint broken from boot still climbs the metric, Grafana alerts on a never-successful scraper fire reliably. A counter-reset (Kepler exporter restart) produces a negative delta which the guard drops (`delta > 0.0 && delta.is_finite()` filter, regular `f64` subtraction since `f64` has no `saturating_sub`), the next scrape produces the next meaningful delta.

**Batch mode.** Same shape as Scaphandre, `analyze` never spawns the Kepler scraper. Only `watch` integrates Kepler.

### Redfish BMC precision bounds

perf-sentinel ships an opt-in integration with the [Redfish](https://www.dmtf.org/standards/redfish) BMC standard for bare-metal wall-plug power readings. When `[green.redfish]` is configured with one or more chassis endpoints, the `watch` daemon polls each chassis's `/Power` resource for `PowerConsumedWatts`, distributes the chassis-level joules across mapped services proportional to their ops, and publishes per-service coefficients with `model = "redfish_bmc"`.

**Platform requirements.**

- **Bare-metal nodes with a BMC supporting Redfish 1.0+.** Dell iDRAC, HPE iLO, Lenovo XCC, Supermicro X11+ all qualify, as does the OpenBMC reference. Cloud VMs do not expose a BMC and cannot use this integration.
- **HTTPS reachable from the daemon to the BMC.** Most BMCs ship with self-signed certificates by default. **Operator-supplied CA bundle support is reserved for a follow-up release.** In this release, setting `ca_bundle_path` causes the scraper to refuse to start with a clear error. Operators with self-signed BMC certs must front the BMC with a reverse proxy that presents a publicly-signed cert (or use HTTP on a trusted network segment).
- **Basic auth credentials.** Redfish Session-token auth (POST `/SessionService/Sessions`) is not yet supported, the `auth_header` field carries a curl-style Basic line.

**What Redfish improves vs the proxy.** Real wall-plug power measurement at the chassis level. Unlike Scaphandre and Kepler (which see CPU + DRAM only via RAPL or eBPF), the BMC reads the actual power supply output, so the periphery (NIC, drives, fans, PSU overhead) is included by construction.

**What Redfish does NOT do.** This is the critical limitation of node-level power:

1. **Chassis granularity, not per-service or per-finding.** Every service mapped to the same chassis receives the **same** coefficient (`chassis_joules / sum_of_ops_deltas`) for a given scrape window. Two services on the same node will never get distinct measured coefficients via Redfish.
2. **No process-level attribution.** Idle processes still consume baseline power that gets allocated to the active services. Treat the per-service coefficient as an upper bound on what those services drew.
3. **No per-finding attribution.** Same as every other measured tag in the chain.
4. **Vendor variance in the JSON response.** Some BMCs return `null` or `0` for `PowerConsumedWatts` (or `PowerWatts.Reading` on the modern schema) during transition states (boot, fan ramp). perf-sentinel rejects null/zero/negative/NaN values as invalid and keeps the previous coefficient until a healthy reading lands. Vendor OEM paths (e.g. HPE's `Oem.Hpe.PowerSummary.Watts`) are no longer configurable: v0.7.6 typed the schema as an enum (`legacy_power` or `environment_metrics`) and dropped the operator-typed JSON pointer. OEMs that publish wattage at a non-standard path must front the BMC with a reverse proxy that re-shapes the payload to the standard schema.

**Schema choice and sensor smoothing.** Both supported schemas resolve to the same downstream `redfish_bmc` model tag, the choice is wire-shape only. Operators should know that the two paths typically expose different smoothing characteristics: `legacy_power` returns vendor-smoothed wattage (Dell iDRAC ~5s rolling average, HPE iLO 1-5s), whereas `EnvironmentMetrics.PowerWatts.Reading` is a `SensorPowerExcerpt` typed as a current-tick gauge. Switching schemas on a chassis preserves the long-window mean coefficient but tightens the variance histogram, expect more jitter on the `redfish_bmc` carbon-per-op series after migration. Pick `legacy_power` for fleet-wide compatibility today, `environment_metrics` for BMCs whose firmware documents it.

**Cumulative energy not yet read.** `EnvironmentMetrics` also exposes `EnergykWh.Reading` (cumulative kWh), which would enable a Kepler-style delta-integration coefficient that is strictly more accurate than `instantaneous_watts × scrape_interval` for long intervals or bursty loads. Today's parser only reads the instantaneous wattage gauge from both schemas. A future release may opt in to the cumulative reading once vendor coverage is broad enough to make it the default path.

**Rate-limit defense.** `scrape_interval_secs` is clamped to `[15, 3600]` for Redfish. Several BMCs (notably HPE iLO 4/5) rate-limit Redfish polling below 30 seconds, and many vendors cache the wattage internally on a 30s update cycle anyway, so a faster interval gains no information while risking 429 responses. Default is 60s.

**IPMI is out of scope.** Redfish is the modern standard, the IPMI path would require linking `ipmitool` or `freeipmi` C bindings which falls outside the "no heavyweight deps" rule. Document any IPMI-only fleet as a gap.

**Staleness handling.** Same `3 × scrape_interval` rule as the other measured sources, with `perf_sentinel_redfish_last_scrape_age_seconds` seeded at scraper start so the gauge climbs from boot even when no chassis has ever succeeded. The gauge is **aggregate**: it resets to zero as soon as any chassis succeeds in a tick. A multi-chassis fleet with one healthy BMC and several failing ones therefore reports `age = 0`, the per-chassis failure signal lives in `perf_sentinel_redfish_scrape_failed_total{reason=...}`. Pair both metrics in Grafana alerts that need per-chassis granularity.

**Batch mode.** Same shape, `analyze` never spawns the Redfish scraper.

### Cloud SPECpower precision bounds

#### Platform requirements.

- A Prometheus-compatible endpoint (Prometheus, VictoriaMetrics, Thanos) that already has CPU utilization metrics from cloud provider exporters (cloudwatch_exporter, stackdriver-exporter, azure-metrics-exporter) or node_exporter.
- perf-sentinel does NOT query cloud provider APIs directly. It reads from Prometheus.

#### What cloud SPECpower improves.

The proxy model uses a fixed energy constant for all I/O operations. Cloud SPECpower replaces this with a CPU-utilization-aware estimate per service:

```
watts = idle_watts + (max_watts - idle_watts) * (cpu_percent / 100)
energy_per_op_kwh = (watts / 1000) * (interval_secs / 3600) / ops_in_window
```

This captures workload-proportional power scaling, which the fixed proxy constant cannot.

#### What cloud SPECpower does NOT do.

1. **Per-finding attribution:** like Scaphandre, this is a per-service coefficient.
2. **Memory or I/O power:** the SPECpower data captures CPU and baseboard, not storage or network.
3. **Shared tenancy correction:** the model assumes the instance's full power is attributed to perf-sentinel's traced workload.

#### Single methodology after the 2026-04-24 refresh.

The embedded lookup table now follows a single homogeneous methodology: `idle_watts = vCPU * idle_per_vCPU_coefficient` and `max_watts = vCPU * max_per_vCPU_coefficient`, with coefficients sourced per provider from the Cloud Carbon Footprint `ccf-coefficients` 2026-04-24 snapshot. AWS, GCP, and Azure share this approach uniformly. The AWS baseboard overhead column from the 2023-05-01 snapshot is no longer published by CCF, so it is dropped uniformly. Where the previous `SPECpower_ssj 2008` direct compute (2024 Q1 - 2026 Q2) diverged from CCF by more than 5 percent on idle or max watts, the value was aligned to CCF for source-of-truth coherence (Sapphire Rapids, EPYC Genoa, Graviton 3/4). Modern entries whose direct compute stays within 5 percent of CCF, or whose architecture is absent from the provider CSV (Azure Emerald Rapids, Azure Genoa, GCP Turin, GCP Ampere Altra, Azure Cobalt 100), are kept on their existing SPECpower direct value and labelled explicitly in `table.rs`. **Consequence**: AWS legacy instances (`m5`, `c5`, `r5`, `m6i`) read lower than before because the baseboard overhead is no longer added on top; Sapphire Rapids instances (`m7i`, `c7i`, `r7i`, GCP `c3`) read higher because the CCF SPECpower aggregate is more recent than our 2024 Q1 direct sample.

#### Graviton 3/4 and Cobalt 100 are estimated, not measured.

AWS does not submit Graviton to SPECpower. Microsoft does not submit Cobalt 100. The 2026-04-24 CCF refresh maps Graviton 2 / 3 / 3E / 4 to its EPYC 2nd Gen coefficient (0.474 idle / 1.693 max W/vCPU) as a conservative proxy in the absence of measured data, so all Graviton generations share the same per-vCPU value. AWS publicly claims Graviton 4 is more efficient than Graviton 3, but no SPECpower submission exists yet to differentiate them. Cobalt 100 (Neoverse N2) is absent from the CCF Azure CSV and is kept on a midpoint blend 0.60/2.20 W/vCPU between Ampere Altra Q80-30 (Neoverse N1, SPECpower 2024 Q1, 0.67/1.75 W/vCPU floor) and the Graviton 3 V1 reference, pending direct Cobalt SPECpower data. These ARM values carry an additional layer of uncertainty: expect **+/-40% rather than +/-30%** for Graviton, Cobalt 100, and Ampere Altra-derived entries.

#### EPYC 5th Gen Turin is proxied to Genoa pending an upstream CCF correction.

The CCF 2026-04-24 entry for EPYC 5th Gen Turin is 3.682 idle / 8.961 max W/vCPU, roughly five times higher than the neighbouring EPYC 4th Gen Genoa (0.739 / 2.282) on the same row layout. The upstream SPECpower submission that feeds this row likely was measured at chip rather than thread granularity, or reflects a tiny sample that does not generalize. We override Turin (AWS `m8a` / `c8a`) to the Genoa coefficient instead of importing the CCF row verbatim: a silent 4x inflation on m8a customers would damage the directional waste-signal credibility of the tool, while a Genoa proxy is at worst conservative and at best correct since Zen 5 is supposed to be at least as efficient as Zen 4 per-thread. The override is tracked here for re-evaluation when CCF publishes a revised EPYC 5th Gen row or when independent SPECpower submissions for EPYC 9755 / 9655 land. Carry **+/-40%** uncertainty on Turin until then.

#### Memory-optimized SKUs carry an additive DRAM premium on top of the CPU coefficient.

CCF 2026-04-24 does not publish a memory-class premium so we layer one on top of the per-vCPU CPU coefficient for the memory-optimized families: `r5`, `r5a`, `r6i`, `r7i`, `r7a` on AWS, `n2-highmem-*` on GCP, and `Standard_E*` v3 through v6 on Azure. The premium is `0.02 W/GB` idle and `0.05 W/GB` max (Crucial DDR4 RDIMM datasheets, Boavizta DIMM model), and the 8 GB/vCPU memory ratio of those families gives a per-vCPU uplift of `+0.16` idle / `+0.40` max. This is one of two methodology departures from the CSV in the 2026-04-24 refresh (Turin override is the other) and is documented inline in `table.rs`. Memory-optimized r-series entries on AMD silicon (`r5a` on EPYC 1st Gen, etc.) get the same uplift as r-series on Intel because DRAM is independent of the CPU architecture. General-purpose families (`m5`, `m6i`, etc.) carry roughly 4 GB/vCPU of DRAM, compute-optimized families (`c5`, `c6i`, etc.) carry roughly 2 GB/vCPU. Neither receives the premium under the current rule, leading to idle under-counts of ~6-8 percent (m-series) and ~3-4 percent (c-series). Both stay inside the 2x uncertainty bracket, and we do not apply a half-premium to avoid compounding the methodology divergence from CCF.

#### Correct mental model.

Cloud SPECpower is an interpolation model with approximately +/-30% accuracy. It is a step up from the I/O proxy (order-of-magnitude estimate) but less precise than Scaphandre RAPL (direct hardware measurement).

#### Batch mode.

Cloud SPECpower is a daemon-only feature (`watch` mode). The `analyze` batch command always uses the proxy model.

### Per-operation energy coefficients

The per-operation energy multipliers (SQL verb weighting, HTTP payload size tiers) are heuristic estimates derived from academic DBMS energy benchmarks (Xu et al. ICDE 2010, Tsirogiannis et al. SIGMOD 2010) and the Cloud Carbon Footprint methodology. The relative ratios between operations (SELECT < DELETE < INSERT/UPDATE) are more robust than the absolute values, which vary across hardware generations and database engines.

Key limitations:

- **No query complexity analysis.** A full table scan SELECT costs more energy than an indexed point lookup, but both get the same 0.5x coefficient. The coefficients capture the average operation class, not the specific query plan.
- **HTTP payload size requires OTel attributes.** The `http.response.body.size` (or legacy `http.response_content_length`) attribute must be present on HTTP spans. When absent, the coefficient falls back to 1.0x (the base constant). Most HTTP instrumentation libraries do not emit this attribute by default.
- **Not used with measured energy.** When Scaphandre or cloud SPECpower provides measured per-service energy, the per-operation coefficients are ignored. This is by design: measured data is always more accurate than heuristic multipliers.

Set `per_operation_coefficients = false` to disable this feature and use the flat energy constant for all operations.

### Network transport energy

The optional network transport energy term estimates the energy cost of moving bytes between regions. The default coefficient (0.04 kWh/GB) is a conservative default below recent whole-network averages (Sustainable Web Design Model v4, 2024: 0.059 kWh/GB operational for networks) and an upper bound for cross-region server traffic, where inter-datacenter coefficients run as low as 0.001 kWh/Gb (Cloud Carbon Footprint).

Key limitations:

- **Wide estimate range.** Published values range from 0.06 to 0.08 kWh/GB depending on the study, year and scope (backbone only vs. full path). The actual cost depends on the number of hops, distance and infrastructure.
- **No CDN or compression effects.** Content delivery networks, HTTP compression and connection reuse all reduce the effective transport energy but are not modeled.
- **Cross-region detection is config-based.** The callee region is determined by looking up the target hostname in `[green.service_regions]`. If the hostname is not mapped, perf-sentinel conservatively assumes same-region (no transport term). This means transport energy is only computed when the user explicitly configures cross-region service mappings.
- **No last-mile modeling.** The estimate covers backbone transport. The energy cost of the last mile (edge network, client device) is excluded.
- **Linear proportionality assumption.** The kWh/GB model assumes energy scales linearly with data volume. Mytton et al. (2024) show this is a simplification: network equipment has a significant fixed baseload power regardless of traffic. The estimate is directional, not precise.
- **Response body only.** Only the response body size (`http.response.body.size`) is counted. The request body (e.g., large POST payloads) is not available in standard OTel HTTP semantic conventions and is excluded. For write-heavy APIs this underestimates transport energy.
- **Caller's grid intensity used for network.** Network infrastructure is distributed across many grids, but perf-sentinel uses the caller region's carbon intensity as a proxy. This is a known simplification consistent with the directional estimation approach.

The feature is disabled by default (`include_network_transport = false`) and must be explicitly opted into.

## Chatty service detection

The chatty service detector only counts HTTP outbound spans (`type: http_out`). A trace with 15 SQL calls to the same database is not "chatty" in the inter-service sense. The threshold is per-trace, not per-endpoint: a trace that fans out across 3 endpoints each making 6 calls (18 total) will trigger at the trace level even though no single endpoint is particularly chatty.

Chatty service findings are NOT counted as avoidable I/O in the waste ratio. They represent an architectural concern (service decomposition granularity), not a batching opportunity.

## Connection pool saturation detection

The pool saturation detector uses a heuristic based on SQL span timestamp overlap, not actual connection pool metrics. It computes peak concurrency by treating each SQL span as an interval `[start, start + duration]` and running a sweep-line algorithm.

Limitations:
- Timestamps from distributed tracing may have clock skew, leading to imprecise overlap detection.
- The detector cannot distinguish between actual pool contention and intentional parallel queries (e.g., scatter-gather patterns).
- For precise monitoring, instrument your application with OTel connection pool metrics (`db.client.connection.pool.usage`, `db.client.connection.pool.wait_time`).

Pool saturation findings are NOT counted as avoidable I/O.

## Serialized calls detection

The serialized calls detector flags sequential sibling spans (same `parent_span_id`) that call different services or endpoints and could potentially be executed in parallel. Severity is `info` to reflect the inherent uncertainty.

False positive considerations:
- Sequential calls to the same service MAY have legitimate data dependencies the tool cannot observe (e.g., "create user" then "send welcome email" where the email needs the user ID).
- The detector skips sequences where all calls share the same normalized template (that pattern is N+1, not serialization).
- The `parent_span_id` field must be present on spans for this detector to work. Traces without parent-child relationships (e.g., flat JSON ingestion without span IDs) will not trigger serialized findings.

The detector reports at most one finding per parent span: the single longest non-overlapping subsequence (found via dynamic programming). If a parent has two disjoint groups of serializable calls separated by overlapping spans, only the longest group is reported.

Serialized call findings are NOT counted as avoidable I/O. They represent a latency optimization opportunity, not an I/O reduction.

## Cross-trace correlation

Cross-trace temporal correlation (`[daemon.correlation]`) requires daemon mode (`perf-sentinel watch`) with sustained, representative traffic. Correlations are statistical: they detect temporal co-occurrences, not causal relationships. A high correlation between an N+1 in service A and pool saturation in service B means they frequently co-occur within the configured time lag, not that one causes the other.

Limitations:

- **Cold start.** The correlator needs time to accumulate enough observations. With `min_co_occurrences = 3` and a 10-minute window, you need at least 3 co-occurrences within 10 minutes before a correlation surfaces. Low-traffic environments may never reach this threshold.
- **Batch mode not supported.** The `analyze` command does not run the correlator. Cross-trace correlation is inherently a streaming concern.
- **Cardinality.** The `max_tracked_pairs` cap (default 1000) prevents unbounded memory growth. If you have many distinct finding types across many services, some pairs may be evicted before reaching the co-occurrence threshold.

To consume correlations:

- Run a daemon: `perf-sentinel watch --otlp-grpc 0.0.0.0:4317`.
- Query: `perf-sentinel query correlations`.
- Or open the dashboard generated by `perf-sentinel report` from a payload that includes correlations (only daemon-produced reports do).

Batch `analyze` always reports an empty correlations array. This is by design, not a bug.

## OTel source code attributes

Findings include a `code_location` field (with `function`, `filepath`, `lineno`, `namespace`) when the OTel spans carry the corresponding `code.*` attributes. This enables source-level annotations in SARIF reports (GitHub/GitLab inline annotations).

Limitations:

- **Most OTel auto-instrumentation agents do not emit `code.lineno` or `code.filepath`.** Manual instrumentation or agent-specific configuration is required. Without these attributes, findings appear without source location (no noise, graceful degradation).
- **`code.function` is the most commonly available attribute.** If only `code.function` is present, the CLI displays it but SARIF cannot produce a `physicalLocation` (which requires at least a file path).
- **Line numbers may be approximate.** Some agents report the method entry point, not the exact line of the I/O call.
- **Hostile `code.filepath` values are dropped from SARIF.** The OTel `code.filepath` attribute is attacker-controlled. Before emission as a SARIF `artifactLocation.uri`, perf-sentinel rejects URI-like strings, absolute paths, path traversal (literal and percent-encoded), double-encoded percent sequences, overlong UTF-8 prefixes, control characters and BiDi/invisible Unicode (Trojan Source class). Findings with rejected filepaths still appear in the report, only without `physicalLocations`.

## Daemon query API

The `perf-sentinel query` subcommand and the `/api/*` HTTP endpoints expose the daemon's internal state. The query API has no built-in authentication or authorization. Access control must be handled externally via network policies or a reverse proxy, same as the OTLP ingestion endpoints. See "No authentication" above.

- **Kill-switch.** Setting `[daemon] api_enabled = false` disables all `/api/*` routes while keeping OTLP ingestion and `/metrics` active. Use this when the daemon runs in an environment where even loopback exposure of findings is unacceptable. Note that `/metrics` still exposes finding counts via `perf_sentinel_findings_total` and related counters, so the query API flag does not remove all observable output.
- **Memory is not reclaimed by `api_enabled = false` alone.** The `FindingsStore` ring buffer is still populated each tick even when the API is disabled, because detection runs before the API check. To reclaim that memory, set `[daemon] max_retained_findings = 0`. This short-circuits the store's `push_batch` and keeps the daemon's RSS minimal when the query API is off.
- **Response size caps.** `/api/findings` caps at 1000 entries per request (`?limit=` parameter is clamped). `/api/correlations` truncates to the top 1000 by confidence. These caps protect against expensive large-response requests when the daemon has built up a large memory footprint.
- **Retained findings are bounded.** The `FindingsStore` ring buffer (default 10,000 findings) evicts the oldest entries when full. For high-traffic daemons, increase `max_retained_findings` or accept that older findings will not be queryable.
- **No persistence.** The daemon stores findings in memory only. A restart clears all retained findings and correlation state. For investigating traces older than the 30-second live window (production incidents looked at after the fact), see [RUNBOOK.md](RUNBOOK.md).

## Automated pg_stat ingestion from Prometheus

The `--prometheus` flag on `pg-stat` scrapes metrics exposed by `postgres_exporter`. This requires:

- A running `postgres_exporter` instance configured to collect `pg_stat_statements` metrics.
- The Prometheus endpoint must be reachable from the machine running perf-sentinel.
- Only the metrics available in the Prometheus exporter are used. Some fields present in the raw `pg_stat_statements` view (e.g. `blk_read_time`, `blk_write_time`) may not be exposed by all exporter versions.

The existing `--input` file path mode is unchanged and remains the recommended approach for CI pipelines.

## Secrets and credentials

perf-sentinel never stores secrets in config output. For scrapers that need credentials, the env-var-preferred pattern applies across the board:

- **Electricity Maps API key**: `PERF_SENTINEL_EMAPS_TOKEN` env var. A `[green.electricity_maps] api_key` in the config file works but emits a warning at load time, because checked-in config files are a common source of accidental credential leaks.
- **PostgreSQL connection string** for `pg-stat --connection-string`: `PERF_SENTINEL_PG_CONNECTION` env var. Passing a connection string with a plaintext password on the CLI also works but emits a warning (recommend `.pgpass` for production).
- **Scraper endpoint URLs** (Scaphandre, cloud energy, Electricity Maps, pg-stat Prometheus): credentials in the URL (`http://user:pass@host`) are rejected at config load. Use the scraper's native auth mechanism instead.
- **TLS key file**: `[daemon] tls_key_path` permissions are checked at startup; a world- or group-readable key emits a warning.

The daemon never writes secrets to stdout/stderr: all scraper error paths use `redact_endpoint` to strip userinfo from any URL before logging.

When the daemon runs with `api_enabled = true`, the query API exposes findings (not secrets) but has no authentication. Restrict loopback access via network policies or a reverse proxy or set `api_enabled = false` to disable the API surface entirely.

## Electricity Maps API

- **API key required.** The Electricity Maps integration requires an API key (free or paid tier). The key should be provided via the `PERF_SENTINEL_EMAPS_TOKEN` environment variable rather than in the config file.
- **HTTPS strongly recommended.** When the configured endpoint is `http://` (cleartext) and an auth token is set, perf-sentinel emits a warning at config load. The Electricity Maps production API is served over HTTPS only; an `http://` endpoint is almost always a misconfiguration or a local test setup.
- **Rate limits.** The free tier allows approximately 30 requests per month per zone. With the default `poll_interval_secs = 300` (5 minutes), this budget would be exhausted in under 3 hours. Free tier users should set `poll_interval_secs = 3600` or higher or use the embedded hourly profiles instead.
- **Daemon only.** The Electricity Maps scraper runs only in `perf-sentinel watch` mode. Batch mode (`analyze`, `tempo`, `calibrate`) uses the embedded profiles.
- **Staleness fallback.** If the API is unreachable for longer than 3x the poll interval, the scraper falls back to embedded hourly or annual profiles.

## Tempo ingestion

- **Protobuf format.** The `perf-sentinel tempo` subcommand requests traces as OTLP protobuf from Tempo's HTTP API. Tempo must be configured to serve protobuf responses (the default).
- **Parallel fetch concurrency cap.** The search-then-fetch flow (`--service --lookback`) fetches matching trace bodies in parallel via a `tokio::task::JoinSet`, capped at 16 in-flight requests through an internal semaphore. The cap is not currently user-configurable. Per-fetch timeout is 30s (vs. 5s for the search step) to allow a wide fanout trace body to be assembled from ingesters and long-term storage. On a capacity-constrained Tempo deployment with long lookback windows (e.g. 24h), some fetches may still time out. Remedy is Tempo-side: scale `tempo-query-frontend` replicas, tune `max_search_duration` and `max_concurrent_queries`.
- **Ctrl-C preserves partial results.** Interrupting a long parallel fetch aborts every in-flight task and returns whatever traces had already completed. The CLI surfaces the dedicated `TempoError::Interrupted` error if zero traces completed before the signal, so CI quality-gate paths can distinguish an operator abort from a genuine empty result (`NoTracesFound`).
- **Search API.** The search mode uses Tempo's `GET /api/search` endpoint which may not be available on all Tempo deployments (requires the search feature to be enabled in Tempo).

## gCO2eq energy constant (legacy section, kept for cross-references)

The carbon estimation uses a fixed energy constant (`0.1 uWh per I/O operation`) as a rough order-of-magnitude approximation. See **Carbon estimates accuracy** above for the complete methodology and disclaimer.

## pg_stat_statements ingestion

- **No trace correlation.** `pg_stat_statements` data has no `trace_id` or `span_id`. It cannot be used for per-trace anti-pattern detection (N+1, redundant). It provides complementary hotspot analysis and cross-referencing with trace-based findings.
- **CSV parsing.** The CSV parser handles RFC 4180 quoting (double-quoted fields, escaped `""`), but assumes UTF-8 input. Non-UTF-8 files will fail to parse.
- **Pre-normalized queries.** PostgreSQL normalizes `pg_stat_statements` queries at the server level. perf-sentinel applies its own normalization on top for cross-referencing, which may produce slightly different templates.
- **No direct PostgreSQL connection.** In file mode (`--input`), perf-sentinel reads exported CSV or JSON files. The `--prometheus` flag scrapes `postgres_exporter` metrics instead of connecting to PostgreSQL directly. See "Automated pg_stat ingestion from Prometheus" above for Prometheus-specific limitations.
- **Entry count.** The parser pre-allocates memory based on input size, capped at 100,000 entries. Files exceeding 1,000,000 entries (CSV rows or JSON array elements) are rejected with an error to prevent memory exhaustion.
