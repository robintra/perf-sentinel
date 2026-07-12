# Changelog

All notable changes to perf-sentinel are documented in this file. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Version numbers follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- `slow_sql` now fires for services instrumented by the PHP OTel contrib packages (Symfony + Doctrine + PDO). Those packages split each query across sibling spans under the request: a `SELECT orders` span carries the statement (`db.query.text`) at ~0 ms while its `Doctrine::execute` sibling carries the real duration with no statement, each layered over a pdo child that repeats the same shape (`db.system.name` sits only on that pdo layer, not on the doctrine spans). Before, OTLP conversion dropped every duration-bearing span as `missing_db_statement`, so all SQL events for such a service lasted ~0 ms and `slow_sql` could never fire regardless of thresholds, while the duplicated statement spans raised spurious `redundant_sql` findings. A stitch pass at OTLP conversion now re-joins each query: a statement-less span whose name suggests query execution (`execute`, `query`; connect/commit/transaction spans keep today's filtering) adopts the statement of the nearest related statement-bearing span (sibling or ancestor/descendant, same trace, donors reusable for the prepare-once-execute-many pattern), and layered duplicates collapse. A statement now identifies a donor on its own (`db.statement`/`db.query.text`), so the doctrine layer participates without a `db.system`; a statement-less span with no `db.system` is admitted only when it has a statement-bearing sibling, so ORM logical-op spans wrapping their own SQL child (Ruby ActiveRecord) do not adopt a descendant's statement. Merged spans are counted under the new `merged_db_span` reason of `perf_sentinel_otlp_spans_filtered_total`, excluded from the daemon zero-retention warning. Fail-open: single-layer emitters (Laravel/PDO, Django/psycopg, ActiveRecord) are byte-identical to before, and a span pair split across collector batches degrades to the previous `missing_db_statement` behavior. On layered-instrumentation fleets SQL event counts can also decrease (the duplicate statement events collapse into one), so IIS and waste-ratio baselines shift in the correct direction.

## [0.9.8]

### Added

- OTLP ingest now admits RPC spans (OTel RPC semconv: `rpc.system`, `rpc.service`, `rpc.method`), the standard shape for gRPC and most RPC frameworks. Before, these spans carried neither `db.statement`/`db.query.text` nor `http.url`/`url.full`, so the I/O filter dropped every one as non-I/O and the topological detectors (`excessive_fanout`, `chatty_service`, `serialized_calls`) plus the occurrence detectors (`n_plus_one_http`, `redundant_http`) were blind on gRPC-heavy fleets. RPC spans are keyed on `rpc.system` and modeled as outbound calls: the target is `rpc.service/rpc.method`, falling back to the span name when either key is absent. Only `SpanKind::Client` is admitted, since `rpc.*` is set on the inbound SERVER handler span too and admitting it would double-count every hop. Admission-only reuse of `EventType::HttpOut`, so the normalize/sanitize path and the finding types are unchanged. RPC spans carry no query text, so `n_plus_one_sql` never applies, and their findings surface under the `_http` types.

### Changed

- **Breaking (HTTP acknowledgment signatures reset).** The HTTP normalizer now keeps the callee host in the grouping template for DNS-addressed calls (`GET user-svc/api/users/{id}` instead of `GET /api/users/{id}`). Before, scheme and authority were stripped unconditionally, so two calls to the same path on different backend services (`http://ms-a/x` and `http://ms-b/x`) collapsed into one template and raised a false `redundant_http` finding with misleading cache/deduplicate advice. Verified on real production topology (Alibaba cluster-trace-microservices-v2022): 10 of the redundant findings on a 300-trace slice were host-strip fusions across distinct callees. IP-literal authorities (IPv4 and bracketed IPv6) are still stripped so load-balanced pod replicas keep deduping, the port and RFC 3986 userinfo are dropped, a trailing DNS root dot is canonicalized (`svc.` == `svc`), and the host is lowercased. Because the finding signature hashes `pattern.template`, every outbound-HTTP finding's signature changes, so existing HTTP acknowledgments must be re-created (SQL and relative-URL findings are unaffected). RPC findings (also modeled as `HttpOut` with a host-less `service/method` target) are unchanged.

## [0.9.7]

### Fixed

- Batch `analyze --input` now accepts OTLP/JSON attributes whose value is an empty list. Canonical protobuf JSON omits empty repeated fields, so an empty list serializes as `{"arrayValue":{}}` (and an empty map as `{"kvlistValue":{}}`) with no `values` key. Before, a single such attribute failed the whole file with `missing field values` and exited non-zero, so nothing was analyzed. The OTLP/JSON reader keeps the strict typed parse as the fast path and backfills the omitted empty list only for the affected document, so files from compliant producers (for example the OpenTelemetry Astronomy Shop demo's recommendation service) analyze cleanly (#81).

## [0.9.6]

### Changed

- The nationally-gridded rows of the embedded carbon-intensity table switch source from Electricity Maps consumption-based 2023-2024 means to Ember yearly generation-based national data (latest year per country), regenerated semiannually by the new `refresh-datasets` workflow through a reviewed PR. Subnational rows (North America, Brazil BR-CS) stay hand-maintained. Low-carbon grids move the most (Sweden 8 -> 35.4, Finland 8 -> 57.5, Norway 7 -> 28.1 gCO2eq/kWh): that is the generation-vs-consumption accounting difference, documented in `docs/METHODOLOGY.md`. Hourly profiles are renormalized to the new annual levels, shapes unchanged. `SPECPOWER_VINTAGE` is unchanged, so operator TOMLs pinning `specpower_table_version` are unaffected.
- The SPECpower instance table is now generated by `scripts/refresh-instance-power.py` from the CCF coefficient CSVs. The m6a/c6a families align to the CCF EPYC 3rd Gen coefficient (previously kept on SPECpower direct compute, inside the documented 5 percent equivalence), and 26 rows lose hand-rounding slips of at most 0.1 W.

## [0.9.5]

### Added

- Batch input auto-detects OTLP JSON. `analyze --input` (and the whole `--input` family: `diff`, `explain`, `inspect`, `report`, `calibrate --traces`) accepts an `ExportTraceServiceRequest` in the protobuf JSON mapping (camelCase keys, hex trace/span ids), both as a single object and as the OpenTelemetry Collector `file` exporter's NDJSON (one request per line, decoded through a serde stream deserializer). Conversion reuses the exact daemon OTLP path, so dd-trace bridged through the `datadogreceiver` now has a batch file route with no Tempo/Jaeger backend in between.
- New `mysql-stat` subcommand: ranks MySQL SQL hotspots from a CSV or JSON export of `performance_schema.events_statements_summary_by_digest`, the MySQL twin of `pg-stat`. Four rankings (total time, calls, mean time, rows examined), picosecond timer columns converted to milliseconds at parse time, case-insensitive column matching, `SCHEMA_NAME` NULL handling, and optional `--traces` cross-referencing against trace findings.
- The self-contained HTML dashboard gains a `mysql_stat` tab (`report --mysql-stat <file>`, optional `--mysql-stat-top <N>`) with the same ranking sub-switcher, text filter, CSV export and copy-link controls as the `pg_stat` tab.
- New opt-in daemon memory-pressure admission control: `[daemon] memory_high_water_pct` (0 disables, the default). A 1 Hz watcher reads the cgroup v2 memory usage (`memory.current / memory.max`) and, once it crosses the configured high-water mark, the OTLP handlers reject ingest with a retryable status (HTTP 503, gRPC `UNAVAILABLE`, counted on `perf_sentinel_otlp_rejected_total{reason="memory_pressure"}`) until usage falls back below it with hysteresis. This bounds daemon RSS independently of queue depth, closing a gap where a traces/sec flood the analysis worker keeps up with could grow the trace window past the container memory limit and get the pod OOMKilled before any queue-depth shedding fired. Linux/cgroup-v2 only, inert and zero-overhead elsewhere. The `tuning` advisor gains a matching rule pointing at the container memory limit.

### Changed

- The workspace toolchain moves to Rust 1.96.1 (fixes a MIR miscompilation and three libssh2 CVEs in Cargo's vendored dependencies).

## [0.9.4]

### Added

- Framework-aware `suggested_fix` now covers PHP. Findings on Laravel/Eloquent and Symfony/Doctrine stacks, detected via the native OpenTelemetry PHP scopes (`io.opentelemetry.contrib.php.laravel`, `io.opentelemetry.contrib.php.doctrine`) or a `.php` source path, carry framework-specific remediation (Eloquent `with()` / `load()` eager loading, Doctrine DQL fetch-join), with a PHP generic fallback across all ten anti-patterns. The Laravel scope is app-wide so PhpLaravelEloquent answers every anti-pattern, while the DB-specific Doctrine scope carries only the SQL fixes. PHP `\`-separated namespaces are recognized by the framework matcher and the ingest-time namespace derivation. dd-trace-php bridged through the Collector `datadogreceiver` carries no framework signal, so those findings fall to the PHP generic tag or stay unenriched.
- The sanitizer-aware N+1 classifier recognizes the Laravel and Doctrine OTel scopes as ORM markers, so an obfuscated Eloquent/Doctrine N+1 classifies as `n_plus_one_sql` under the default `auto` mode, at parity with the JVM, Ruby and Node stacks.
- The self-contained HTML dashboard highlights PHP `$variables` in `suggested_fix` recommendation snippets (the generic code highlighter is now PHP-aware).

## [0.9.3]

### Added

- OTLP ingestion now extracts SQL from dd-trace traces bridged through the OpenTelemetry Collector `datadogreceiver`. When `db.statement` is absent, the query is read from the Datadog resource (`dd.span.Resource`), gated on a database signal (`db.system.name`, `db.system`, or the dd-trace `db.type` meta key) so HTTP spans are never misread and non-SQL datastores (Redis, MongoDB, ...) are still dropped. This lets teams on Datadog with no OpenTelemetry instrumentation feed perf-sentinel without changing application code.
- OTLP ingestion recognizes the stable OTel 1.27+ `db.system.name` attribute, not only the older experimental `db.system`, so SQL spans from current OpenTelemetry SDKs and the datadogreceiver are correctly identified.

## [0.9.2]

### Added

- Framework-aware `suggested_fix` now covers Ruby. Findings on Active Record stacks, detected via the `OpenTelemetry::Instrumentation::ActiveRecord` instrumentation scope or a `.rb` source path, carry `includes` / `preload` / `eager_load` remediation, with a Ruby generic fallback across all ten anti-patterns.
- The SQL normalizer tokenizes MySQL backtick-quoted identifiers (`` `col` ``), preserving them verbatim so digits inside an identifier are no longer extracted as literals.

### Changed

- Spans whose `db.system` names a non-SQL datastore (Redis, Memcached, MongoDB, Cassandra, DynamoDB, Couchbase, CouchDB, Elasticsearch, OpenSearch, Neo4j, HBase, Geode, InfluxDB) are dropped at ingestion across the OTLP, Jaeger and Zipkin paths instead of being fed to the SQL tokenizer, which avoids false `n_plus_one_sql` and `redundant_sql` findings on cache and document traffic. The drop is gated on `db.system` alone and counted under a dedicated `non_sql_datastore` reason on `perf_sentinel_otlp_spans_filtered_total`, kept out of the daemon zero-retention warning so a cache-only fleet no longer raises a phantom instrumentation-gap alert. These spans are not modeled, so they also do not feed the fanout and serialized-calls detectors.

### Fixed

- The self-contained HTML report no longer embeds the raw `db.statement` in its traces payload. The embedded-traces block carried `event.target` verbatim, leaking SQL literals (for example `ARRAY['secret', 'pii']`) into the file even though the displayed template was masked. Only the masked template is embedded now. JSON, SARIF, CLI and Prometheus exemplars were already unaffected.

## [0.9.1]

### Security

- Updated `opentelemetry_sdk` to 0.32.1, resolving CVE-2026-48504 (unbounded memory allocation in W3C Baggage propagation). This is a transitive dependency bump with no behavior or API change.

## [0.9.0]

### Changed

- The self-contained HTML dashboard is rebuilt as an application shell. A sidebar navigates the sections, a new Overview landing page gathers the gate verdict, KPI cards, top findings and the diff and carbon rails, and the Findings view becomes a master/detail pane with the Explain trace tree folded inline. KPI cards are tinted by their severity band, SQL, code and endpoints are syntax-highlighted, and the Geist font family is embedded. The report stays a single offline file under the same strict CSP.
- Remediation suggestions wrap code tokens in backticks so the dashboard renders them as inline code.

### Added

- A `local_batch` confidence tag separates a local `analyze` run from a CI one, driven by the `CI` environment variable and reported in the `confidence` field next to the existing batch and daemon tags.

## [0.8.14]

### Changed

- The HTML dashboard dark theme darkens the secondary panel background from `#2c2c2c` to `#212121`, giving metric cards, rows and the topbar more contrast against the primary background. Cosmetic, dark theme only.

## [0.8.13]

### Added

- The carbon report emits `co2.sci_per_trace`, the SCI v1.0 per-functional-unit intensity `((E × I) + M) / R` with R = 1 trace, alongside the existing `co2.total` numerator footprint. The functional unit is declared on `co2.functional_unit` and the new estimate carries the methodology tag `sci_v1_intensity`. The SCI specification permits average grid intensity, so the figure is SCI-conformant.
- Each detector type maps to the RGESN 2024 criteria it relates to via `FindingType::rgesn_criteria()`, surfaced per anti-pattern in the periodic disclosure (`applications[].anti_patterns[].rgesn_criteria`) and documented as a crosswalk table in `docs/METHODOLOGY.md`. This is an interpretive mapping, not a compliance certification, and `slow_*` carries no criterion (RGESN family 9 is machine-learning specific).
- The periodic disclosure schema gains `methodology.standard_crosswalk`, an interpretive ESRS E1 datapoint crosswalk (energy to E1-5, operational carbon to E1-6 Scope 2 location-based, embodied carbon to E1-6 Scope 3) with in-band caveats, bumping the disclosure schema to `perf-sentinel-report/v1.3`. Both schema additions are backward compatible: older readers and reports stay valid and an older report keeps its `content_hash` when re-hashed.

## [0.8.12]

### Fixed

- The `query monitor` Trends tab now plots its Energy, Carbon and headroom charts in a fixed-width time window instead of compressing the curves as history accumulates. The x-axis spans a full window of points behind "now" from the start, so each new sample scrolls the curves leftward at a constant rate and the time-span axis labels stay put. Before the window fills, its left part stays empty rather than showing a zoomed-in view of the few points collected so far. CLI/TUI-only change, no daemon behavior change.

## [0.8.11]

### Added

- The interactive TUIs gain mouse-drag-resizable panels. In `inspect` (and `analyze --tui` / `demo --tui`) the borders between the Traces, Findings, Correlations and Detail panels can be dragged with the mouse to redistribute space, and in `query monitor` the Trends tab resizes its Energy/Carbon split and its charts/headroom split the same way. Press `m` to toggle mouse mode (mouse capture is opt-in, so native terminal selection and copy-paste stay available while it is off), drag a border to resize, and `r` to reset the layout to its defaults. Hovering a border highlights it with a heavy accent line and a handle glyph at the grab point, the in-app stand-in for a resize cursor since a terminal application cannot change the OS mouse pointer. Panel sizes are per-session and not persisted. CLI/TUI-only change, no daemon behavior change.

## [0.8.10]

### Added

- `perf-sentinel demo` gains `--tui` and `--html`. The bundled demo can now open the interactive TUI report or write the self-contained HTML dashboard, in addition to the colored terminal report. The `--html` output is a full showcase: every dashboard tab is populated from embedded fixtures, with findings spanning all detector types, Explain span trees, the GreenOps summary, a pg_stat ranking, a Diff against an embedded baseline run, and synthesized cross-trace correlations. Live ack/revoke stays daemon-only.
- The HTML dashboard header now renders the perf-sentinel wordmark logo (embedded inline, with theme-aware light and dark variants) as a link to the project repository, replacing the plain-text brand. Applies to every HTML report, not just the demo.
- The interactive TUI (`inspect`, `analyze --tui`, `explain --tui`, `demo --tui`) shows a centered `Powered by perf-sentinel` credit with the repository link pinned to the bottom of every view, mirroring the HTML dashboard footer. The brand name and link are green (the link underlined where the terminal supports it) and the muted text adapts to light and dark terminals.

### Changed

- The embedded demo dataset now exercises all ten detector types across the three severity levels, so the demo report (and the TUI/HTML showcase built from it) reflects the full detection surface.
- The Inspect view gives the Correlations panel more width by narrowing the Findings panel. When the Findings column is too slim for the full "[acked by <who>]" ack suffix it collapses to a compact "[acked]" marker, so the ack status stays visible in narrow terminals.
- Dark-mode contrast tuned across all HTML reports: lighter muted (secondary and tertiary) text, more visible control borders and elevated surfaces, and the footer "perf-sentinel" credit link now uses the success-green accent.


## [0.8.9]

### Fixed

- `perf-sentinel demo` now annotates its quality gate verdict as informational. The demo never enforces the gate (it has no `--ci` and always exits 0), but its last line read "Quality gate: FAILED" in red and looked like an error to a first-time user. The line now reads "Quality gate: FAILED (informational in demo, would exit 1 under analyze --ci)". The annotation is console-only, demo-only and limited to the failed state. The `analyze`, `tempo` and `jaeger-query` renderers and every machine export (JSON, SARIF, HTML, NDJSON) are unchanged.

### Documentation

- The crates.io install command in both READMEs and the distribution-strategy notes now recommends `cargo install perf-sentinel --locked` for reproducible fresh installs.


## [0.8.8]

Adds `perf-sentinel query monitor`, a read-only live operator TUI for a running daemon, backed by two new daemon endpoints, an extended status endpoint, and six new Prometheus gauges with matching Grafana panels. Also lightens the self-contained HTML dashboard and hardens terminal output against control-sequence injection. No daemon wire protocol change, no breaking config change.

### Added

- `perf-sentinel query monitor`, a live operator TUI separate from the developer-facing `inspect` drill-down. Four tabs cycled with Tab: Advisor (the daemon's `warning_details` settings hints, color-coded by kind), Energy (the effective energy/carbon mix: source per service with measured share, grid intensity per region tagged cold embedded vs hot scraped), Trends (live charts, below), and Scrapers (live backend health). A background poller refreshes from the daemon every `--refresh` seconds (default 5, range 1-3600); when the daemon becomes unreachable the last good snapshot stays on screen with a stale indicator. Read-only, no API key needed.
- The monitor's Trends tab plots the poll history as braille charts (native ratatui `Chart`, no new dependency): energy and carbon per scoring window side by side, and a Headroom chart showing each runtime gauge (`active_traces`, `analysis_queue_depth`, `stored_findings`) as a percentage of its configured cap with the settings advisor's 90% threshold drawn as a reference line, so an operator sees a knob approaching its cap before the advisor hint fires. One point lands per refresh tick into a 240-point client-side ring (20 minutes at the default 5 s); the history lives in the monitor only. Against a pre-0.8.8 daemon the Headroom panel degrades to a hint.
- `GET /api/energy` on the daemon query API: live health of the five energy/intensity backends (configured flag frozen from the `[green]` startup config, last scrape age, scrape success/failure counters where the backend has them). Optional fields are omitted rather than zeroed for unconfigured backends, so a pre-registered zero gauge cannot read as a fresh scrape. The effective mix itself stays on `/api/export/report`.
- `GET /api/status` gains the gauge/capacity pairs backing the Headroom chart: `max_active_traces`, `analysis_queue_depth`, `analysis_queue_capacity` and `max_retained_findings` alongside the existing gauges. Additive fields, older clients keep parsing.
- A Config tab on `query monitor`, backed by the new read-only `GET /api/config`: every effective `[daemon]` parameter with its current value, its compiled-in default (computed client-side) and a one-line explanation of what it does, with values differing from the default flagged `modified`. The endpoint is an explicit allowlist, never a blanket serialization of the internal config, so no secret leaks: TLS cert/key paths and the ack API key are summarized to booleans (`tls_configured`, `ack_api_key_set`) and never echoed. Against a pre-0.8.8 daemon the tab shows a hint.
- Six Prometheus gauges on `/metrics` carry the Trends data to Grafana: `perf_sentinel_energy_kwh` and `perf_sentinel_carbon_gco2` (the window's energy and operational carbon as scalar totals; the per-service/region breakdown stays off `/metrics` for cardinality and lives on the monitor), `perf_sentinel_stored_findings`, and the three configured caps `perf_sentinel_max_active_traces`, `perf_sentinel_analysis_queue_capacity`, `perf_sentinel_max_retained_findings` (set once at startup) so a dashboard can compute each runtime gauge as a percentage of its cap. The bundled `examples/grafana-dashboard.json` gains three panels (energy per window, carbon per window, runtime headroom with the 90% advisor line).

### Changed

- Both TUIs (`query inspect` and the new `query monitor`) now follow the terminal theme, so light and dark backgrounds both read well, and they restore the terminal idempotently across the panic hook and the RAII guard.

### Fixed

- The cloud-energy scraper now advances its staleness gauge on a failed scrape, matching the Kepler scraper's behavior.

### Performance

- The HTML dashboard embeds a slimmed report copy: `per_endpoint_io_ops` is dropped from the embed (no dashboard view reads it) and `green_summary.top_offenders` is capped to 25 rows (the dashboard renders only the top entry), while `analyze --format json` keeps both in full. On a 15000-finding oversized report this brings the file from 6.4 MB back under the 5 MB target and frees enough budget to embed ~950 traces where 0 fit before. Trace ranking and the trim banner still read the un-slimmed data, so neither degrades past the caps.

### Security

- `sanitize_for_terminal` now also strips BiDi reordering marks and invisible characters (the Trojan-Source class), consolidated out of the SARIF helper so every terminal and HTML sink shares one sanitizer. The monitor's Config tab routes daemon-controlled strings (the listen address, environment and CORS origins) through it, so a hostile daemon cannot inject escape sequences into the operator's terminal.

### Documentation

- `INSPECT.md`, `QUERY-API.md`, `METRICS.md` and the CLI and design docs document the monitor tabs, the new endpoints and the gauges, with the French mirrors updated in lockstep. Both READMEs showcase the monitor demo, and the inspect and all-in-one TUI stills were refreshed.

## [0.8.7]

Closes the two 0.8.6 observability follow-ups (OTLP span retention counters, shed-exclusion proof), applies a primary-source audit of the embedded carbon constants, and hardens the daemon and batch CLI against the limits exposed by the new high-scale lab scenarios (wide service topologies, 100k+ trace corpora). Four embedded grid values change, so carbon outputs shift for the affected regions. No daemon wire protocol change, no breaking config change.

### Added

- `perf_sentinel_otlp_spans_received_total` and `perf_sentinel_otlp_spans_filtered_total{reason}` (reasons `not_io`, `missing_db_statement`, `missing_http_url`, pre-warmed to 0) expose the span retention ratio of the deliberate I/O filter. A fleet whose instrumentation strips `db.statement` or `http.url` used to convert every OTLP request to zero events with no signal anywhere; the counter pair makes that visible (`spans_received` rising while `events_processed_total` stays flat).
- `perf_sentinel_service_io_ops_overflow_total` counts I/O ops that received no per-service attribution because the 1024-service cardinality cap was reached. The warn still fires once, the counter moves on every unattributed op, so ongoing undercounting of per-service throughput and measured-energy attribution is observable.
- `perf_sentinel_correlator_pairs_evicted_total` counts cross-trace correlator pairs evicted by the `max_tracked_pairs` cap, with a bounded warn (eviction is amortized per 10% of the cap). Correlations disappearing from `/api/correlations` between reads now have a signal.
- An inline daemon test proves shed traces never reach analysis outputs (findings store stays empty for the shed trace while counters move), closing the 0.8.6 shed-test follow-up.
- A settings advisor: when lifetime counters show a config knob undersized for the observed load, `/api/export/report` emits `tuning` entries in `Report.warning_details` naming the knob, its current value and the suggested adjustment. Six rules cover analysis-queue sheds, ingest-queue rejects, a near-full trace window, the per-service metering cap, correlation-pair evictions, and zero analyzable-span retention (spans keep arriving and not one is analyzable; a high not_io share alone is healthy on a fleet exporting all its spans). Complements the static comfort-zone warnings at startup with runtime evidence.

### Changed

- Carbon data refresh against primary sources (Electricity Maps consumption-based 2023-2024, corroborated by Ember): Paris regions (`eu-west-3`, `europe-west9`, `francecentral`, `fr`) move from 56 to 41 gCO2eq/kWh, Sao Paulo (`sa-east-1`, `br`) from 62 to 96, Belgium (`europe-west1`, `be`) from 187 to 165. The matching hourly profiles are rescaled to the same levels with their diurnal and seasonal shapes preserved.
- The `eu-central-1` hourly profile is rescaled from a grand mean of ~431 to ~341 gCO2eq/kWh, resolving the historical ~28% divergence from the annual table value (338). The audit inverted the documented story: the German grid got cleaner through 2023-2025, the profile was the stale side (frozen at the 2022 coal-crisis level), not the annual value. Hourly-profile reports for Frankfurt drop by roughly 21% as a result, and the profile-vs-annual ±5% invariant now holds for every region with no exception.
- `GENERIC_PUE` rises from 1.2 to 1.5, tracking the Uptime Institute Global Data Center Survey weighted average (1.54 in 2025, flat for six years). The Generic bucket only covers self-hosted, colocation and country-code regions, for which the industry survey average is the defensible prior; hyperscaler regions keep their own provider PUE (AWS 1.15, GCP 1.09, Azure 1.17, all confirmed against the providers' current official figures).

### Fixed

- The cross-trace correlator admission-controls new pairs inside a batch. Previously the `max_tracked_pairs` cap was only enforced at batch end, so one batch of findings from a wide topology (hundreds of distinct services, almost every pair new) could insert millions of pair entries before the first eviction ran, and the map's high-water capacity was never returned to the allocator: at 1500 services the daemon OOM-killed its 256Mi pod in about a minute. With admission control the same load holds a flat ~57 MiB RSS. Pairs refused at the cap are folded into `perf_sentinel_correlator_pairs_evicted_total` (counted as distinct pairs per batch, not once per matching occurrence), and a batch that hit refusals still evicts the lowest-co-occurrence incumbents down to 90% of the cap at batch end, so fresh pairs are admitted on the next batch instead of early-window noise squatting the map for a full window. `max_tracked_pairs = 0` refuses every pair without panicking.
- The OTLP enqueue waits at most 2 seconds for a slot on the ingest channel before rejecting (HTTP 503 / gRPC `UNAVAILABLE`, both retryable per the OTLP spec; gRPC `INTERNAL` only on a closed channel during shutdown) and counting `channel_full`. A plain `send().await` only errors on a closed channel, so genuine queue saturation used to park senders until the router request timeout with no rejection ever counted, which also kept the `ingestion_drops` warning and the new tuning hint from firing in the exact scenario they describe.
- Concurrent OTLP decode is now bounded on both the HTTP and gRPC paths, so a saturation flood can no longer monopolize protobuf-decode CPU and starve the `/health` liveness probe. Deployments running under sustained saturation should still give the probe headroom, for example `timeoutSeconds: 5` and `failureThreshold: 5`.
- The batch CLI input cap is decoupled from the daemon network payload limit. `analyze`, `diff`, `report`, `explain`, `calibrate`, `bench` and `pg-stat` read local files up to 1 GiB (previously bounded by `[daemon] max_payload_size`, whose 100 MiB ceiling made any larger trace export unanalyzable). Oversized files are rejected from metadata before reading a byte.
- The HTML dashboard bounds the findings it embeds, critical first (warning, then info, canonical order within each band), with a banner stating the kept/total split. Previously only embedded traces were trimmed to the ~5 MB target, so a large batch (tens of thousands of findings) shipped a 50+ MB HTML file. The JSON report keeps the full set; `--max-traces-embedded` opts out of size targeting entirely.
- The no-daemon `tui` feature combo builds without warnings (`Clear` import, `with_pre_rendered_trees`, and a `mut` binding are now gated or allowed appropriately).

### Performance

- The ISO 8601 timestamp parser gains a fixed-layout fast path for the canonical 24-byte form every converter emits, and the general path drops its per-parse `Vec` allocations: 69.5 ns to 8.4 ns per parse, a 4 to 9 percent gain on the detector benches (timestamps are parsed several times per span across detect and carbon scoring).
- The daemon's per-service meter caches the labeled Prometheus counter children (the same pattern as the OTLP reject counters): the per-event path drops from a label-hash plus `MetricVec` lock to one `HashMap` lookup and an atomic add (11.9 ns to 1.0 ns in the micro bench).
- The batch CLI frees the raw input buffer before analysis starts, and `bench` clones its input inside the iteration loop instead of pre-cloning `iterations` copies (the old harness peaked at iterations x input RSS).
- The HTML findings trim serializes findings once instead of cloning the whole report.
- New measurement infrastructure: a seeded synthetic trace generator (`synth`, doc-hidden), a criterion suite over every pipeline stage, `bench --synthetic-events` for fixture-free runs, and a `profiling` build profile for flamegraphs.

### Documentation

- The network transport coefficient's provenance is corrected: 0.04 kWh/GB is presented as a conservative upper bound below the Sustainable Web Design Model v4 figure (0.059 kWh/GB operational), with the Mytton, Lunden & Malmodin 2024 power-model critique cited accurately (the previous text attributed to that paper a range it does not contain, and misquoted the Shift Project figure).
- A fabricated database-energy citation is replaced with the real paper (Z. Xu, Y.-C. Tu, X. Wang, "Exploring Power-Performance Tradeoffs in Database Systems", IEEE ICDE 2010), and the DBJoules reference is added.
- SCI wording is aligned with the specification revisions: the project "aligns with" SCI v1.0 rather than "implements" it, ISO/IEC 21031:2024 is attributed to ISO/IEC JTC 1 (developed by the GSF), average grid intensity is justified under SCI v1.1 (v1.0 required marginal rates), and the disclosure disclaimers are no longer described as SCI-defined.
- `ARCHITECTURE.md` module table catches up with the codebase (periodic-disclosure stack, five energy backends, `diff`/`calibrate`/`acknowledgments`/`http_client`/`shutdown` rows) and a new Cargo features section documents what `daemon`/`tui`/`tempo`/`jaeger-query` gate. `CLI.md` cross-references the supply-chain trio. `METRICS.md`, `RUNBOOK.md` and `LIMITATIONS.md` document the new counters and their signals. `METRICS.md` and `CONFIGURATION.md` document the tuning advisor rules. FR mirrors updated in lockstep, including the previously missing Kepler and Redfish gauge rows and warning-kind section in `METRICS-FR.md`.
- `RUNBOOK.md` gains a measured sizing reference drawn from the saturation curve, and the stale benchmark tables are dropped from the design docs, which now point at git history instead.

## [0.8.6]

Makes the daemon's two bounded queues tunable and trims a per-batch allocation on the streaming hot path. No daemon wire protocol change, no breaking config change.

### Added

- `[daemon] ingest_queue_capacity` and `[daemon] analysis_queue_capacity` (both default `1024`, range `1` to `1,048,576`, validated at config load) make the ingestion channel and the analysis-worker queue depths configurable. Raise them under bursty load to reduce ingestion backpressure and analysis-worker shedding. Both were previously hardcoded to 1024.
- Metered load shedding on the analysis worker: under sustained overload the bounded queue fills and whole batches are shed instead of blocking ingestion, explicitly and never silently. `perf_sentinel_analysis_queue_depth` exposes the live backlog, and `perf_sentinel_analysis_shed_batches_total` and `perf_sentinel_analysis_shed_traces_total` count what was dropped. If the worker itself stops, for example a detector panics on a pathological trace, the daemon exits with an error so a supervisor restarts it rather than staying up while silently analyzing nothing.

### Changed

- `detect` and `score` no longer run inline on the daemon's `tokio::select!` ingestion loop. They run on a single dedicated analysis worker task fed over a bounded channel, in FIFO order so the stateful cross-trace correlator still sees a deterministic sequence. A long analysis pass can no longer stall `rx.recv()` or the TTL eviction ticker, so ingestion and eviction liveness no longer depend on analysis latency.
- The daemon shares its base `CarbonContext` across analysis batches via `Arc` instead of deep-cloning the region map and calibration table on every evicted batch, so the common no-scraper deployment turns a per-batch `HashMap` clone into an `Arc` refcount bump. Shed counting moved onto `MetricsState::record_shed`, and `AnalysisBatch` construction is centralized in a single constructor.
- `watch --help` now lists every `[daemon]` tunable (listeners, sizing, the bounded-queue knobs, and the sub-sections), `analyze --help` lists the batch `[thresholds]`, `[detection]` and `[green]` tunables, and the root help carries a config-location note. `perf-sentinel man` now emits one page per subcommand, so tuning documented only in a subcommand's long help is discoverable from the manual.

### Documentation

- `LIMITATIONS.md` and its FR mirror note that the per-window disclosure archive is best-effort (drop-on-full): under sustained load whole windows can be dropped from the NDJSON archive even when their findings were analyzed and served, and a graceful shutdown does not extend its delivery guarantee to the archive.
- The daemon architecture diagram reflects the analysis worker, the bounded work channel, metered shedding, and the fail-loud worker-death arm.
- The README and its French mirror gain four operator-facing points: the deterministic output guarantee (identical input yields byte-identical JSON and SARIF, so a CI quality gate never flickers), the daemon backpressure and shedding behavior, the internal `sampling_rate` interaction with count-based detectors kept distinct from upstream sampling, and the OTLP ingestion trust boundary.

## [0.8.5]

Drains the daemon's in-flight streaming window on `SIGTERM` as well as `SIGINT`, so a normal Kubernetes pod termination flushes the window through detection instead of dropping it. Also makes the daemon binary and the core test suite build and run on Windows. No daemon wire protocol change, no breaking config change.

### Added

- A shared `crate::shutdown::shutdown_signal()` helper resolves on `SIGINT` (all platforms) and `SIGTERM` (Unix), used by both the daemon event loop and the one-shot Tempo fetch loop, built once and pinned before each `select!` loop so the signal listeners register a single time. The daemon now drains its in-memory streaming window on `SIGTERM` as well as `SIGINT`, so a normal Kubernetes pod termination (rolling update, scale-down) flushes the in-flight window through detection instead of dropping it. Only an ungraceful kill (`SIGKILL` after the grace period, OOM) loses it, so keep `terminationGracePeriodSeconds` above the configured window duration to benefit. The Tempo fetch drain aborts in-flight fetches on `SIGTERM` too, for consistent shutdown behavior across modes.

### Fixed

- The daemon binary and the core test suite now build and run on Windows (MSVC). These were pre-existing Windows-only failures the Linux CI never caught. A `.cargo/config.toml` reserves an 8 MiB main-thread stack via `/STACK:8388608` for `cfg(all(windows, target_env = "msvc"))`, so the debug `#[tokio::main]` future no longer overflows Windows' default 1 MiB stack, which crashed even `perf-sentinel --version`. The `archive.rs` symlink-rejection test and its now-Windows-unused `assert_matches` import are gated on `#[cfg(unix)]`, the `ack.rs` non-Unix `tighten_parent_dir_perms` stub stays `async` to mirror the Unix signature, and the Kepler and Redfish staleness-gauge tests poll for the gauge to climb instead of assuming a fixed 300 ms scrape failure (instant on Linux, up to the fetch timeout on Windows). No Linux behavior change.

### Documentation

- Documented (EN and FR, across README, LIMITATIONS, INSTRUMENTATION and HELM-DEPLOYMENT) the entry costs flagged by three external critiques: how instrumentation quality bounds findings because spans missing their carrying attributes (`db.statement`/`db.query.text`, `http.url`/`url.full`) are dropped silently, how upstream head-based sampling keeps per-trace detectors correct but degrades rare patterns, aggregates and cross-trace correlation (kept distinct from the daemon `sampling_rate` knob), and the daemon state model (in-memory window, no persistence, no shared state across replicas, trace-id routing for per-trace correctness, window loss bounded to ungraceful kills). Also clarified the `SIGTERM` handler scope and the Windows stack-reservation note.

## [0.8.4]

Adds a `man` subcommand that renders the perf-sentinel manual page to stdout, and seeds every user-facing command's `--help` with copy-pasteable usage examples. No daemon wire protocol change, no breaking config change.

### Added

- `perf-sentinel man` renders the top-level roff manual page to stdout via `clap_mangen`, mirroring the existing `completions` subcommand. Redirect it into a man path (for example `perf-sentinel man > /usr/local/share/man/man1/perf-sentinel.1`) so `man perf-sentinel` works. The page lists the subcommands, like `git.1`.
- Usage-example blocks under the `--help` of every user-facing command (`analyze`, `watch`, `explain`, `report`, `diff`, `tempo`, `jaeger-query`, `calibrate`, `pg-stat`, `query`, `ack`, `disclose`, `verify-hash`, `hash-bake`, `completions`), rendered under both `-h` and `--help` and mirroring the invocations documented in `docs/CLI.md` and the README.

### Fixed

- Tightened several command help texts: removed semicolons in favor of separate sentences, and clarified that the `disclose --intent audited` value is reserved for a future release and exits with code 2.

### Documentation

- Documented the `man` subcommand in `docs/CLI.md`, its French mirror `docs/FR/CLI-FR.md`, and both READMEs.

### Internal

- The non-fatal continuity and completeness warnings in `disclose` were extracted into a pure function that returns the warning lines, which `cmd_disclose` prints to stderr. Behavior unchanged, the extraction lowers the cognitive complexity of `cmd_disclose` and the pure function is now covered by unit tests.
- `scripts/release.sh` gains a `--skip-lab` flag that bypasses the simulation-lab validation gate explicitly. It logs a loud audit warning and never writes the validation ledger, so a release validated by other means leaves no false PASS behind. All other pre-checks and the version gate still apply. The public-communication step was removed from the release procedure.

## [0.8.3]

Adds a temporal-coverage continuity signal to the periodic public disclosure so a reader can tell a continuously-measured period from one where the daemon ran only a handful of days, the in-binary signal closest to an operator who simply stops measuring for part of a period. Also adds an in-band provenance marker on the scope manifest and reserves a hook for a future inter-period transparency log. The report schema gains an additive v1.2 revision, v1.1 and v1.0 readers and reports remain valid, and the `content_hash` of a pre-v1.2 report is unchanged when re-hashed on a v1.2 binary. No daemon wire protocol change, no breaking config change.

### Added

- `aggregate.temporal_coverage` (schema `perf-sentinel-report/v1.2`): the fraction of the declared period's calendar days that carried archived windows (`observed_days / days_in_period`), alongside `observed_days`, `days_in_period` and `largest_gap_days`. Daemon archiving is traffic-gated, so this measures days with observed traffic, a lower bound on activity, not daemon uptime. It is published and surfaced as a CLI warning plus an in-band disclaimer below an informational threshold, never a hard `official` gate, because a legitimately quiet period would otherwise be punished. It closes the visibility gap on partial daemon shutdown that `days_covered` (pure calendar arithmetic) and `period_coverage` (calibration quality) left open.
- `scope_manifest.coverage_basis`: an in-band provenance marker listing which scope fields are operator-asserted (the unaudited denominators `total_applications_declared` and `total_requests_in_period`, plus the exclusion lists) versus machine-derived (`applications_measured`, `requests_measured`, `coverage_percentage`), so a reader of `coverage_percentage` knows its denominator rests on operator assertion.
- Reserved `integrity.cross_period_log` hook for a future external append-only or Rekor-style log chaining successive report hashes across periods, the mechanism that would make total non-participation detectable by a third party. Always absent in v1.2, populated only under a future `audited` intent, so current report hashes are unaffected.

### Changed

- The `official`-intent validator gains consistency checks that catch hand-edited or forged reports: `days_covered` must equal `(to_date - from_date) + 1`, `requests_measured` must not exceed an operator-declared `total_requests_in_period`, and a populated `temporal_coverage` block must be internally consistent (`observed_days <= days_in_period`, `days_in_period == period.days_covered`). A disclose-produced report satisfies all of these by construction, so only a forged report trips them. Omitting `total_requests_in_period` for an `official` report now emits a warning rather than silently dropping `coverage_percentage`.

### Documentation

- Documented the v1.2 schema additions, the traffic-gated caveat on temporal coverage, the self-disclosure limits on the operator-declared denominators, and the reserved cross-period hook across `docs/SCHEMA.md`, `docs/REPORTING.md`, `docs/design/08-PERIODIC-DISCLOSURE.md` and their French mirrors, plus the JSON Schema and the two example reports.

## [0.8.2]

Adds a two-tier avoidable energy and carbon breakdown to the periodic public disclosure so the disclosed waste figure can no longer be shrunk by loosening the operator's own N+1 detection threshold. Also bumps the interactive TUI backend. The report schema gains an additive v1.1 revision, v1.0 readers and reports remain valid. No daemon wire protocol change, no breaking config change.

### Added

- The periodic public disclosure now reports avoidable energy and carbon at two N+1 thresholds side by side, `aggregate.canonical_waste` and `aggregate.operational_waste` (schema `perf-sentinel-report/v1.1`). The canonical tier is computed at a fixed threshold pinned in the binary (`2`) that the operator cannot configure, so the headline avoidable figure is non-manipulable. Raising the operational `n_plus_one_threshold` can no longer shrink the disclosed waste, the way it could when the disclosure was indexed solely on the operator's findings. The operational tier records the operator's own configured threshold next to its avoidable figures, so a reader sees the gap between what the operator detects and the canonical floor. The canonical pass runs at daemon archive time over the raw traces, where re-detection is still possible, and `disclose` sums both tiers from the archives. The pre-existing flat fields `estimated_optimization_potential_kgco2eq`, `aggregate_waste_ratio` and `aggregate_efficiency_score` are retained as aliases of the canonical tier. For `intent = official` the validator requires the canonical threshold to equal the binary's pinned value, the operational threshold is recorded but deliberately not range-checked. Both tiers are covered by the report `content_hash`. The schema change is additive, the new tiers default and are omitted from the wire when absent, so a v1.0 report re-hashed on a v1.1 binary keeps the same `content_hash`.

### Changed

- Upgraded the interactive TUI backend from ratatui 0.30.0 to 0.30.1. The patch hardens several widgets against panics and arithmetic overflow (empty `BarChart`, single-label `Chart` axis, `Clear` on areas outside the buffer, chart and sparkline scaling) and fixes `Paragraph` text-alignment inheritance. No behavior change for perf-sentinel's list and paragraph TUI views.

### Fixed

- `verify-hash` could report a `content_hash` mismatch on an untampered official disclosure. The cause was serde_json parsing floats without the `float_roundtrip` feature, so a value written by the serializer could come back one unit in the last place off on re-parse, changing the canonical hash. The feature is now enabled, a freshly disclosed report verifies, and a regression test pins a value that drifts under the default parser. The defect predates this release and is resolved here.

### Security

- A cargo-deny gate now layers license, advisory, source and ban checks on top of cargo-audit, the rendered Helm manifests are scanned with Checkov, and the CI and security-audit workflows run under default-deny top-level permissions.

### Documentation

- Restructured the English and French READMEs. Added a table of contents and a dedicated input-formats section, moved the performance and supported-languages sections behind collapsible details, expanded the data-handling and licensing sections, and refined the wording around energy and carbon estimation (GreenOps footprint, carbon-accounting limitations).

## [0.8.1]

Maintenance release. Hardens the SQL normalizer against a future refactor regression and documents the release profile's stance on integer overflow checks. No change to the daemon, the CLI surface, the report JSON schema, or any wire format. Every user-facing output is byte-for-byte identical to 0.8.0.

### Internal

- The homemade SQL tokenizer takes `&str` slices of the query as it tokenizes. Those slices are safe because the tokenizer only ever anchors a bound on an ASCII delimiter (`'`, `"`, `$...$`, digits), and every ASCII byte is a UTF-8 char boundary, an emergent invariant of the scanning discipline rather than a guarded one. The slices now route through a single helper that asserts, in debug builds, that both byte bounds fall on char boundaries. The check compiles out in release and turns a future slice taken at a non-ASCII-anchored position into a loud test failure instead of a latent panic. The five existing UTF-8 tokenizer tests now exercise the assertion with real multi-byte input.
- The `disclose` preview state initialization was simplified around `PathBuf`. No behavior change.

### Documentation

- The release-profile design note (`docs/design/07-CLI-CONFIG-RELEASE.md` and its French mirror) now documents why the release profile deliberately leaves `overflow-checks` off. Under `panic = "abort"`, enabling it would turn any integer overflow on attacker-influenced arithmetic into a process abort, and the carbon accumulators being `f64` means the flag would not even catch the silent-wrap case it is usually invoked against. Overflow handling stays explicit and local at the few integer sites where a wrong value would matter.

## [0.8.0]

Turns the interactive inspector into an all-in-one TUI, adds a read-only preview for periodic disclosure reports, and moves the workspace to the Rust 1.96.0 toolchain. The default stdout output of `analyze` and `explain` is unchanged, so CI and scripting are unaffected. No config breaking change, no report JSON schema change.

### Added

- Unified interactive TUI with three views forming a single drill-down. The existing `Inspect` browser (traces, findings, correlations, detail) is now flanked by `Analyze` (GreenOps summary: I/O waste ratio, top offenders, quality gate, findings by severity) and `Explain` (the selected trace's annotated span tree, full screen). Enter descends `Analyze -> Inspect -> Explain`, Esc ascends back, and a top tab bar shows the current view. `analyze --tui` opens on the Analyze view, `explain --tui` opens on the Explain view focused on `--trace-id`, and both reuse the full analysis pipeline so all three views stay coherent regardless of the entry command. The default stdout output of `analyze` and `explain` is unchanged: `--tui` conflicts with `--format` and `--ci`, so CI and scripting are unaffected. Under `query inspect` the Analyze view is fed live from the daemon's `/api/export/report`, degrading to a hint on older daemons. Panel navigation inside Inspect now also accepts vim `h`/`l` alongside the arrow keys.
- `disclose --tui` opens a read-only preview of the periodic disclosure report. A calendar stepper drives the period (month, quarter, year, or custom, with `from`/`to` snapping to calendar boundaries), intent and confidentiality toggle live, and the summary reports the window count, period coverage against the official threshold, measured and excluded services, the totals (requests, carbon, energy, waste ratio) and, for official intent, the validator verdict. The footer prints the exact `disclose` command for the current settings, to copy into a reproducible run. The preview re-reads the same cold NDJSON archive as the canonical command, so the figures match, but it never hashes or writes a report. It is backed by a new `archive_time_range` helper in the periodic aggregator that scans only each window's `ts` field to anchor the default period.

### Changed

- The workspace moves to the Rust 1.96.0 toolchain and the minimum supported Rust version is bumped accordingly. The release adopts the stabilized idioms it enables (`Path: PartialEq<str>`, `slice::array_windows`, and `core::assert_matches!` in tests), all semantics-preserving. The published benchmark figures were refreshed on 1.96.0.

### Security

- Quality-gate rule names rendered in the colored CLI report now pass through `sanitize_for_terminal`, closing a path where an attacker-controlled rule name could reach the terminal unescaped. This is the same control-character and escape-sequence guard already applied to the other operator-visible strings in the report, extended to the one rule-name field that still bypassed it.

### Documentation

- New `docs/INSPECT.md` and `docs/REPORTING.md` sections, with their French mirrors, document the unified TUI drill-down and the read-only disclose preview, including the keybindings and the still frames for each view. The README and README-FR Quick look were reworked to lead with the HTML dashboard then the interactive TUI, and the CLI-commands diagram now marks the `--tui` entry points. The project ships a new logo and banner.

## [0.7.8]

Closes the `suggested_fix` coverage gaps surfaced by the v0.7.7 simulation lab and extends N+1 detection to HTTP. Three additive changes: vendor-specific OTel scope recognition (.NET EF Core, Quarkus), a last-resort service-name fallback for framework detection, and HTTP sanitizer-aware classification. Lab validation reached 118/120 (98.3%) with `suggested_fix` coverage at 11/12. No config breaking change, no CLI surface change, no daemon wire protocol change, no report JSON schema change.

### Added

- Vendor-specific OpenTelemetry scope recognition (`VENDOR_SCOPE_RULES`): scopes that do not follow the `io.opentelemetry.*` convention now resolve a framework. `OpenTelemetry.Instrumentation.EntityFrameworkCore` and `Microsoft.EntityFrameworkCore` map to `csharp_ef_core`, `io.quarkus.hibernate.reactive` / `io.quarkus.panache.reactive` / `io.quarkus.reactive` map to `java_quarkus_reactive`, and the catch-all `io.quarkus` maps to `java_quarkus` (reactive sub-packages checked first). Matching is segment-boundary aware (`vendor_prefix_matches`), so `io.quarkusbridge.acme` does not match `io.quarkus`. Resolves the missing `suggested_fix` on .NET EF Core (dotnet-svc) and Quarkus reactive (mutiny-svc) services.
- Service-name fallback for framework detection (`SERVICE_NAME_RULES`): when instrumentation scopes, `code_location`, and filepath are all absent, a distinctive framework substring in the service name (e.g. `helidon` in `helidon-se-svc`) infers the framework as a last resort. Entries are limited to names distinctive enough to avoid false positives, generic terms like `diesel`, `gorm`, `prisma`, `quarkus` are intentionally excluded.
- HTTP N+1 sanitizer-aware classification: `classify_group` dispatches SQL and HTTP. HTTP groups that fail the direct distinct-params rule (`distinct_params < threshold`) are reclassified from `redundant_http` to `n_plus_one_http` on timing variance (`Auto`/`Always`), or a primary signal (HTTP placeholder, high occurrence, or sequential siblings) corroborated by timing variance (`Strict`). Unlike SQL, high occurrence alone is not sufficient corroboration for HTTP because there is no `looks_sanitized` gate.

### Security

- `safeHttpsHref` in the HTML dashboard now rejects C0 controls, DEL, and C1 controls (`\x00-\x1f\x7f-\x9f`) in addition to the existing `https://` scheme lock, completing the control-character guard for defense-in-depth.

### Documentation

- New limitation entry "HTTP query-string redaction and N+1 visibility" (`docs/LIMITATIONS.md` + FR mirror): OpenTelemetry .NET `System.Net.Http` redacts the query string to `?*` by default, so query-parameter-based HTTP N+1 loops (`?seq=1`, `?seq=2`, ...) reach perf-sentinel as byte-identical URLs and are detected as `redundant_http`. The distinguishing parameter is destroyed upstream and unrecoverable by any trace consumer. Operators who rely on query-parameter HTTP N+1 detection set `OTEL_DOTNET_EXPERIMENTAL_HTTPCLIENT_DISABLE_URL_QUERY_REDACTION=true`, or model the varying identifier as a path segment. Cross-referenced from the .NET instrumentation guide and the detection design notes (EN + FR).

### Operator-visible behavior change

- HTTP workloads under `sanitizer_aware_classification = "strict"` (or `auto`/`always`) may see some groups previously reported as `redundant_http` (Warning) now reported as `n_plus_one_http` (Critical at >= 10 occurrences) when they show timing variance. CI gates wired on critical-only count may flag groups that were previously only at warning level. Same recommendation as the 0.7.7 SQL change: re-baseline `n_plus_one_http_critical_max` on a representative trace sample, or treat the upgrade as the moment to address those patterns at source. GreenOps scoring is unchanged.

## [0.7.7]

Fixes an N+1 SQL detection gap on non-Java stacks. The sanitizer-aware classifier and the SQL normalizer both assumed JDBC-style `?` placeholders, so Go (pgx `$1`), Python (psycopg `%s`, SQLAlchemy `:name`), .NET (`@param`), and any stack that sends pre-parameterized SQL to the wire were silently classified as `redundant_sql` instead of `n_plus_one_sql`. The release also adds framework-aware fix recommendations for Go and Node.js/TypeScript (96 entries, was 70), and comprehensive per-language instrumentation documentation. Lab validation against 11 multistack services reached 103/110 findings (94%), up from 78/110 (71%) on v0.7.6.

### Added

- `suggested_fix` coverage broadened from 3 to 10 anti-patterns across 6 language ecosystems (Java, C#, Python, Rust, Go, JavaScript/TypeScript). v2 covered `n_plus_one_sql`, `n_plus_one_http` and `redundant_sql` for Java/C#/Rust. v3 adds entries for `redundant_http`, `slow_sql`, `slow_http`, `excessive_fanout`, `chatty_service`, `pool_saturation` and `serialized_calls`, introduces Python support (`python_django`, `python_sqlalchemy`, `python_generic`), Go support (`go_gorm`, `go_generic`), and Node.js/TypeScript support (`node_prisma`, `node_generic`). Language detection from scope prefixes (`github.com/` for Go, `@opentelemetry/instrumentation-` / `@prisma/` / `@nestjs/` for JavaScript) and file extensions (`.go`, `.js`, `.jsx`, `.ts`, `.tsx`, `.mjs`, `.mts`, `.cjs`, `.cts`). ORM marker `django.db` widened to `django` to match the Python OTel scope `opentelemetry.instrumentation.django`. Total FIXES entries: 96 (was 21 in v2).
- Findings now carry optional per-span diagnostic timing stats on the `pattern` field: `span_duration_us_p50` (median, microseconds), `span_duration_us_p99` (P99), `span_duration_cv_x1000` (coefficient of variation scaled by 1000, e.g. 523 = CV 0.523). Populated by the n+1 and slow detectors. Not used in any detection verdict or GreenOps scoring, exposed purely so downstream consumers (operator dashboards, lab validators) can profile cache-warm patterns without needing daemon-log access. Omitted from JSON when not populated (`skip_serializing_if`).

### Fixed

- `template_has_placeholder` now recognizes five placeholder styles instead of JDBC-only `?`: `$?` (PostgreSQL native, Go pgx, Rust sqlx), `%s` (Python DB-API, psycopg, MySQLdb), `@param` (.NET Npgsql, SqlClient), `:name` (Oracle, SQLAlchemy named). Guard rules exclude false positives on `@@ROWCOUNT` (SQL Server system variables), `::` (PostgreSQL type casts), and `:digit` (array slices).
- The SQL normalizer now treats PostgreSQL `$N` positional parameters (`$1`, `$2`, ...) as placeholders, emitting `$?` in the template with empty extracted params. Previously `$N` was misinterpreted as a dollar-quoted string delimiter, preventing the correlator from grouping identical queries on Go pgx and Python asyncpg traces.
- `sanitizer_aware_classification = "strict"` now reaches parity with `"auto"` on bare-driver stacks (Vert.x reactive PG, pgx, asyncpg, sqlx, Prisma `queryRaw`) via a sequential-siblings + variance condition.
- Sequentiality is now evaluated in microseconds (`start_ms × 1000 + duration_us`), not milliseconds. The previous `duration_us / 1000` truncated sub-millisecond durations to zero, letting truly concurrent spans pass the sequentiality gate and silently miss n+1 patterns whose intra-millisecond timing was the only signal. Same fix applied to the existing serialized-calls detector.
- `sanitizer_aware_classification = "strict"` now reclassifies sanitized SQL groups with an ORM scope marker AND high occurrence count (≥ 3 × `n_plus_one_threshold`, default 15) even when per-span timing variance is low. Covers the cache-warm trap observed on EF Core + Npgsql (lab dotnet-svc Phase 5) and Hibernate L2 cache, where a real n+1 lookup-by-PK produces tight per-span timings because rows stay in the database's shared buffers. Legacy polling loops below the threshold (typical 5-10 calls per request) stay classified as `redundant_sql` to preserve precision on cached identical reads.
- Removed `sqlx` from the ORM scope marker list: jmoiron/sqlx (Go) is a thin wrapper on `database/sql` and Rust `sqlx` is a bare driver, neither is an ORM. Their n+1 patterns are now handled by the bare-driver branch above.

### Documentation

- `docs/INSTRUMENTATION.md` and its FR mirror gain four per-language sections covering Go (otelhttp + otelpgx), Python Django (psycopg), Python FastAPI (SQLAlchemy + asyncpg) and Node.js (Nest.js + Prisma), plus a new "SQL placeholder styles and detection" reference section with a five-row table mapping each placeholder style to its drivers and OTel SDK.
- `docs/CONFIGURATION.md` and `docs/design/04-DETECTION.md` (and FR mirrors) document the five placeholder styles and the `@@`/`::`/`:digit` exclusion guards, and new root-level documentation indexes (`docs/00-INDEX.md`, `docs/FR/00-INDEX-FR.md`) group 21 documents into five sections with sub-directory pointers.

### Operator-visible behavior change

- Bare-driver workloads running under `sanitizer_aware_classification = "strict"` may see some groups previously reported as `redundant_sql` (Warning at ≥5 occurrences) now reported as `n_plus_one_sql` (Critical at ≥10 occurrences). CI gates wired on critical-only count may flag groups that were previously only at warning level. GreenOps scoring (IIS, avoidable_io_ops, io_waste_ratio, carbon attribution) is unchanged: both finding types contribute identically.

  **Recommended upgrade path for operators with `quality_gate` rules:**
  1. Run `perf-sentinel analyze` once on a representative trace sample with 0.7.7 and inspect the per-trace finding counts vs the 0.7.6 baseline.
  2. If new `n_plus_one_sql` Critical findings appear on bare-driver services that were previously surfaced as `redundant_sql` Warnings, either (a) raise the corresponding `n_plus_one_sql_critical_max` threshold to absorb the baseline shift, or (b) treat the upgrade as the expected moment to address those n+1 patterns at source. The findings are the same defects under a sharper label, not new defects.
  3. The `serialized_calls` and `pool_saturation` detectors now correctly fire on sub-millisecond sequential / concurrent SQL bursts that were previously masked by an integer-division bug in the timing math. Same recommendation: re-baseline `serialized_calls_critical_max` and `pool_saturation_critical_max` (or fix the patterns) before tightening the gate.

## [0.7.6]

Hardening pass on Scaphandre and Redfish energy sources. Two config breaking changes (both intentional, no migration path other than rewriting the affected sections) eliminate silent over-attribution failure modes that the v0.7.4 / v0.7.5 incident class made unacceptable: the Scaphandre `process_map` matcher now consults `cmdline` in addition to `exe` to disambiguate co-located JVMs / .NET runtimes, and Redfish endpoints declare their wire schema (`legacy_power` or `environment_metrics`) per-endpoint instead of a single global JSON pointer. Both new struct shapes deny unknown TOML fields. New runtime warn-once nets and wire-conformance CI assertions catch silent upstream renames on Scaphandre at runtime, mirroring the Kepler v0.10 net delivered in v0.7.5.

### BREAKING CHANGES

**1. `[green.scaphandre].process_map` schema (was flat string, now typed struct).**

Pre-0.7.6:

```toml
[green.scaphandre.process_map]
"order-svc" = "java"
```

0.7.6+:

```toml
[green.scaphandre.process_map."order-svc"]
exe_contains = "bin/java"
cmdline_contains = "order-svc.jar"
```

The matcher uses substring containment (not exact equality) on both labels. Real Scaphandre v1.0.2 emits `exe` as an absolute path (`/usr/lib/jvm/temurin-25-jdk-amd64/bin/java`) so the legacy basename `"java"` never matched the runtime payload. Multiple co-located services sharing a JVM or CLR collide on `exe` and require `cmdline_contains` to disambiguate. Scaphandre concatenates argv without separators (`java -jar /tmp/svc.jar` is emitted as `cmdline="java-jar/tmp/svc.jar"`), so `cmdline_contains` must use a fragment of that concatenated form, typically the jar / dll filename. The legacy flat string form is rejected at config load with a clear serde type-mismatch error. `#[serde(deny_unknown_fields)]` on `ProcessMatcher` turns typos like `cmdline_containss` into config-load errors instead of silent over-attribution.

**2. `[green.redfish].endpoints` schema (was URL string, now typed struct with per-endpoint schema selector).** The top-level `power_path` field is removed.

Pre-0.7.6:

```toml
[green.redfish]
power_path = "/PowerControl/0/PowerConsumedWatts"

[green.redfish.endpoints]
"chassis-1" = "https://bmc/redfish/v1/Chassis/1/Power"
```

0.7.6+:

```toml
[green.redfish.endpoints."chassis-1"]
url = "https://bmc/redfish/v1/Chassis/1/Power"
schema = "legacy_power"
```

`schema` is a closed enum with two variants today: `legacy_power` (resolves `/PowerControl/0/PowerConsumedWatts`) and `environment_metrics` (resolves `/PowerWatts/Reading`). Each endpoint declares its own schema, so a fleet mixing legacy `/Power` BMCs and modern `EnvironmentMetrics` BMCs is configured by adding two entries with different schemas. Both new struct shapes deny unknown fields. The legacy flat string form, the legacy top-level `power_path` field, and unknown schema variants all fail at config load with a serde error rather than silently degrading. Vendor OEM JSON pointers (e.g. HPE's `Oem.Hpe.PowerSummary.Watts`) are no longer configurable, OEMs that publish wattage at a non-standard path must front the BMC with a reverse proxy that reshapes the payload.

### Added

- **Scaphandre matcher upgraded to cmdline-aware contains-based matching.** `apply_scrape` walks `process_map` and selects the unique `ProcessPower` whose `exe` contains the configured `exe_contains` and whose `cmdline` contains the optional `cmdline_contains`. Multiple matches per service emit a warn-once latch keyed by service name, with debug-level follow-up on subsequent ambiguous ticks and an automatic clear-on-clean-match so a future flap re-warns. The parser preserves the `cmdline` label on `ProcessPower`, previously discarded.
- **Scaphandre zero-sample warn-once net.** `track_zero_sample_streak` in `score/scaphandre/scraper.rs` mirrors the Kepler equivalent and fires at most once per HTTP-200 streak of three consecutive scrapes with no parsed `ProcessPower` entries. The warn message hints at the most common causes (upstream rename, `--no-procfs`, host-only build) and explicitly excludes the RAPL-less case, which produces non-zero readings with `power=0` and is a separate failure mode. The CI grep marker is extracted into `ZERO_SAMPLE_WARN_MARKER` so a doc-pass rename of the warn text can be caught by the wire-conformance gate.
- **Scaphandre staleness gauge symmetry with Kepler.** `scaphandre_last_scrape_age_seconds` now advances on every failure tick (was: only set to 0 on success and frozen at the last good value), seeded from scraper start so a never-succeeded scraper still climbs the gauge from boot. Operators alerting on `rate(...)` of this gauge will see the same behavior they get from `kepler_last_scrape_age_seconds`.
- **Redfish `EnvironmentMetrics` schema support.** The parser dispatches on the new `RedfishSchema` enum (`legacy_power` or `environment_metrics`) and reads the canonical JSON pointer per schema, no operator-typed pointer. `EnvironmentMetrics.PowerWatts.Reading` is the modern equivalent of `PowerControl[0].PowerConsumedWatts` introduced by DMTF Release 2020.4. Both schemas resolve to the same downstream `redfish_bmc` model tag.
- **Wire-conformance CI extensions for Scaphandre and Redfish.** The `scaphandre-wire-conformance` job now spawns 2 co-located JVMs and asserts that the cmdline-aware matcher disambiguates them without firing the ambiguous-matcher warn, plus a defensive assertion that the zero-sample warn never fires on a healthy run. The `redfish-wire-conformance` job now asserts the modern `/PowerWatts/Reading` path resolves to a positive finite number on the DMTF mockup, alongside the existing legacy `/PowerControl/0/PowerConsumedWatts` assertion, and the end-to-end step configures two chassis-ids (one per schema) to exercise both wire shapes through the same daemon config.
- **New observation workflow `scaphandre-exe-observation.yml`.** Manual `workflow_dispatch` job that runs real Scaphandre v1.0.2 against 3 toy JVMs, 3 toy .NET assemblies and 1 native Rust binary, then dumps the full `/metrics` as a downloadable artifact and answers five empirical questions about what `exe` / `cmdline` actually look like on the wire. The data captured on 2026-05-23 (`PowerWatts.Reading = 374` from the DMTF mockup) is what drove the schema design.

### Changed

- **`docs/LIMITATIONS.md` (and FR mirror) Redfish precision-bounds section gains methodology notes** on sensor smoothing divergence between `legacy_power` (vendor-smoothed wattage, Dell iDRAC ~5s rolling, HPE iLO 1-5s) and `environment_metrics` (current-tick gauge), and on the `EnergykWh.Reading` cumulative-energy field that the modern schema exposes but perf-sentinel does not yet consume. Operators switching schemas on a chassis should expect mean-preserving but variance-tightening behavior on the `redfish_bmc` carbon-per-op series.
- **`docs/LIMITATIONS.md` (and FR mirror) Scaphandre precision-bounds section documents v1.0.2 as the validated upstream reference version** (pinned by SHA256 of the upstream `.deb` artifact in the wire-conformance job). The parser remains version-agnostic by design, the pin only documents the validated reference.
- **`docs/CONFIGURATION.md` (and FR mirror)** rewritten for the new TOML shapes (`[green.scaphandre.process_map."svc"]` table form, `[green.redfish.endpoints."chassis"]` with `url` + `schema`), with side-by-side examples of multi-JVM Scaphandre attribution and mixed legacy/modern Redfish fleet.
- **`docs/METHODOLOGY.md` (and FR mirror) gains an "Academic grounding" section** citing the RAPL primary literature, the Scaphandre and Kepler software-meter literature, and the SPECpower / CCF dataset lineage that feeds the `cloud_specpower` path. The README clarifies that perf-sentinel performs per-span attribution and exposes OTJAE (OpenTelemetry Java Auto-instrumentation Equivalent) detail in its outputs.

### Internal

- **`ProcessPower` parser test fixtures captured from real Scaphandre v1.0.2 output** (recorded via the observation workflow on 2026-05-23). The fixtures encode the argv-concatenation behavior (`cmdline="java-jar/tmp/svc-a.jar"`) so a regression that breaks the substring matcher would surface in unit tests, not only at runtime.
- **`RedfishConfig.power_path` and `DEFAULT_POWER_PATH` removed.** Per-endpoint `RedfishSchema::json_pointer()` is a `const fn` returning `&'static str` per variant, the canonical pointer is decided at compile time. The `custom_power_path_resolves_for_oem_vendors` test was removed in lockstep, OEM-custom paths are no longer a supported configuration surface.
- **`TickContext.cfg` field removed** from the Redfish scraper after dropping `power_path` (the only consumer). `parse_chassis_uris` now returns `(chassis_id, hyper::Uri, RedfishSchema)` triples instead of pairs.
- **New `scripts/release.sh` canonical tag-and-push path.** Runs every pre-flight check (clean working tree, signing identity, `check-tag-version.sh`, `check-helm-tag-version.sh`, `check-chart-appversion-annotation.sh`, latest lab-validation freshness) before any destructive action and aborts on the first failure. The release procedure docs (EN and FR) now point at it as the single entry point for the tag-and-push steps.
- **Helm chart bumped to track `appVersion = 0.7.6`.** The default daemon image tag now points at `ghcr.io/robintra/perf-sentinel:0.7.6`. No values.yaml schema change.

## [0.7.5]

Bug fix on the Kepler integration that shipped in 0.7.4. The scraper targeted metric names and a label name that no released Kepler version ever published, so on any real Kepler v2 cluster every scrape returned HTTP 200 with zero matching samples, the `kepler_ebpf` `co2.model` tag never lit up, and carbon attribution silently fell through to the proxy chain. Cross-checked against the upstream Kepler metrics documentation. The Redfish BMC integration is unchanged, the DMTF schema it targets stayed stable. No CLI surface change, no daemon wire protocol change, no report JSON schema change.

### Fixed

- **Kepler scraper aligned to the Kepler v2 series.** The container variant now reads `kepler_container_cpu_joules_total` (was `kepler_container_joules_total`) keyed by `container_name` (unchanged). The new `Process` variant reads `kepler_process_cpu_joules_total` keyed by `comm` (the kernel command-name label, was the non-existent `command`).

### Removed

- **`metric_kind = "process_package"` and `metric_kind = "process_dram"` are removed.** Both targeted metrics that Kepler never published at any granularity, so neither value ever produced a working scraper in 0.7.4 and the compat impact is empty. Operators with these values in `.perf-sentinel.toml` get a config-load error pointing at the replacement. The new accepted set is `"container"` (default) and `"process"`, matched case-insensitively for parity with `parse_daemon_environment` on the same TOML surface.

### Added

- **Config-load pre-validation.** `load_from_str` now parses `metric_kind` before the lossy `Config::from` conversion, mirroring the daemon-environment pattern. Invalid values surface as `ConfigError::Validation` at startup instead of a buried `tracing::error` followed by silent Kepler disable.
- **`TASK_COMM_LEN` cap.** When `metric_kind = "process"`, `service_mappings` label values are capped at 15 bytes to match the Linux kernel's `comm` truncation. A label longer than 15 bytes can never match a real sample on the wire, so rejecting at config load saves a debugging round.
- **Cardinality cap.** `service_mappings` is capped at 1024 entries, mirroring `MAX_SERVICE_REGIONS`. Bounds the config-load memory footprint against fat-finger or hostile configs.
- **Control-character rejection.** `parse_kepler_metric_kind` and `validate_kepler_service_mappings` reject C0/C1 control characters in the operator-supplied strings before any error message interpolates them. Closes a low-severity ANSI-injection vector where a hostile `.perf-sentinel.toml` could embed escape sequences and have them rendered through stderr the first time an operator ran the binary on the file.
- **Zero-sample diagnostic.** A new warn-once log fires after three consecutive HTTP-200 scrapes that yield zero matching samples, the exact failure mode of running against a Kepler exporter older than v0.10 (legacy metric names without the `_cpu_` infix). Other causes named in the warn: `metric_kind` mismatched with the deployment topology, or `service_mappings` label values that do not exist on the wire. `perf_sentinel_kepler_last_scrape_age_seconds` keeps its existing semantics (resets to 0 on every HTTP-200), so alerts driven only by the gauge will not catch the zero-sample case and should be paired with `rate(perf_sentinel_kepler_scrape_total{status="success"}[5m])` and the daemon-side `co2.model` tag presence.

### Documentation

- **`docs/CONFIGURATION.md`** and its FR mirror: `metric_kind` reference table reduced to `container` / `process`, examples updated.
- **`docs/LIMITATIONS.md`** and its FR mirror: "Kepler precision bounds" updated, `metric_kind` references corrected.
- **`docs/METRICS.md`**: new sections "Kepler scrape counters" and "Redfish scrape counters" mirroring the existing Scaphandre section, with the zero-sample staleness note.
- **`KeplerMetricKind` enum doc warns explicitly that Kepler v1 / pre-0.10 deployments are not supported**, the `score/kepler/parser.rs` module doc is refreshed to reflect the Kepler v2 series, and the commented `[green.kepler]` block in `examples/perf-sentinel.toml` is updated.

### Internal

- **11 new unit tests, and the existing Kepler fixtures (26 hardcoded Prometheus literals) renamed to the v2 series.** Coverage spans `parse_kepler_metric_kind` (case-insensitive matching, empty-string rejection, raw value preservation in error messages, control-character rejection, legacy migration round-trip through `load_from_str`), `validate_kepler_service_mappings` (15-byte boundary for `Process`, cardinality cap), the `convert_kepler_section_with_env` `Process` happy path, and four `track_zero_sample_streak` edges (under threshold, at threshold, latch persistence over a longer streak, reset on non-empty scrape).
- **Helm chart 0.2.39 to 0.2.40, `appVersion` 0.7.4 to 0.7.5.** The `artifacthub.io/changes` annotation now carries both the 0.7.4 `kind: added` block and the 0.7.5 `kind: fixed` block so Artifact Hub consumers keep the Kepler and Redfish feature-addition narrative visible alongside the fix.

## [0.7.4]

Two new opt-in measured-energy backends join the carbon attribution stack, and the documentation tree gains primer pages and a restructured README. The Rust API, the daemon wire protocol, the CLI surface, and the report JSON schema are preserved byte-for-byte from 0.7.3. Operators on ARM64 (Graviton, Ampere, Cobalt 100, Apple Silicon) or on bare-metal nodes with a baseboard management controller now have a real measured path that the previous proxy fallback could not provide. Both new sections are absent from the default config, so no existing deployment changes behavior at upgrade time.

### Added

- **Kepler eBPF energy source (`[green.kepler]`, model tag `kepler_ebpf`).** Daemon-only scraper that reads Kepler's cumulative joule counters off the standard Prometheus `/metrics` endpoint, computes per-service joule deltas vs the previous scrape, and publishes measured energy-per-op coefficients. The scrape mode is selected via `source` (`prometheus` by default for the DaemonSet deployment pattern, or `direct` for a co-located exporter). The `metric_kind` knob switches between the three Kepler counter families: `kepler_container_joules_total`, `kepler_process_package_joules_total`, and `kepler_process_dram_joules_total` (default `container`). A `service_mappings` table ties each perf-sentinel service name to the Kepler label value identifying the same workload. Works on ARM64 hosts where Scaphandre's RAPL sensor is unavailable, with the upstream ARM eBPF precision caveats documented in `docs/LIMITATIONS.md` "Kepler precision bounds". The `+cal` calibration suffix never applies to measured spans. New Prometheus surfaces, all label-bounded at compile time: `perf_sentinel_kepler_last_scrape_age_seconds` (gauge), `perf_sentinel_kepler_scrape_total{status}` (counter, 2 statuses), `perf_sentinel_kepler_scrape_failed_total{reason}` (counter, 6 reasons pre-warmed). The reason set is shared with Scaphandre because both sources hit the same six HTTP failure modes verbatim, so a single dashboard panel can union-rate them.
- **Redfish BMC energy source (`[green.redfish]`, model tag `redfish_bmc`).** Daemon-only scraper that polls one or more chassis `/Power` resources for `PowerConsumedWatts`, distributes the chassis-level joules across mapped services proportional to their ops-deltas, and publishes per-service energy-per-op coefficients. Real wall-plug measurement including periphery (PSU, NIC, drives, fans), unlike Scaphandre and Kepler which see CPU and DRAM only. Single coefficient per chassis: all services mapped to the same node receive the same per-op value. The `power_path` field (JSON pointer, default `/PowerControl/0/PowerConsumedWatts`) accommodates vendor variance, with fixtures and unit tests covering Dell iDRAC, HPE iLO, and the OpenBMC reference response shape. Multi-chassis fleets configure one entry per node in the `endpoints` map. The scrape interval is clamped to `[15, 3600]` seconds to defend against BMC rate-limit retaliation. `ca_bundle_path` is reserved for a follow-up release, setting it today causes the scraper to refuse to start with a clear error message, and self-signed BMC certificates require a reverse proxy with publicly-signed TLS for now. IPMI is explicitly out of scope, session-token auth via `/SessionService/Sessions` is also out of scope, Basic auth via `auth_header` is the only supported credential path. New Prometheus surfaces: `perf_sentinel_redfish_last_scrape_age_seconds` (gauge), `perf_sentinel_redfish_scrape_total{status}` (counter), `perf_sentinel_redfish_scrape_failed_total{reason}` (counter, 9 reasons pre-warmed, including `invalid_json`, `path_missing`, `invalid_value` for vendor variance).

### Changed

- **Carbon precedence chain extended.** New canonical order: `electricity_maps_api > scaphandre_rapl > kepler_ebpf > redfish_bmc > cloud_specpower > io_proxy_v3 > io_proxy_v2 > io_proxy_v1`. Where multiple sources are configured for the same service, the highest-fidelity one wins. Existing Scaphandre and cloud `SPECpower` behavior is unchanged, a service already served by Scaphandre keeps Scaphandre regardless of Kepler or Redfish being configured. The `OpsSnapshotDiff` per-service ops-delta tracker was extracted from `score::scaphandre::ops` into a shared sibling `score::ops_snapshot_diff`, so all four measured-energy scrapers (Scaphandre, Kepler, Redfish, cloud `SPECpower`) share one implementation.
- **The `co2.model` enum gains two values (`kepler_ebpf`, `redfish_bmc`).** The field is already documented as an open string set populated dynamically, so 0.7.3 consumers continue to parse it without changes. Numerical carbon shifts on existing workloads only occur when an operator explicitly enables Kepler or Redfish, in which case the new measured coefficient replaces the proxy fallback for the mapped services.

### Documentation

- **README restructured** around a TL;DR header with a documentation index and inline integration diagrams. The GreenOps section was reframed from "waste counter" to "specialized carbon calculator" with the compliance angle surfaced, and a still-frame embedded for every feature in both EN and FR.
- **New primer hubs in `docs/`:** SCI v1.0 and energy-tooling primer in `METHODOLOGY.md`, OpenTelemetry and Prometheus primers in `INSTRUMENTATION.md`, a Sigstore primer hub in `SUPPLY-CHAIN.md` with cross-references throughout the doc tree. The workflow doc now glosses ack signature, JSONL, quality gate, and SARIF.
- **`docs/LIMITATIONS.md`** now carries a complete ARM64 coverage section explaining how Scaphandre RAPL, Kepler eBPF, and Redfish BMC coexist on ARM hosts, an expanded RAPL coverage discussion alongside a software-only-tool accuracy note, and two new sections "Kepler precision bounds" and "Redfish BMC precision bounds".
- **FR doc tree audit pass** for missing accents, anglicisms, and prose semicolons. FR README cross-references now point at FR mirrors instead of EN docs.
- **Diagrams:** `docs/diagrams/mmd/daemon.mmd` and `docs/diagrams/mmd/carbon-scoring.mmd` updated to expose Kepler and Redfish in the scraper subgraph and the energy resolution flow, and the CLI-commands diagram refreshed.

### Internal

- **Helm chart 0.2.38 to 0.2.39, `appVersion` 0.7.3 to 0.7.4.** The `artifacthub.io/changes` annotation surfaces the new Kepler and Redfish backends.

## [0.7.3]

GreenOps reference data refresh. The Rust API, the daemon wire protocol, the report JSON schema, and the entire CLI surface are preserved byte-for-byte from 0.7.2. Carbon numbers shift on existing workloads because the embedded coefficients are aligned with newer upstream sources. The shifts come from reference data, not from a model or formula change, so the `cloud_specpower` energy model tag is intentionally not bumped.

### Changed

- **Per-provider PUE constants refreshed from the latest sustainability reports.** AWS 1.135 to 1.15 (2024 global fleet), GCP 1.10 to 1.09 (TTM 2024), Azure 1.185 to 1.17 (FY25 owned-and-controlled fleet), Generic unchanged at 1.2. Impact on operational CO2: approximately +1.3 percent on AWS regions, -0.9 percent on GCP regions, -1.3 percent on Azure regions. The sources are cited inline in `crates/sentinel-core/src/score/carbon.rs` and `docs/design/05-GREENOPS-AND-CARBON.md`. The new vintage is exposed at runtime via `score::cloud_energy::embedded_specpower_vintage()` and via the `PUE_VINTAGE` const for release procedure audits.
- **Embedded `INSTANCE_POWER` table regenerated from the CCF coefficients snapshot 2026-04-24** (commit `b0032d928c78`) across AWS, GCP, and Azure. About 390 instance types covered with a single homogeneous methodology: `idle_watts = vCPU * idle_per_vCPU` and `max_watts = vCPU * max_per_vCPU`. The AWS-specific baseboard overhead column from the 2023-05-01 snapshot is no longer published upstream and is dropped uniformly. Modern entries (Sapphire Rapids, EPYC Genoa, Graviton, Emerald Rapids on supported providers) are cross-checked against CCF with a 5 percent rule: re-aligned to CCF on divergence, kept on the previous `SPECpower_ssj 2008` direct compute otherwise. Architectures absent from a provider's CCF CSV (Azure Sapphire Rapids, Azure Emerald Rapids, Azure Genoa, GCP Turin, GCP Ampere Altra, Azure Cobalt 100) keep their SPECpower direct value and are labelled in the table.
- **Practical impact on existing workloads:** AWS legacy families (`m5`, `c5`, `r5`, `m6i`, etc.) drop 30 to 60 percent on operational CO2 because baseboard overhead is no longer layered on top. Sapphire Rapids on AWS (`m7i`, `c7i`, `r7i`) and GCP (`c3`) rise about 19 percent on max watts and 48 percent on idle watts. EPYC Genoa on AWS (`m7a`, `c7a`) and GCP (`c3d`, `n2d`) rise about 11 percent on max watts and 85 percent on idle watts. Graviton 2 / 3 / 3E / 4 align with CCF's EPYC 2nd Gen proxy, which erases the AMD vs Graviton vs Intel idle differentiation across generations. Azure legacy entries are unchanged, the CCF Azure CSV happened to publish the same values already embedded.

### Added

- **New instance families.** AWS `m8a` / `c8a` (EPYC 5th Gen Turin, proxied to Genoa pending an upstream CCF correction, the CCF 2026-04-24 Turin row is 5x higher than neighbouring architectures, likely a chip-vs-thread measurement error in the upstream `SPECpower` submission, see `docs/LIMITATIONS.md` "EPYC 5th Gen Turin"), AWS `m8i` / `c8i` (Intel Emerald Rapids, CCF row, no override), AWS `r7a` (EPYC 4th Gen Genoa memory-optimized, mirrors `m7a` / `c7a` plus the DRAM premium), and GCP `c4a` (Google Axion ARM on Neoverse V2, no native ARM row in the GCP CSV, proxied to AWS Graviton 4, itself mapped by CCF to EPYC 2nd Gen as a conservative placeholder). Granite Rapids (`m9i`) is intentionally out of scope for this release, the silicon has not landed broadly enough on AWS to justify embedding coefficients.
- **Memory-optimized DRAM premium.** Memory-optimized SKUs now carry an additive DRAM premium on top of the per-vCPU CPU coefficient: `0.02 W/GB` idle and `0.05 W/GB` max, sourced from Crucial DDR4 RDIMM datasheets and the Boavizta DIMM model. At the 8 GB / vCPU ratio of these families, that translates to per-vCPU uplifts of `+0.16` idle / `+0.40` max. Applied on AWS `r5`, `r5a`, `r6i`, `r7i`, `r7a`, GCP `n2-highmem-*`, and Azure `Standard_E*` v3 through v6. Compute-optimized (`c*`) and general-purpose (`m*`) families do not receive the premium and are documented as carrying a small idle under-count, inside the 2x uncertainty bracket. This is the only methodology departure from the CCF CSV beyond the Turin override.
- **Binary `SPECpower` vintage surfaced in disclosure reports.** Periodic disclosure reports produced by `perf-sentinel disclose` include a new optional field `methodology.calibration_inputs.binary_specpower_vintage`, populated automatically from the running binary via `score::cloud_energy::embedded_specpower_vintage()` and independent from the operator-declared `methodology.calibration.specpower_table_version` in the org config TOML, so consumers can compare both strings to detect drift between operator disclosure and embedded data. For reports with `intent = "official"`, the validator now rejects the report when the declared `specpower_table_version` does not match the binary's vintage on an ISO date prefix exact match, substring matches that previously slipped through ("2026", "CCF", and similar) are now flagged. Operators should update their org config TOML to `specpower_table_version = "2026-04-24"` before producing Official disclosures, Internal-intent reports are unaffected. The JSON schema `docs/schemas/perf-sentinel-report-v1.json` adds the field with `maxLength: 64` defense-in-depth and bumps the example fixtures G1 and G2.

### Documentation

- **`docs/LIMITATIONS.md`** and **`docs/FR/LIMITATIONS-FR.md`**: section "Cloud `SPECpower` precision bounds" rewritten around the single homogeneous methodology, with new paragraphs covering the Turin override rationale, the DRAM premium, and the m-series / c-series under-count.
- **`docs/design/05-GREENOPS-AND-CARBON.md`** and FR mirror: `table.rs` description updated to reflect the methodology after the refresh, with the explicit split between CCF-aligned modern entries and `SPECpower` direct-compute exceptions.
- **`docs/SCHEMA.md`** and FR mirror: new field `binary_specpower_vintage` documented under methodology.
- **`docs/RELEASE-PROCEDURE.md`** and FR mirror: step 2.5 updated, both `CCF_LEGACY_VINTAGE` and `SPECPOWER_VINTAGE` now point at the 2026-04-24 snapshot, with bump rules clarified.
- **CI templates and `docs/CI.md` / FR mirror:** `PERF_SENTINEL_VERSION` pinned to 0.7.3.

### Internal

- **Helm chart 0.2.37 to 0.2.38, `appVersion` 0.7.2 to 0.7.3.** The `artifacthub.io/changes` annotation surfaces the GreenOps refresh and the new disclosure field.

## [0.7.2]

Adds the `perf-sentinel hash-bake` subcommand and applies a defense-in-depth hardening pass across two parallel surfaces: terminal rendering and TOML config validation. No breaking change on the daemon, the `disclose`, `verify-hash`, or report JSON contracts published in 0.7.1, and no legitimate input that was accepted in 0.7.1 is rejected by 0.7.2 (C1 control bytes are not part of any specified TOML or JSON form).

### Added

- **New `perf-sentinel hash-bake` subcommand.** `hash-bake --report <PATH> --output <PATH> [--allow-signed]` reads the report, recomputes the canonical SHA-256 `content_hash` via the same `compute_content_hash` API the disclosure pipeline already uses, writes the hash into `integrity.content_hash`, and saves the result via an atomic temp+rename. Intended for test fixture generation and for debugging reports whose hash drifted from canonical after manual edits. Exit codes: 0 (success), 1 (refused: report already signed and `--allow-signed` not passed), 3 (input error: unreadable, JSON invalid, oversized, temp file collision, write failure). The output writes `integrity.content_hash` only, `integrity.signature`, `integrity.binary_attestation`, and `report_metadata.integrity_level` are not touched.
- **Atomic write hardened** with `OpenOptions::create_new(true)` plus `O_NOFOLLOW` on unix. A stale or symlinked `<output>.tmp` aborts the bake with exit 3 instead of being silently clobbered. The temp filename appends `.tmp` to the output path rather than replacing the extension, so a path like `report.tmp` does not collide with itself.
- **64 MiB input size cap** enforced via `fs::metadata` precheck before allocation, so a multi-GB JSON fed by a poisoned mirror or a recursive symlink target is rejected without allocating.
- **Signed reports refused by default.** Re-baking does not invalidate an existing signature (the canonical form blanks `integrity.signature`), but the default refusal guards against accidental overwrites of signed disclosures. Opt in with `--allow-signed`.

### Security

- **C1 control range stripped at every terminal-bound surface.** `sentinel_core::text_safety::sanitize_for_terminal` and `safe_url` now strip the C1 control range `0x80..=0x9F` (CSI `U+009B`, ST `U+009C`, OSC `U+009D` honoured by VT-family terminals when 8-bit controls are enabled). The filter switched from a `bytes()` scan to a `chars()` scan to catch the multi-byte UTF-8 encoding of C1 codepoints. Workspace-wide impact, every render boundary in `render.rs`, `tui.rs`, `ack.rs`, `query.rs`, `explain.rs`, `disclose.rs`, `verify_hash.rs`, `html.rs` benefits transitively.
- **`sentinel_core::config::has_control_char` extended with the same C1 range.** A malicious `.perf-sentinel.toml` placing a C1 byte in `disclose_output_path`, `auth_token`, or any field that ends up formatted into a `tracing::warn!` line on stderr can no longer survive load-time validation. The TOML loader is the right gate, the warning emission path does not route through `sanitize_for_terminal`.

### Changed

- **`verify-hash --report <local>` gains the same 64 MiB cap as `hash-bake`,** gated by an `fs::metadata` precheck (it previously had no size cap on the local file). Remote `--url` mode is unaffected, it keeps the tighter `MAX_REMOTE_BYTES = 10 MiB` cap. Error wording and path sanitisation are aligned between the two subcommands so an operator parsing stderr gets consistent matching on either failure path. The cap value lives in a shared `crates/sentinel-cli/src/limits.rs::MAX_LOCAL_REPORT_BYTES` constant consumed by both modules.

### Documentation

- **`docs/REPORTING.md`** and **`docs/FR/REPORTING-FR.md`**: new section "Computing a canonical content hash with `hash-bake` (0.7.2+)" with the exit code table.
- **`docs/design/10-SIGSTORE-ATTESTATION.md`** and FR mirror: new "Tooling: `hash-bake`" paragraph positioning the subcommand as a fixture and debug tool complementing the disclosure pipeline, not part of the signed-disclosure chain itself.
- **Example reports** under `docs/schemas/examples/` bump their `binary_verification_url` to 0.7.2.

### Internal

- **Helm chart 0.2.36 to 0.2.37, `appVersion` 0.7.1 to 0.7.2.** The `artifacthub.io/changes` annotation surfaces the `hash-bake` addition on Artifact Hub.

## [0.7.1]

Supply-chain maintenance release. The SLSA build provenance tooling moves from `slsa-framework/slsa-github-generator@v2.1.0` (in de-facto maintenance since 2025-02-24, all internal actions stuck on Node.js 20 while GitHub-hosted runners switch to a Node 24 default on 2 June 2026) to GitHub-native `actions/attest-build-provenance`. The new pipeline produces a SLSA Build L3 attestation (level up from L2), stores it on the GitHub attestations API instead of a release asset, and is verified with `gh attestation verify` instead of `slsa-verifier verify-artifact`. Daemon and `verify-hash` behavior on already-clean inputs is preserved byte-for-byte from 0.7.0.

### BREAKING CHANGES

**Downstream binary verification recipe changed.** A script that fetched `multiple.intoto.jsonl` from the release assets and ran `slsa-verifier verify-artifact --provenance-path multiple.intoto.jsonl ...` no longer works on 0.7.1+ binaries, the asset is no longer published. Migration on the consumer side:

```bash
gh attestation verify perf-sentinel-linux-amd64 \
  --owner robintra \
  --repo perf-sentinel
```

Requires `gh` CLI 2.49+ (earlier versions do not implement `gh attestation verify`). The v0.7.0 release retains its legacy `multiple.intoto.jsonl` and is unaffected, the breaking change applies only to 0.7.1 onward.

### Changed

- **`actions/attest-build-provenance@v4.1.0` replaces the previous reusable workflow.** SHA-pinned in `.github/workflows/release.yml`, same pattern as `helm-release.yml` was already using for the chart attestation.
- **Two release-workflow jobs collapsed:** `compute-subjects` (base64-encoded SHA list, was an input to the reusable generator) and the standalone `provenance` reusable workflow call are removed. The attestation step now runs inside the existing `release` job, right after the SHA256SUMS generation.
- **SLSA level claim bumped from L2 to L3.** `actions/attest-build-provenance` produces a level-3 attestation by construction (provenance signed via Sigstore OIDC, builder isolation on a GitHub-hosted runner), so the `integrity.binary_attestation.slsa_level` field declared in disclosure reports now reads `"L3"` for 0.7.1+ builds.
- **The attestation lives in the GitHub attestations API,** queryable by binary digest. No release-asset payload to mirror or republish, the trust root is the GitHub OIDC signing identity in the public Sigstore Rekor log.
- **`verify_binary_attestation` hint updated.** It now prints the `gh attestation verify <binary> --owner robintra --repo perf-sentinel` recipe instead of the previous `slsa-verifier verify-artifact --provenance-path ...` one, and the PARTIAL exit code (`2`) trigger is now `gh` CLI absent instead of `slsa-verifier` absent. The behavior of a scripted `verify-hash && deploy` gate is unchanged, it still blocks on any non-zero code. No change to content hash recompute, Sigstore signature verification, identity binding, or any other check, the migration is scoped to the SLSA verification slot only.

### Documentation

- **`docs/SUPPLY-CHAIN.md` and `docs/FR/SUPPLY-CHAIN-FR.md`** section "SLSA build provenance" rewritten end-to-end with the new `gh attestation verify` command, the `gh` CLI prerequisite, and a migration note for consumers still pinned on v0.7.0.
- **`docs/REPORTING.md`** plus FR mirror: section "Binary build provenance" and the exit code table cell for PARTIAL updated.
- **`docs/SCHEMA.md`** plus FR mirror: section "Integrity" documents both verification commands (the legacy one for v0.7.0, the new one for 0.7.1+) and explains the `slsa_level` enum bump.
- **`docs/METHODOLOGY.md`** plus FR mirror: section "Cryptographic integrity" mentions both pipelines side by side.
- **`docs/design/10-SIGSTORE-ATTESTATION.md`** plus FR mirror: section "Failure modes" rewrites the binary attestation delegation paragraph.
- **Example reports under `docs/schemas/examples/`** bump their `integrity.binary_verification_url` to 0.7.1.

### Internal

- **Helm chart 0.2.35 to 0.2.36, `appVersion` 0.7.0 to 0.7.1.** The `artifacthub.io/changes` annotation surfaces the SLSA migration and the breaking-change recipe on Artifact Hub.

## [0.7.0]

Introduces the public periodic disclosure pipeline. A new `perf-sentinel disclose` subcommand aggregates an archived NDJSON window stream into a single period-level JSON report with deterministic content hashing and an in-toto v1 attestation sidecar. A new `perf-sentinel verify-hash` subcommand chains content hash recompute, Sigstore signature verification, and a SLSA L2 binary provenance check in one third-party-runnable command. Carbon accounting moves from aggregate to per-service attribution when runtime calibration is available, and an official disclosure requires 75 percent per-service coverage to be accepted.

### BREAKING CHANGES

**`verify-hash` requires identity binding.** It now refuses to invoke cosign without operator-supplied identity flags. Three modes: `--expected-identity <ID> --expected-issuer <URL>` (cosign verifies the bundle was issued by exactly this OIDC identity, the safe default for a third-party audit), `--no-identity-check` (cryptographic integrity only, explicitly logged as PARTIAL, reserved for internal self-check before publication), or neither flag passed (`Status::Fail` on the signature slot). Passing the report-supplied `signer_identity` and `signer_issuer` to cosign as constraints was autosigning: any GitHub or Google account holder could forge a bundle and have `verify-hash` return TRUSTED. The new contract forces the consumer to declare the expected signer.

### Added

- **`perf-sentinel disclose` subcommand** aggregates an NDJSON stream of per-window reports into one period-level public document. The stream is produced by the daemon's new `[daemon.archive]` writer (size-rotated, count-pruned). Two granularity levels via `--confidentiality`: `internal` (G1, full per-pattern detail per service) or `public` (G2, anti-pattern counts only). `--strict-attribution` refuses windows with non-attributed spans, useful when asserting that 100 percent of measured operations were correctly attributed. Full flag surface: `--intent` (`internal | official | audited`), `--confidentiality`, `--period-type`, `--from`, `--to`, `--input` (file or directory, repeatable), `--output`, `--org-config`, `--strict-attribution`. The `audited` intent is accepted for forward-compat but the CLI returns exit code 2 with `Error: audited intent is not yet implemented`.
- **In-toto v1 attestation sidecar** via `--emit-attestation`, a complete statement (`_type`, `predicateType`, `subject`, `predicate`) ready to sign with `cosign sign-blob --bundle bundle.sig --new-bundle-format`. The predicate carries pattern counts for audit visibility.
- **Deterministic `integrity.content_hash`,** invariant under post-disclose signature insertion. The hasher blanks `integrity.content_hash`, `integrity.signature`, `integrity.binary_attestation`, and `report_metadata.integrity_level` before computing, so an operator can patch the signature locators into `report.json` without breaking the hash.
- **`integrity.core_patterns_required` and `core_patterns_hash`** declare which canonical anti-pattern set produced the report. `verify-hash` cross-checks the hash against the local binary's canonical set, catching a substitution attempt where a hostile report claims patterns the running binary cannot detect.
- **`perf-sentinel verify-hash` subcommand** chains three checks: deterministic content hash recompute (pure Rust, always run), Sigstore signature verification via `cosign verify-blob --new-bundle-format`, and a SLSA L2 binary provenance summary with a `slsa-verifier` command pointing at the binary in `integrity.binary_verification_url`. Five distinct exit codes: `0` TRUSTED, `1` UNTRUSTED (hash mismatch, signature invalid, identity mismatch), `2` PARTIAL (cosign or slsa-verifier absent, sidecars missing), `3` INPUT_ERROR, `4` NETWORK_ERROR. A scripted `verify-hash && deploy` gate still blocks on non-zero, but a wrapper distinguishing 2 vs 1 can tell tooling absence from a tamper attempt.
- **Remote verification mode** with `--url <report.json>` fetches the report, `attestation.intoto.jsonl`, and `bundle.sig` from the same URL prefix, allowing a third-party auditor to verify a publicly-hosted disclosure without cloning the producer's infrastructure.
- **Per-service carbon attribution.** `GreenSummary` now carries energy and carbon at per-service granularity when the scoring pipeline observed runtime calibration: `per_service.{energy_kwh, carbon_kg, energy_source_model, measured_ratio}` populated when a window's per-endpoint energy attribution is present, `calibration_inputs.energy_source_models` listing the distinct energy models observed in the period so an auditor sees which scope (`measured`, `proxy_io`, ...) the totals lean on, `runtime_windows_count` and `fallback_windows_count` in the aggregate distinguishing per-service-attributed windows from those that fell back to the I/O proxy, and a `period_coverage` field exposing the runtime-calibration coverage ratio as a first-class metric in the disclosure.
- **75 percent runtime coverage gate.** An `intent = "official"` disclosure requires `runtime_windows_count / (runtime_windows_count + fallback_windows_count) >= 0.75`. Below 75 percent, the I/O proxy dominates the totals and per-service attribution loses meaningful coverage. The gate is enforced at `disclose --intent official` time and at daemon startup when `[reporting] intent = "official"` is configured.
- **Configurable Rekor URL** via `[reporting.sigstore] rekor_url`, defaults to the public Rekor.
- **SLSA L2 binary provenance** for every release binary via `slsa-framework/slsa-github-generator`. The release publishes `multiple.intoto.jsonl` alongside the platform binaries and Docker image, with subject hashes gated on the build step succeeding.
- **New `report::periodic` module in `sentinel-core`** with five submodules: `schema` (v1.0 wire types, `BTreeMap` everywhere for hash determinism), `validator` (`validate_official` collecting every error), `hasher` (canonical JSON + SHA-256, hex via `{byte:02x}` with no `hex` crate), `aggregator` (NDJSON file/dir reader with period filtering and per-service attribution), and `org_config` (operator-supplied TOML loader for organisation/methodology/scope_manifest).
- **New `[reporting]` config section** in `.perf-sentinel.toml` exposing `intent`, `confidentiality_level`, `org_config_path`, `disclose_output_path`, `disclose_period`. Validated at config load.
- **New `[daemon.archive]` opt-in section** writes one NDJSON line per scoring window (`{ts, report}` envelope) with size-triggered rotation (`max_size_mb`, default 100) and count-based pruning (`max_files`, default 12). Hooked into the daemon's per-window scoring path behind a `tokio::sync::mpsc::UnboundedSender<String>` so disk I/O never blocks the hot path.

### Changed

- **The in-toto v1 statement is signed with `cosign sign-blob`** instead of `attest-blob`, which would wrap the statement in a second statement and create a permanent malformed entry in the public Rekor log. The migration covers both signing and verifying paths and requires cosign 2.4+ in the signing pipeline.
- **`DaemonError` gains two `#[non_exhaustive]` variants:** `ReportingValidation { errors: Vec<String> }` (returned at startup when `[reporting] intent = "official"` is configured but the org-config is missing fields required for a publishable disclosure) and `ArchiveOpen { path, source }` (returned when the configured archive file cannot be opened).
- **The daemon's per-window scoring path keeps `per_endpoint_io_ops` instead of dropping it.** The aggregator needs per-service I/O shares for proportional energy/carbon attribution, and the value is already computed by `score_green`'s single-pass span iteration, so the daemon now propagates it into the optional archive envelope. No effect on the existing `/metrics`, NDJSON-on-stdout, or `/api/export/report` surfaces.

### Documentation

- **`docs/REPORTING.md` and `docs/FR/REPORTING-FR.md`** document the disclosure pipeline end-to-end: `--period-type`, glob `--input` behavior, G1 vs G2 granularity, the `integrity.signature` schema with per-field provenance, an interim `jq` helper to patch locator fields between sign and publish, the URL convention for `--url` sidecars, identity verification modes, and build provenance for local builds.
- **`docs/SCHEMA.md`** plus **`docs/schemas/perf-sentinel-report-v1.json`** publish the formal JSON schema for the disclosure document. The example reports in `docs/schemas/examples/` track the 0.7.0 baseline.
- **`docs/design/08-PERIODIC-DISCLOSURE.md`, `09-CARBON-ATTRIBUTION.md`, and `10-SIGSTORE-ATTESTATION.md`** record the methodology and the constraints that shaped the implementation.
- **New documentation:** `docs/METHODOLOGY.md`, an operator-facing `docs/examples/perf-sentinel-org.toml`, and a README section "Public reporting". French translations under `docs/FR/`.

### Internal

- **`cognitive_complexity` clippy gate** enforced workspace-wide at threshold 60, with a pre-commit hook running clippy on staged Rust files.
- **`process_window`, `validate_methodology`, and `score_green`** refactored into per-axis helpers, all below the new threshold.
- **Daemon advisory warnings** for `[reporting] disclose_output_path` (reserved for 0.8.0) emit exactly once at startup, including when the daemon CLI overrides the listen address.
- **New deps:** `uuid = "1.23.1"` (features `v4` + `serde`) in `sentinel-core`, same line in `sentinel-cli`. Pulls `getrandom 0.4` transitively. No removal.
- **`Aggregate`, `Notes`, and `DaemonArchiveConfig` derive `Default`** for builder ergonomics on the disclose path.
- **Helm chart 0.2.34 to 0.2.35, `appVersion` 0.6.2 to 0.7.0.** The `artifacthub.io/changes` annotation surfaces the disclosure pipeline and the autosigning fix on Artifact Hub.

## [0.6.2]

Visual polish on the HTML dashboard. Two cramped layouts on the `Explain` and `pg_stat` panels are now properly spaced.

### Changed

- **Explain breadcrumb renders as a chip** with `padding: 8px 12px`, a `--color-background-tertiary` tint and `--border-radius-md`, instead of bare 11px text flush against the span tree below. Same per-trace context (`trace_id . service . endpoint`), visually detached from the tree.
- **`.ps-drill` (the "Filtered from Explain" banner on the `pg_stat` tab and the suggested-fix box at the bottom of `Explain`) carries a `margin-bottom: 12px`** so the elements below the banner (ranking-tab chips on `pg_stat`, page footer on `Explain`) no longer sit flush against it.

### Documentation

- **0.6.1 remeasurement results** documented alongside the benchmarking methodology, so the numbers reported in the 0.6.1 notes are reproducible from the docs.
- **TUI ack flow demo** with VHS-recorded GIF and screenshots, mirroring the 0.5.24 keyboard-driven `a` / `u` flow.
- **`perf-sentinel ack` CLI demo script** added as a VHS tape, with screenshots and a GIF documenting the `create` / `list` / `revoke` flow shipped in 0.5.22.
- **PR template added and contributing guide expanded** with the fixture and demo-asset regeneration pipelines (VHS for terminal GIFs, `npm run demo` for dashboard tour assets).
- **CI templates and the snippets in `docs/CI.md` / `docs/FR/CI-FR.md`** pin `PERF_SENTINEL_VERSION = 0.6.2`, catching up from the 0.5.x baselines that had drifted.

### Internal

- **Helm chart 0.2.33 to 0.2.34, `appVersion` 0.6.1 to 0.6.2.** The `artifacthub.io/changes` annotation surfaces the dashboard polish on Artifact Hub.

## [0.6.1]

Hardening pass driven by an internal multi-reviewer audit (Rust idioms, hot-path performance, security). The CORS-vs-`api_key` interaction is now a hard config-load error instead of a startup `WARN`, the CI ack TOML loader refuses to follow symlinks, SARIF result bodies pass through the BiDi sanitizer that was already in place for ack metadata, and the OTLP gRPC listener caps HTTP/2 stream multiplexing at 256 per connection. No public surface change beyond the new validation error.

### Security

- **CORS wildcard combined with `[daemon.ack] api_key` is rejected at config load** instead of warned at startup. Header-based `X-API-Key` auth is not blocked by `allow_credentials = false`, so wildcard CORS plus an API key let any browser origin replay a captured key. Operators that want wildcard CORS for development must now explicitly unset `api_key`.
- **`acknowledgments::load_from_file` refuses to follow symlinks** on the CI baseline TOML path, mirroring the daemon JSONL store discipline. Closes the "hostile collaborator plants a symlink to a build secret in a CI runner working tree" vector.
- **SARIF `finding_to_result` strips BiDi and invisible-format characters** from the message body and logical locations, in addition to the ack metadata path that was already sanitized. A hostile span emitting `service.name = "alice<RLO>@evil"` no longer renders mirrored in GitHub or GitLab code-scanning UIs.
- **OTLP gRPC listener caps HTTP/2 concurrent streams at 256 per connection** via tonic's `max_concurrent_streams` and `concurrency_limit_per_connection`. Bounds the blast radius of a misbehaving client on non-loopback binds.

### Changed

- **`detect::n_plus_one::parse_timestamp_ms` is a thin adapter over `crate::time::parse_iso8601_utc_to_ms`** instead of a duplicate implementation. The shared `time.rs` module is now the single source of truth for civil-date arithmetic across the crate.
- **`default_region` is lowercased once at config load**, mirroring the existing `service_regions` discipline so downstream resolvers no longer pay a `to_ascii_lowercase` allocation per call.
- **`OtlpRejectReason::as_str`, `AckFailureReason::as_str`, `ScaphandreScrapeReason::as_str` are `const fn`** matching the pattern already in place on `Confidence::as_str` and `FindingType::as_str`.
- **OTLP span-index cap is named `MAX_SPANS_PER_RESOURCE`** instead of two duplicated `100_000` literals in `build_span_index` and `build_scope_index`.
- **`OtlpRejectReason::ALL`** fixed-size array exposes every variant for exhaustive pre-warming, keeping the `MetricsState::new` startup loop drift-free.

### Performance

- **Probe-before-allocate exemplar sanitization** in `report::metrics::sanitize_exemplar_value` (returns `Cow<'_, str>`). Trace IDs are almost always already valid hex and now skip the allocation on the hot path.
- **`chatty` detection runs in a single pass** over the trace's HTTP-out spans (count and indices collected together) instead of two iterations.
- **`serialized` detection sorts via `sort_unstable_by_key`** on `u64` end timestamps, faster than the stable variant with no observable difference downstream.
- **HTTP query-param `Vec` is pre-sized from the ampersand count**, capped at 100, eliminating the doubling-growth path on URLs with many parameters.
- **Avoidable-finding dedup `HashMap` capacity matches the avoidable-finding count** instead of the total finding count, removing the over-allocation when most findings are slow or fanout.

### Internal

- **`opentelemetry-proto` 0.31 → 0.32 / `tonic` 0.14.5 → 0.14.6 / `tokio` 1.52.2 → 1.52.3.** The OTel-proto bump added `KeyValue::key_strindex` for the OTel Profiling signal, three test sites in `ingest/otlp.rs` now initialize via `..Default::default()` to stay forward-compatible.
- **`Config::validate_daemon_cors` cognitive complexity reduced** to satisfy SonarCloud `rust:S3776` (was 16, threshold 15). Per-origin and wildcard-mode checks live in two free functions, the orchestrator is ten lines.
- **Helm chart 0.2.32 to 0.2.33, `appVersion` 0.6.0 to 0.6.1.** The `artifacthub.io/changes` annotation surfaces the security and performance items on Artifact Hub.

## [0.6.0]

First SemVer-incompatible release of the 0.x line. Three things break together: the public `Config` API splits into sectioned sub-structs (`thresholds`, `detection`, `green`, `daemon`), the eight legacy top-level keys deprecated since 0.5.26 are removed so a 0.5.x `.perf-sentinel.toml` that still uses any of them now fails at load with an explicit migration error, and the OTLP ingest path no longer reaches into `report::metrics::MetricsState` directly, a new `MetricsSink` trait owned by `ingest` closes a long-standing layering leak. The audit pass also lands its first wave of user-facing lexicon alignments. Wire formats are unchanged: JSON, SARIF, HTML, TOML ack store, and Prometheus `/metrics` outputs are byte-identical to 0.5.28 for already-clean inputs, no data on disk needs migration, only the configuration file and library imports.

### BREAKING CHANGES

**1. Config sectioned API.** `Config` splits into four sub-structs reachable as `config.thresholds`, `config.detection`, `config.green`, `config.daemon`. Library consumers that read `config.green_default_region`, `config.tls_cert_path`, `config.n_plus_one_threshold` and similar flat fields need to migrate to the nested `config.<section>.*` form. The `RawConfig` to `Config` adapter still accepts the section plus flat-keys mix on disk for keys that have a section equivalent, the eight legacy top-level keys below are the only ones that now hard-fail.

**2. Eight legacy top-level config keys removed.** The keys deprecated with a `WARN` since 0.5.26 are gone. Loading a `.perf-sentinel.toml` that still uses any of them returns a `ConfigError::Validation` whose message names both the removed key and its replacement, so a single pass on the load error tells you exactly what to edit:

- `n_plus_one_threshold` becomes `n_plus_one_min_occurrences` in `[detection]`.
- `window_duration_ms` keeps its name and moves to `[detection]`.
- `n_plus_one_sql_critical_max`, `n_plus_one_http_warning_max`, and `io_waste_ratio_max` keep their names and move to `[thresholds]`.
- `listen_port` becomes `listen_port_http` in `[daemon]`.
- `max_events_per_trace` and `max_payload_size` keep their names and move to `[daemon]`.

**3. `MetricsSink` trait closes the ingest to report leak.** Before 0.6.0 the OTLP ingest path imported `report::metrics::MetricsState` directly to record per-protocol rejection counters. That meant `ingest` could only be enabled when `report::metrics` was compiled in, and `MetricsState` was part of every public OTLP handler signature. The new `pub trait MetricsSink: Send + Sync` in `ingest/otlp.rs` has one method, `record_otlp_reject(&self, reason: OtlpRejectReason)`, and `MetricsState` implements it in `report/metrics.rs`. The dependency direction is inverted: `ingest` no longer depends on `report`, `report` provides an implementation of the trait `ingest` defines. CLI consumers see no behavior change, the `/metrics` exemplars are byte-identical for already-clean inputs.

### Added

- **`MetricsSink` trait** in `crates/sentinel-core/src/ingest/otlp.rs`, decoupling the OTLP ingest from `MetricsState`. `MetricsState` is the only built-in implementation today, the OTLP handlers now accept any `Arc<dyn MetricsSink>`.
- **`pub const API_KEY_HEADER: &str = "X-API-Key";`** in `crates/sentinel-core/src/http_client.rs`. The daemon `check_ack_auth` and the outbound `fetch_with_body` both consume it through this single constant, and the new `template_propagates_api_key_header_constant` test asserts the live-mode JS propagates the same constant, replacing eight literal-string drift guards with one typed lockstep check.
- **Glossary section** in `docs/ARCHITECTURE.md` (and FR mirror) covering `event` / `finding` / `pattern` / `detection` plus the four operating modes (`batch`, `CI`, `daemon`, `watch`) and the `Confidence` axis.
- **`docs/ACK-WORKFLOW.md` "Service renames invalidate acks" section** (and FR mirror), documenting how `service.name` renames, `http.route` refactors, and SQL/HTTP template churn invalidate existing acknowledgments.
- **`docs/LIMITATIONS.md` "Long-running traces and TTL eviction" section** (and FR mirror), explaining sparse-burst undercounting in streaming mode and the mitigation knobs.
- **Grafana Pyroscope column** in the README comparison table and a "Not a continuous profiler" entry. The framing is complementary, not competitive: Pyroscope tells you where compute time goes, perf-sentinel tells you which I/O patterns drive that time.

### Changed

- **CLI: `Found N issue(s)` becomes `Found N finding(s)`.** The CLI integration test asserts the new wording so it stays in lockstep with the data model and the JSON / SARIF / API surfaces.
- **`fan-out` becomes `fanout`** across EN and FR docs, aligning prose with the `ExcessiveFanout` enum and the snake_case `excessive_fanout` finding label.
- **`docs/SUPPLY-CHAIN.md` `Acknowledgement` aligned to `Acknowledgment`** to match the 386 US-spelling occurrences elsewhere.
- **`live_mode_acks_cap_matches_daemon_constant` parses `DAEMON_ACKS_CAP` from the template** and asserts equality against `daemon::query_api::MAX_ACKS_RESPONSE` instead of a one-sided literal `1000`.
- **OTLP HTTP and gRPC handlers and the daemon listener layer accept `Option<Arc<dyn MetricsSink>>`** instead of `Option<Arc<MetricsState>>`.

### Removed

- **8 legacy top-level config keys** (`n_plus_one_threshold`, `window_duration_ms`, `n_plus_one_sql_critical_max`, `n_plus_one_http_warning_max`, `io_waste_ratio_max`, `listen_port`, `max_events_per_trace`, `max_payload_size`). Loading hard-fails with `ConfigError::Validation` instead of falling back to the section default with a `WARN`.
- **8 tautological `template_carries_*` tests** in `crates/sentinel-core/src/report/html.rs`, each asserting that a literal HTML id, class, or function name appeared in the static template with no semantic check. Tests with real semantic value are kept (`template_carries_scoring_config_bandeau_and_helpers`, `template_carries_estimated_column_and_helper`, `template_carries_csp_placeholder`). Net: -8 tests, +1 test, behavior unchanged.
- **`tracing-test = "0.2.6"` dev-dependency** on `perf-sentinel-core`. It was only used by the legacy-flat deprecation tests, which the legacy keys removal made redundant.

### Internal

- **`Config` is four sub-structs** (`thresholds`, `detection`, `green`, `daemon`). The `RawConfig` to `Config` adapter still accepts the section plus remaining flat-keys mix on disk. Per-section `validate_*` functions centralize range and consistency checks.
- **CI fix:** `live_mode_acks_cap_matches_daemon_constant`, the `fresh_metrics_sink` test helper, and the `MetricsState` test import in `ingest/otlp.rs` are now gated by `#[cfg(feature = "daemon")]`. `cargo check -p perf-sentinel-core --no-default-features --all-targets` is warning-clean.
- **Helm chart still pinned at 0.2.31 / `appVersion` 0.5.28 at release time.** A chart bump pinning 0.6.0 follows separately.

## [0.5.28]

### BREAKING CHANGES

**Acknowledgment signature format changed from 16 to 32 hex characters.**

Previous format: `<finding_type>:<service>:<endpoint>:<16 hex>`
New format:      `<finding_type>:<service>:<endpoint>:<32 hex>`

Existing `.perf-sentinel-acknowledgments.toml` files with 16-hex signatures stop matching findings after upgrade. Findings previously acknowledged reappear in reports until re-acked under the new format. The TOML loader does not error on a mixed-legacy file, the entries become inert no-match strings.

The daemon JSONL ack store keeps replaying on startup, but every legacy 16-hex line is now skipped with a `WARN`-level event, and an end-of-replay summary log reports the dropped count. This avoids the silent state drift where a forgotten flush would have surfaced legacy entries as "active" acks matching no finding. Operators may also pre-empt the warning by flushing the store while the daemon is offline:

```bash
rm /var/lib/perf-sentinel/acks.jsonl
```

The default storage path is `<data_local_dir>/perf-sentinel/acks.jsonl` (XDG-respecting on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows). Operators with `[daemon.ack] storage_path` set explicitly should target that path instead.

This is a patch bump (0.5.27 to 0.5.28) despite the breaking change. The project is in early development with a small external user base, the SemVer signal cost was judged lower than the cadence cost of a minor bump. Future breaking changes may follow standard SemVer.

**SARIF fingerprint discontinuity.** GitHub Code Scanning and other SARIF consumers cache findings by `fingerprints["perfsentinel/v1"]`, which equals the signature. After upgrade every previously-seen finding has a new fingerprint, registering as net-new. The `perfsentinel/v1` key is preserved (no `v2` bump) for transport stability, the discontinuity is documented and recoverable by clearing the consumer's cache.

### Added

- **Embedded SPECpower entries for 2024-2026 cloud architectures** (131 new rows in `crates/sentinel-core/src/score/cloud_energy/table.rs`, growing the table from 187 to 318 entries). AWS m7i, c7i, r7i, m7a, c7a, m6a, c6a, m7g, c7g, m8g, c8g. GCP c3, c3d, c4, c4d, n2d, t2a. Azure Standard_Dv6, Standard_Dadsv6, Standard_Dpsv6 (Cobalt 100), Standard_Ev6. Bare metal `xeon-6780e` (Sierra Forest). Graviton 3 and 4 are estimated as a documented mix of an Ampere Altra floor and a Sapphire Rapids minus 25% upper bound, Cobalt 100 uses the midpoint blend of N1 and V1 (0.60 / 2.20 W per vCPU).
- **TUI `Enter` on Correlations panel jumps to Detail for `sample_trace_id`** in `perf-sentinel query inspect`. Three silent no-op cases are intentional: the row has no `sample_trace_id`, the trace is no longer in the active set, or no row is selected.
- **`enter_detail` helper** in the TUI router so the Correlations binding and the legacy Findings drill-down share one code path.
- **`docs/LIMITATIONS.md` sections on the two data vintages** ("Two data vintages, two methodologies", with explicit +/-40% uncertainty bounds), the Graviton/Cobalt 100 estimated bounds, the Genoa n=1 caveat, and the legacy memory-optimized gap. FR mirror updated.

### Changed

- Signature hash prefix bumped from 8 bytes (16 hex chars, ~64 bits) to 16 bytes (32 hex chars, ~128 bits).
- Daemon-side `validate_signature` accepts 32 hex chars exclusively, every shorter or non-conforming signature returns HTTP 400 with `InvalidSignature`. The boundary check now also requires at least one byte before the leading colon, closing the previously-accepted `:<32 hex>` empty-prefix shape.
- Daemon `replay_and_compact` skips legacy 16-hex entries with a per-line `WARN` and an end-of-replay summary, instead of silently inserting them into the active map.
- All test fixtures, the SARIF `SAMPLE_SIGNATURE`, and the operator-facing examples in `README`, `docs/CLI.md`, `docs/CONFIGURATION.md`, `docs/ACK-WORKFLOW.md`, `docs/ACKNOWLEDGMENTS.md`, `docs/QUERY-API.md`, `docs/RUNBOOK.md` and their FR mirrors updated to the 32-char format.
- **`SpanEvent` and `NormalizedEvent` repeated string fields are now `Arc<str>`**: `service`, `cloud_region`, `code_function`, `code_filepath`, `code_namespace`, `instrumentation_scopes`, `template`. The serde feature `rc` is activated, the JSON wire format is unchanged.
- **OTLP ingest hoists `service_name` and `cloud_region` `Arc<str>` to the `resource_spans` level**, each span Arc-clones the shared buffer. Jaeger ingest builds the `process_id` to `Arc<str>` map once per trace.
- **Calibration `ops_per_service` is now `HashMap<Arc<str>, u64>`** keyed via `Arc::clone(&event.service)`.
- **AWS `PROVIDER_DEFAULTS` stays on `m5.large` (2.0 / 20.0)** to preserve the waste signal across the methodology shift between legacy CCF and modern SPECpower entries. Bumping to m7i would silently drop reported energy ~3x because the legacy AWS entries are baseboard-inclusive while the new entries use per-vCPU coefficients.
- **Azure `PROVIDER_DEFAULTS` bumps to `Standard_D2s_v6` (1.1 / 6.4)**. v4 to v6 are methodologically homogeneous on Azure.
- Helm chart 0.2.30 to 0.2.31, `appVersion` 0.5.27 to 0.5.28. The `artifacthub.io/changes` annotation surfaces the breaking change on Artifact Hub.

### Performance

- **OTLP and Jaeger ingest collapse N per-span service allocations to one per Resource or trace** via `Arc::clone` of a hoisted `Arc<str>`. For 10K spans sharing one service in one Resource block, this is 1 allocation instead of 10K.
- **`collect_instrumentation_scopes` returns `Vec<Arc<str>>` directly** instead of building a `Vec<String>` and converting at the boundary, removing one intermediate Vec alloc per span with scopes.
- **Calibration loop saves one `String::clone` per event** by keying `ops_per_service` on `Arc<str>` and routing the join with `EnergyReading::service` through `HashMap::get(s.as_str())` (`Borrow<str>`).

### Internal

- **Two helpers extracted from `crates/sentinel-core/src/daemon/ack.rs`**: `tighten_parent_dir_perms` (Unix `0700` chmod, no-op on non-Unix) and `apply_replay_entry` (Ack/Unack match plus active-set cap), dropping cognitive complexity below the SonarCloud threshold (17 to ~8 on `AckStore::new`, 16 to ~7 on `replay_and_compact`).
- **`expectScreenshotWritten` helper** in `crates/sentinel-cli/tests/browser/demo/stills.spec.ts` asserts each captured PNG is at least 1 KiB, satisfying SonarCloud `typescript:S2699`.
- **Final cheatsheet visibility assertion** in `crates/sentinel-cli/tests/browser/demo/tour.spec.ts` so the tour test has at least one explicit `expect()` call.
- **Eleven tests lock the canonical signature input shape** (4 ack-store, 3 OTLP, 2 Jaeger, 2 Zipkin) so a future field rename or sanitization tweak that would change the digest input is caught at the test boundary.

### Notes

- **No public Rust surface change beyond `SpanEvent` and `NormalizedEvent` field types.** Crates depending on `perf-sentinel-core` 0.5.27 that read `event.service` need to switch from `&String` patterns to `&str` (for example via `event.service.as_ref()`). The serde feature `rc` is now activated unconditionally on `perf-sentinel-core`.

## [0.5.27]

Hardening pass on the CLI output paths and the daemon ack flow, plus a TUI refactor that eliminates the UI freeze during ack/revoke, alongside a batch of allocation-light rewrites on the analysis hot paths. No public surface change, no behavior change for already-clean inputs.

### Added

- **Mixed-content WARN at HTML render time** when `--daemon-url http://...` points at a non-loopback host. Catches the "report served over HTTPS but daemon URL is HTTP" case before the operator opens the report and discovers the Acks panel is silently broken. Loopback URLs (`localhost`, `127.0.0.1`, `[::1]`) are exempt because dev setups intentionally run the daemon on HTTP.
- **CORS wildcard + ack `api_key` WARN at daemon startup** when `[daemon.cors] allowed_origins = ["*"]` is combined with `[daemon.ack] api_key`. Wildcard CORS plus an `X-API-Key` auth lets any browser origin replay a captured key. Whitelist explicit origins for production deployments.
- **`--auth-header` ps-visibility WARN on `tempo` and `jaeger-query`**, mirroring the existing nudge on `pg-stat`. Operators are pointed at `--auth-header-env`.
- **1 KiB cap on the `ack create` stdin signature read** so a `cat /dev/urandom` pipe cannot exhaust memory before the daemon-side validator rejects it.
- **1 KiB cap on the interactive API-key prompt** for the same reason.

### Changed

- **CLI write paths use `O_NOFOLLOW` on Unix** for the HTML report, the calibration TOML, and the diff `--output` file. Mirrors the daemon ack store hardening so a hostile pre-planted symlink cannot redirect the write outside the operator's tree.
- **`validate_http_endpoint` (Tempo, Jaeger query) and `validate_prometheus_endpoint` (`pg-stat`) reject ASCII control characters** before reaching `hyper::Uri`, matching the discipline already in place on the daemon and ack URL validators.
- **CLI errors sanitize signatures, daemon URLs, and daemon-supplied bodies** through `text_safety::sanitize_for_terminal` consistently across every print site. A hostile env-var value or daemon response body can no longer repaint the operator's terminal at error time.
- **Daemon ack store parent directory tightened to `0700` on Unix** when the default storage path is created. Closes the "world-writable XDG_DATA_HOME" edge in shared-tenancy environments.
- **`rewrite_compacted` re-checks the ack file for symlinks immediately before the rename**, closing the long compaction window where a hostile local user could otherwise plant a symlink between startup and swap.
- **`debug_assert!` on the CSP placeholder safety net is now a plain `assert!`** so the safety net survives release builds.
- **`.gitleaks.toml` finding-signature regex tightened** to the actual `sanitize_endpoint` output charset (`[A-Za-z0-9_.-]`), shrinking the false-positive bypass surface.
- **Helm chart 0.2.29 to 0.2.30**, `appVersion` 0.5.26 to 0.5.27. The `artifacthub.io/images` annotation is updated in lockstep. No chart template change.

### Performance

- **Probe-before-allocate `strip_bidi_and_invisible`** (returns `Cow<'_, str>`), saving an allocation per finding signature and per SARIF acknowledgment field on clean inputs (the common case).
- **`compute_signature` builds via `String::with_capacity` + `push_str`** instead of a 12-arg `format!`, and `sanitize_endpoint` returns `Cow<'_, str>` so a clean endpoint pays zero copy.
- **Redundant detection indexes N+1 templates in a `HashSet` once** before the group loop, swapping `O(G * F)` per-trace for `O(G + F)`.
- **`endpoint_stats_to_per_endpoint_io_ops` sorts borrowed `(&str, &str)` pairs**, so the comparator no longer walks freshly allocated `String`s.
- **`top_offenders` and bench latency sort use `f64::total_cmp`** for stable ordering without the `partial_cmp().unwrap_or(Equal)` dance.
- **`parse_daemon_environment` and `SanitizerAwareMode::from_config` use `eq_ignore_ascii_case`**, dropping a `to_ascii_lowercase` allocation per call.

### Internal

- **`parse_scraper_auth_header` returns `ScraperAuthOutcome::{None, Some, Invalid}`** instead of `Result<Option<_>, ()>`, sidestepping the `clippy::option-option` and `clippy::result-unit-err` lints.
- **`MAX_SIGNATURE_LEN` is now `pub const` (`#[doc(hidden)]`)** so the CLI can read the daemon's per-signature byte cap without forking the constant.
- **`PROMETHEUS_SCRAPE_FLOOR`, `resolve_auth_header_or_exit`, `AckSubmitError`, `post_ack_via_daemon`, `delete_ack_via_daemon`, `decode_body_message` and `FINDINGS_FETCH_LIMIT` are correctly feature-gated**, eliminating dead-code warnings on `--no-default-features --features daemon` and similar matrices.
- **TUI ack/revoke is non-blocking** (`crates/sentinel-cli/src/tui.rs`). `submit_ack_modal` snapshots the modal state into an owned `AckSubmitPayload`, then `Handle::current().spawn(execute_ack_submit(...))` returns immediately. The sync `run_loop` drains a `tokio::sync::mpsc::UnboundedReceiver<AckOutcome>` before each redraw and uses `event::poll(50ms)` while a write is in flight, falling back to blocking `event::read()` at idle so the power profile matches the pre-refactor baseline. Edge cases handled: a held Enter or double tap on Submit is gated by the `submitting: bool` flag (no duplicate spawn), an `AckOutcome::Failure` arriving after Esc-while-submitting logs at WARN before being dropped (a misconfigured `[daemon.ack] api_key` cannot stay hidden), and `AckOutcome::Success` carries `Option<HashMap<String, AckSource>>` so a refetch failure (`None`) keeps the previous snapshot while a legitimate empty refetch (`Some(empty)`) clears it. `AckSubmitPayload` ships with a hand-written `Debug` that redacts the API key.

## [0.5.26]

Soft deprecation of the eight legacy top-level configuration keys. The flat form keeps working byte-for-byte, but a `WARN`-level event now fires at config load when the section override is absent, pointing operators to the recommended sectioned form. The event carries structured fields so log shippers can count occurrences per flat without regex.

### Added

- **`resolve_with_deprecation_warning` helper** in `crates/sentinel-core/src/config.rs` (`#[must_use]`, generic over `T` without `Copy`). Emits a `tracing::warn!` event with structured fields `legacy_key` and `replacement` exactly when the legacy flat form is set without its section override. The eight resolution sites in `impl From<RawConfig> for Config` now route through it, the priority stays `section > flat > default`.
- **19 new tests** in `crates/sentinel-core/src/config.rs::tests`: 16 priority tests (resolves_to_value plus yields_to_section per flat), 1 silence-when-unset assertion locking the no-noise default-only path, 2 capture tests via `tracing-test 0.2.6` (new dev-dependency) asserting the warning fires with the expected structured fields and stays silent when the section overrides the flat.

### Deprecated

- **Top-level (flat) configuration keys**: `n_plus_one_threshold`, `window_duration_ms`, `listen_addr`, `listen_port`, `max_active_traces`, `trace_ttl_ms`, `max_events_per_trace`, `max_payload_size`. These keys still work and resolve as before, but now emit a `WARN`-level deprecation message at config load pointing to the section-based equivalent under `[detection]` or `[daemon]`. The event carries structured fields `legacy_key` and `replacement` so log shippers can count occurrences per flat without regexing the message. They will be removed in a future release. Migration table and examples in `docs/CONFIGURATION.md` (and FR mirror). When both the flat and the sectioned form are set for the same setting, the sectioned form takes precedence and no warning is emitted, behavior preserved bit-for-bit.

### Changed

- **Helm chart `0.2.28` → `0.2.29`**, `appVersion` `0.5.25` → `0.5.26`, default daemon image tag points at `ghcr.io/robintra/perf-sentinel:0.5.26`. The `artifacthub.io/images` annotation is updated in lockstep. No chart-level template change.
- **`cli_watch_*` e2e tests** now use override port pairs in the +20000 zone (24318/24317 and 24320/24319) to avoid colliding with daemon defaults (4318/4317) and the local +10000 dogfooding zone (14318/14317). The SIGTERM test asserts daemon liveness via `try_wait` before kill, so a silent bind failure no longer passes the test by accident.

## [0.5.25]

Adds a Prometheus counter pair to observe Scaphandre scrape outcomes from the daemon-side scraper. Mirrors the ack-counters pattern shipped in 0.5.21 (one `_total{status}` and one `_failed_total{reason}`), pre-warmed at startup so dashboards build with `rate()` queries without `absent()` guards. Lab scenarios that run the Scaphandre mock can later move from log-grep verdicts to deterministic counter-delta assertions.

### Added

- **`perf_sentinel_scaphandre_scrape_total{status}` counter**: total Scaphandre scrape attempts on the daemon scraper, partitioned by outcome. `status` label values: `success`, `failed`. Pre-warmed to 0 at daemon startup.
- **`perf_sentinel_scaphandre_scrape_failed_total{reason}` counter**: total failed Scaphandre scrapes, partitioned by failure reason. `reason` label values: `unreachable` (transport error, connection refused, DNS failure, TLS handshake), `timeout` (3-second deadline elapsed), `http_error` (non-2xx status), `body_read_error` (transport error during body read), `request_error` (request build failure), `invalid_utf8` (response body not UTF-8). Pre-warmed to 0 for all 6 reasons at daemon startup. Configuration parsing failures (invalid endpoint URI) are not counted: the scraper task aborts at startup before the counter is touched.
- **New `ScaphandreScrapeReason` enum** in `crate::report::metrics`, exposed for callers that need to map a `ScraperError` to the canonical Prometheus label string. Mirrors the existing `OtlpRejectReason` and `AckFailureReason` pattern.
- **New `scraper_error_reason` helper** in `crate::score::scaphandre::scraper`, kept module-private for now. Lift to `http_client.rs` when a second scraper (Tempo, Electricity Maps) grows the same instrumentation.
- **`docs/METRICS.md`** (and FR mirror) section documenting the new counters with sample PromQL queries.
- **`perf-sentinel completions <shell>`** subcommand emits a completion script for `bash`, `zsh`, `fish`, `powershell`, or `elvish` to stdout, following the cargo, gh, and rustup pattern. No release artifact, no installer wiring. Locked by `completions_subcommand_accepts_known_shells` and `completions_subcommand_rejects_unknown_shell`. Documented in `docs/CLI.md` and the FR mirror.
- **Signature stability lock**: 4 new tests in `crates/sentinel-core/src/acknowledgments.rs::tests` covering the cross-restart invariant (`signature_stable_across_trace_id_changes`) and three diff sentinels on `endpoint`, `service`, and `finding_type`. The signature format `<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>` is now a public stability contract for `.perf-sentinel-acknowledgments.toml` files already deployed.
- **OTLP route precedence lock**: 3 new tests in `crates/sentinel-core/src/ingest/otlp.rs::tests` covering `http.route > http.url > url.full` from the parent HTTP span. Critical for ack signature stability when the producer emits both attributes (the route template wins).
- **Jaeger and Zipkin route precedence lock**: 2 tests each covering `http.route > http.target` from the current span tags. Same canonical priority across all three ingest paths.
- **`scripts/check-tag-version.sh` intra-workspace pin validation**: a second loop scans every `crates/*/Cargo.toml` for sibling-crate dependency pins and aborts the release on mismatch with `[workspace.package].version`. Strict-eq pins (`version = "=X.Y.Z"`) are stripped before comparison.
- **`scripts/hooks/pre-commit` and `scripts/install-hooks.sh`**: zero-dep gitleaks pre-commit. The hook uses `gitleaks git --staged` and skips gracefully if gitleaks is absent or older than 8.16. The installer is idempotent (`ln -sf` symlink in `.git/hooks/`) and detects a non-default `core.hooksPath` configured globally with two clear remediation paths.
- **CONTRIBUTING.md sections** for "Git hooks" (one-line `bash scripts/install-hooks.sh` after clone, `gitleaks 8.16+` requirement, `--no-verify` bypass) and an extended "Release process" listing intra-workspace dependency pins as a bump target.
- **`docs/ACK-WORKFLOW.md` "Signature stability and service restarts"** plus FR mirror: documents the four signature components, the critical dependency on `http.route`, the `http.url` and `url.full` fallbacks, the curl recipe to verify producer-side instrumentation, and a "Carbon scoring scope" subsection clarifying that the cumulative counters reflect distinct request executions (use `rate()` for trend dashboards).

### Changed

- **`MetricsState::new`** registers two new counters and pre-warms 8 series total (2 status + 6 reasons). Zero behavior change for users who don't configure `[green.scaphandre]`.
- **`run_scraper_loop`** in `crate::score::scaphandre::scraper` now increments the new counters on every tick, success and failure alike. The existing log machinery (warn-once, unsupported-platform escalation after 3 consecutive failures) is unchanged.
- **Helm chart `0.2.27` → `0.2.28`**, `appVersion` `0.5.24` → `0.5.25`, default daemon image tag points at `ghcr.io/robintra/perf-sentinel:0.5.25`. No chart-level template change.
- **`docs/INTEGRATION.md` slimmed** by ~170 lines, redirecting operators to the focused `docs/INSTRUMENTATION.md` (producer-side) and `docs/CI.md` (consumer-side) entry points. The carbon-scope text in `docs/ACK-WORKFLOW.md` is rewritten to align with the cumulative Prometheus counter semantics rather than the earlier "double-count cross-restart" framing.
- **Operator guides condensation**: verbose sections in `docs/INTEGRATION.md`, `docs/LIMITATIONS.md`, `docs/CI.md`, the design notes under `docs/design/`, and code comments across the workspace are condensed in favor of the .md sources. No behavior change.
- **`clap_complete = "4.6"` to `"4.6.3"`** explicit pin in `crates/sentinel-cli/Cargo.toml` to surface the resolved version in the manifest itself rather than only in `Cargo.lock`.

## [0.5.24]

Closes the three-axis UX marathon over the daemon ack API. After the CLI helper (0.5.22) and the HTML dual-mode (0.5.23), the TUI ratatui interactive (`perf-sentinel query inspect`) now supports ack and revoke actions directly from the terminal. Tech leads auditing findings can act without leaving the session.

### Added

- **TUI ack/revoke**: two new keybindings in `perf-sentinel query inspect`. `a` opens an acknowledgment modal (reason, expires, by) on the selected finding and posts to `/api/findings/<sig>/ack`. `u` opens a revoke confirmation modal and submits a `DELETE`. Modal navigation: Tab / BackTab cycle fields, Enter on a text field advances, Enter on Submit posts, Esc cancels. The modal renders centered with focus highlight cyan and an error footer in red on 4xx/5xx.
- **`[acked by <user>]` indicator**: italic gray badge appended to acknowledged findings in the Findings panel. Sourced from `FindingResponse.acknowledged_by` (TOML or daemon). Refreshed after every successful submit.
- **`--api-key-file` on `query inspect`**: new flag mirrors the `perf-sentinel ack` helper. Auth resolution priority: `PERF_SENTINEL_DAEMON_API_KEY` env var first, file fallback. No interactive password prompt in the TUI (raw mode + alternate screen are incompatible with rpassword TTY input). On 401 without a key, the modal shows an actionable error message.
- **`include_acked=true`** on the boot fetch: `query inspect` now fetches `/api/findings?limit=10000&include_acked=true` so the TUI sees acknowledged findings and can render the indicator. Previously only active findings were displayed.
- **Sync-async bridge**: `tokio::task::block_in_place` wraps `crate::tui::run` in `query.rs::run_inspect_action`, allowing the synchronous `run_loop` to call the async daemon HTTP helpers via `Handle::current().block_on(...)` without runtime panic.
- **`AckSubmitError` enum**: new pub(crate) error type in `crate::ack` mapping HTTP status codes to actionable variants (`Unauthorized`, `Conflict`, `NotFound`, `Disabled`, `StoreFull`, `Validation`, `Http`, `Transport`). `Display` impl never includes the API key, defensive against accidental leak. The modal renders `StoreFull` with a hint to revoke expired acks or raise the daemon limits, and surfaces daemon-supplied 400 bodies via `Validation` so the operator sees the actionable message rather than a generic `HTTP 400 ...` line.
- **`post_ack_via_daemon` / `delete_ack_via_daemon` helpers**: thin pub(crate) wrappers in `crate::ack` consumed by `submit_ack_modal`. Reuse the shared `http_call` helper for client construction.
- **Lifted helpers to `pub(crate)`**: `parse_expires`, `read_api_key_file`, `resolve_api_key`, `http_call` in `crate::ack`. Previously private to `cmd_ack`. The TUI submit path consumes them without duplication.
- **`Deserialize` on `FindingResponse` and `AckSource`** (in `sentinel-core::daemon::query_api`): the wire types are now round-trippable so the CLI can decode the daemon's per-finding ack annotation. `acknowledged_by` becomes `#[serde(default)]` on the deserialize side so older daemon responses without the field still parse.
- **`percent_encode_signature_segment` helper**: the signature interpolated into `/api/findings/{sig}/ack` URLs is percent-encoded first, with a `Cow<'_, str>` return and a zero-allocation common path (real signatures match `[A-Za-z0-9_:.-]+` and pass through unchanged). Defense in depth against a daemon synthesizing exotic signatures in `FindingResponse`, the daemon already validates the shape server-side.
- **`FINDINGS_FETCH_LIMIT` re-exports the daemon's `MAX_FINDINGS_LIMIT`** (1000) so the boot fetch and the post-submit refetch cannot drift from the server-side cap. Both `MAX_FINDINGS_LIMIT` and `MAX_ACKS_RESPONSE` are now `pub` with `#[doc(hidden)]` on the daemon side: visible to the workspace CLI consumer, hidden from any future published API surface.
- **Bidi and control-character filter** on modal input via `is_modal_input_char_acceptable`: rejects `c.is_control()` and the bidi block (`U+202A..U+202E`, `U+2066..U+2069`). Defense in depth against bracketed paste of attacker-crafted content. The daemon already strips bidi server-side on the persistence path.
- **Panic hook** installed at TUI boot via `std::sync::Once`. Restores raw mode and the main screen before the standard hook prints the panic message, chained to the previous hook so the message is not lost. Idempotent across re-entry, atomic across concurrent calls.
- **New documentation page** `docs/INSPECT.md` covering the TUI keybindings, the ack modal flow, the auth resolution, and the sync HTTP caveat. Updated `docs/ACK-WORKFLOW.md` with a fourth row in the decision table (TUI ack action). French mirrors at `docs/FR/INSPECT-FR.md` and `docs/FR/ACK-WORKFLOW-FR.md`.

### Changed

- **Helm chart `0.2.26` → `0.2.27`**, `appVersion` `0.5.23` → `0.5.24`, default daemon image tag points at `ghcr.io/robintra/perf-sentinel:0.5.24`. The `artifacthub.io/images` annotation is updated in lockstep. No chart-level template change.
- **`query inspect` boot fetch** changed from `/api/findings?limit=10000` to `/api/findings?include_acked=true&limit=1000`. The daemon already capped at 1000 server-side, so this is honesty-only on the wire, the only behavioral change is the `acknowledged_by` field flowing through.
- **HTML report live mode polish** picked up during the 0.5.24 smoke pass on top of the 0.5.23 surface: modal centering uses `inset: 0` plus `margin: auto` rather than absolute positioning (was misaligned on tall viewports), the `Show acknowledged` toggle is initialized at boot rather than only on user change, the auth modal surfaces an `Invalid API key` message on the second 401 when a stale key was already cached and clears it via `sessionStorage.removeItem`, and the `Forget key` button is hidden when no key is cached (was always visible under `body.ps-live`).

### Notes

- `a` / `u` keys are no-op in batch mode (`inspect --input`). Acknowledgment requires a running daemon to persist.
- HTTP requests in the modal are synchronous and freeze the UI for their duration (typically 100-300ms on localhost). Acceptable for a scope-minimal release. An async event loop refactor is a candidate followup if user feedback signals friction.
- `[daemon.ack] enabled = true` is the default since 0.5.20. The TUI write surface is gated behind it, the daemon answers 503 and the modal shows "daemon ack store is disabled" if the operator turned it off.
- The marathon UX is now complete: CLI helper (0.5.22), HTML dual-mode (0.5.23), TUI interactive (0.5.24). All three surfaces consume the same daemon ack API (0.5.20 + 0.5.21) with no daemon-side change across the marathon.

## [0.5.23]

Ships the second of three UX surfaces above the daemon ack API (after the CLI helper in 0.5.22): the HTML report can now operate in a live mode that connects to a running daemon for interactive ack/revoke workflows. The static mode remains preserved byte-equivalent.

### Added

- **HTML report live mode**: new `--daemon-url <URL>` flag on `perf-sentinel report`. When set, the generated HTML connects to the daemon at runtime: per-finding Ack/Revoke buttons, an Acknowledgments panel listing daemon-side acks, a `Show acknowledged` filter toggle, a connection status indicator, and a manual refresh button. Without the flag, the report stays static (current behavior preserved).
- **Daemon CORS support** (opt-in): new `[daemon.cors] allowed_origins` config section. Default `[]` means no CORS headers (current loopback-only behavior preserved). `["*"]` is wildcard (development). Non-wildcard list whitelists exact origins. Allowed methods: GET, POST, DELETE, OPTIONS. Allowed headers: Content-Type, X-API-Key, X-User-Id. Mixed wildcard+explicit lists are rejected at config validation.
- **Dynamic Content-Security-Policy**: live mode adds `connect-src <daemon_url>` to the meta tag. Static mode keeps the strict `default-src 'none'` policy byte-equivalent.
- **HTML auth flow**: when the daemon has `[daemon.ack] api_key` configured, the report prompts for the key on the first 401 (write call) and stores it in `sessionStorage` under `perf-sentinel.daemon.api-key` (purged at tab close). The `/api/status` healthcheck stays unauthenticated so the connection badge can confirm reachability without a key.
- **AbortController fetch timeout**: every live-mode browser fetch is bounded at 10 seconds so a hung daemon does not leave requests pending until the browser's default network timeout.
- **`fetch_with_body` URL validator reuse**: the existing `validate_url` from the `perf-sentinel ack` subcommand is now `pub(crate)` and shared with `perf-sentinel report --daemon-url` and `perf-sentinel query`, fixing a long-standing drift where `query --daemon` accepted userinfo / path / query strings while `ack --daemon` rejected them.
- **New documentation pages**: `docs/HTML-REPORT.md` covers the live mode end-to-end (CORS prerequisites, auth flow, security caveats, smoke test). `docs/CONFIGURATION.md` gains a `[daemon.cors]` section. `docs/ACK-WORKFLOW.md` gets a third option in the decision table. French mirrors at `docs/FR/HTML-REPORT-FR.md`, `docs/FR/CONFIGURATION-FR.md`, `docs/FR/ACK-WORKFLOW-FR.md`.
- **`Forget key` button** revealed when an X-API-Key sits in the live state. Clicking purges sessionStorage and re-pings the daemon. Hidden when no key is cached so the static mode stays clean.
- **CORS layer scoped to `/api/*`**: built into the query API sub-router before merging into the outer router, so OTLP `/v1/traces`, `/metrics` and `/health` are never reachable cross-origin even under wildcard mode. `Access-Control-Max-Age` is 120 seconds, no `Access-Control-Allow-Credentials`. The `cors_layer_does_not_leak_to_otlp_or_metrics_or_health_routes` test mirrors the real router topology so a future axum upgrade that flipped the merge order breaks the build instead of regressing security.
- **Cross-section config validation**: `daemon_api_enabled = false` combined with a non-empty `cors_allowed_origins` is rejected at config load with an actionable error pointing at both knobs. Catches the silent "why isn't ack working post-deploy" trap.
- **Compile-time invariant on `STATIC_CSP`** via a `const _: () = { ... }` block, enforcing that the static prefix never contains a `{{` byte sequence that could shadow placeholder substitution. The runtime `debug_assert!` in `inject` covers the daemon-URL half.
- **44 new tests** across the workspace: 11 CORS layer tests in `daemon/listeners.rs::cors_tests`, an end-to-end CORS scoping integration test that mirrors the real router topology, 8 CSP and payload tests in `report/html.rs`, an IPv6 literal test, 8 config validation tests for the new `[daemon.cors]` section, plus the cross-section consistency check.

### Changed

- **Helm chart `0.2.25` → `0.2.26`**, `appVersion` `0.5.22` → `0.5.23`, default daemon image tag points at `ghcr.io/robintra/perf-sentinel:0.5.23`. The `artifacthub.io/images` annotation is updated in lockstep.
- **Substitution order in `inject`**: JSON is substituted first, then CSP, then title, so a hostile `input_label` containing `{{REPORT_JSON}}` cannot shadow the static placeholder. Locked in by `hostile_input_label_with_json_placeholder_does_not_double_substitute` and friends.
- **`tower-http` features** extended with `"cors"` (was `["decompression-gzip", "limit"]`). No new top-level dependency, just an additional feature on a crate already in the daemon dep tree.

### Notes

- The static HTML mode is preserved byte-equivalent. CI artifacts generated without `--daemon-url` continue to behave exactly as in 0.5.22.
- The X-API-Key is held in `sessionStorage` only for the tab session, never in `localStorage`. Acceptable for personal devops use, not recommended on shared browser profiles.
- Live mode requires CORS configured on the daemon. HTML opened in a browser cannot connect to a daemon that does not echo `Access-Control-Allow-Origin` for the document origin.
- The `--daemon-url` flag is gated behind the `daemon` feature, in line with the rest of the daemon-facing CLI surface.
- The CSP keeps `script-src 'unsafe-inline'` (the report inlines its JS in a single self-contained file). `connect-src` whitelists ONLY the operator-passed daemon URL, never `*`.
- Stale-key recovery on rotation: a 401 from `/api/status` purges the cached key and sets the badge to `Authentication required`. The next write call opens the auth modal with a clean slate.
- CORS preflight DoS surface documented: `OPTIONS` preflight short-circuits before the X-API-Key check, so any whitelisted origin (or any origin under wildcard mode) can spam preflights past the auth boundary. Mitigation posture for 0.5.23 is a reverse proxy with per-IP rate limiting when exposing the daemon cross-origin, a native `tower-governor` integration is tracked for a future release.

## [0.5.22]

Adds a CLI helper for the daemon ack API introduced in 0.5.20. Operators can now acknowledge findings from the terminal without typing curl commands. No daemon-side change, the CLI consumes the existing 0.5.20 / 0.5.21 surfaces unchanged.

### Added

- New CLI subcommand `perf-sentinel ack` with three subactions:
  - `ack create --signature <SIG> --reason <TEXT> [--expires 7d]` to create a daemon ack via `POST /api/findings/{sig}/ack`.
  - `ack revoke --signature <SIG>` to remove one via `DELETE /api/findings/{sig}/ack`.
  - `ack list [--output text|json]` to enumerate active daemon acks via `GET /api/acks`. The text format renders a colored aligned table with a footer that mentions the daemon's 1000-entry cap and points users at `.perf-sentinel-acknowledgments.toml` for the CI TOML acks (which the CLI deliberately does not touch).
- Daemon URL resolution via `--daemon` flag, `PERF_SENTINEL_DAEMON_URL` env, or default `http://localhost:4318`.
- Auth resolution via `PERF_SENTINEL_DAEMON_API_KEY` env, `--api-key-file <path>` (trailing newline stripped), or interactive `rpassword` prompt on 401 when stdin is a TTY. No `--api-key` flag, by design (process-list and shell-history exposure).
- Duration parsing on `--expires`: ISO8601 datetimes (`2026-05-11T00:00:00Z`) or relative durations (`7d`, `24h`, `30m`).
- Conventional Unix exit codes: 0 success, 1 generic, 2 client (4xx), 3 server (5xx). Errors on stderr with actionable hints (e.g. on 409 the hint points at `ack revoke`, on 401 it points at the env var).
- New `fetch_with_body` helper in `sentinel-core::http_client` for POST/DELETE requests with `X-API-Key`, timeout, body cap. Returns `(StatusCode, Bytes)` so callers can discriminate non-2xx without `?` short-circuit.
- New documentation pages `docs/CLI.md` and `docs/ACK-WORKFLOW.md` (with French mirrors at `docs/FR/CLI-FR.md` and `docs/FR/ACK-WORKFLOW-FR.md`). The ACK-WORKFLOW page covers the CI TOML, daemon API, and CLI helper in one place with a "choose the right mechanism" decision table.
- **`call_with_tty_retry` helper** in the new ack module: builds the client once per CLI invocation and reuses it across the 401-prompt-retry path, so no second TLS init is paid.
- **Hardened `--daemon` URL validator** that rejects empty authority, port without host, userinfo, path components, and query strings before the first request goes out. Each rejection has its own actionable error message.
- **`O_NOFOLLOW` on `--api-key-file`** (Unix only): the CLI refuses to follow symlinks at the path target, mirroring the daemon's posture for the JSONL store. When the file mode is group or world readable (`mode & 0o077 != 0`), the CLI emits a one-line stderr warning suggesting `chmod 600`, gated behind `stderr.is_terminal()` so CI pipelines do not see it on every invocation. Embedded ASCII control characters in the file are rejected at read time with a message naming the file.
- **`MAX_ACKS_RESPONSE` exposed `pub`** in `crates/sentinel-core/src/daemon/query_api.rs` and re-imported in the CLI under the same name, so the `ack list` footer ("showing up to 1000") cannot drift from the daemon-side cap.
- **18 integration tests** in `crates/sentinel-cli/tests/cli_ack.rs` (spawning a hand-rolled HTTP/1.1 mock server on `127.0.0.1:0`), **30+ unit tests** in `crates/sentinel-cli/src/ack.rs` for the URL validator, API-key file reader, expires parser, table renderer, and exit-code mapping, plus **5 unit tests** for `fetch_with_body` in `crates/sentinel-core/src/http_client.rs`.

### Changed

- **`build_client` and `build_client_with_body` share a private generic constructor** (`build_client_inner<B>()`) in `crates/sentinel-core/src/http_client.rs`. Same TLS configuration on both, no risk of rustls feature drift between the GET and POST/DELETE paths.

### Notes

- No daemon-side change. The HTTP shapes, status codes and JSON bodies are unchanged from 0.5.20 / 0.5.21. The Prometheus counters added in 0.5.21 keep tracking operator activity, including activity initiated from the new CLI.
- `ack list` shows only daemon acks. TOML CI acks remain visible in the file itself; the CLI deliberately does not read or merge the TOML side because the CLI is often run far from the application repo (PagerDuty laptop) where the file path is unknown.
- Helm chart `chart-v0.2.25` ships alongside this release (lockstep, `appVersion` bumped, no chart-level config change).
- The `X-API-Key` value is flagged `sensitive` on the wire. hyper redacts sensitive header values from `Debug` output and HPACK tables, mirroring the `AuthHeader::set_sensitive(true)` pattern used by `fetch_get` for `Authorization` headers.

## [0.5.21]

Adds Prometheus instrumentation for the runtime ack endpoints introduced in 0.5.20. No behavior change on the HTTP layer, status codes and JSON shapes are unchanged. Two new counters on `/metrics` give operators trend lines on operator-driven activity and a structured signal for failure modes (auth misconfiguration, store saturation).

### Added

- `perf_sentinel_ack_operations_total{action}` counter (`action="ack"|"unack"`) for successful ack and unack operations on the daemon HTTP API. Cached children for the hot path, pre-warmed at zero.
- `perf_sentinel_ack_operations_failed_total{action,reason}` counter for failures, with `reason` covering the 9 documented reachable values: `already_acked`, `not_acked`, `unauthorized`, `no_store`, `invalid_signature`, `limit_reached`, `file_too_large`, `entry_too_large`, `internal_error`. `file_too_large` flags per-daemon saturation (compaction needed), `entry_too_large` flags per-request misuse (oversized `by` / `reason` payloads); they used to collapse on the same series, they are now separated so operators can dashboard them independently. Pre-warmed for the 13 reachable `(action, reason)` combinations (8 on `ack`, 5 on `unack`) so dashboards can use `rate()` without `absent()` guards. Impossible combinations (e.g. `action="ack",reason="not_acked"`) are intentionally not pre-warmed.
- `AckAction::as_str` and `AckFailureReason` helper types in `crates/sentinel-core/src/report/metrics.rs` mirror the existing `OtlpRejectReason` pattern.
- **`#[inline]` on counter-bumping methods**: `record_ack_success`, `record_ack_failure`, and the 0.5.19 `record_otlp_reject` for consistency, matching the project inlining policy on critical helpers. Counter-bumping stays branchless on the success path (cached `IntCounter` children, single relaxed atomic add per call).
- **`register_int_counter_vec` helper** in `crates/sentinel-core/src/report/metrics.rs` factors the create-clone-register pattern across the three `IntCounterVec` registration sites (`otlp_rejected_total`, `ack_operations_total`, `ack_operations_failed_total`).
- **`check_ack_preconditions` helper** in `crates/sentinel-core/src/daemon/query_api.rs` factors the auth-then-store-presence guard shared by `handle_ack` and `handle_unack`. Records the matching `AckFailureReason` (`Unauthorized` or `NoStore`) before returning, so every error exit stays observable in `/metrics`. The constant-time `X-API-Key` comparison via `subtle::ConstantTimeEq` is untouched.
- **Six unit tests** in `crates/sentinel-core/src/report/metrics.rs`: `as_str` round-trip across all variants, success-path increments per action, failure-path increments per `(action, reason)`, pre-warmed-zero contract on both counters, impossible-combinations-not-pre-warmed contract, and the `/metrics` rendered-output contract.
- **Three integration tests** in `crates/sentinel-core/src/daemon/query_api.rs`: a no-store failure increments `reason="no_store"`, a TOML conflict bumps the same `reason="already_acked"` series as a daemon-side double-ack, and a malformed signature increments `reason="invalid_signature"`. The four pre-existing ack tests gain counter assertions on the success and `unauthorized` paths.
- **`docs/METRICS.md`** and FR mirror: new "Ack metrics (since 0.5.21)" section with the label table, the per-`reason` HTTP-status mapping, the pre-warming contract, three sample PromQL queries, and a paragraph on the auth-presence inference signal.
- **`docs/HELM-DEPLOYMENT.md`** and FR mirror: new "Daemon ack runtime store" subsection covering the four operator decisions when running the ack store under Kubernetes (`api_key` when bound non-loopback, persistence path remap to a PVC, `securityContext` mode-floor caveat, TOML ConfigMap mount), plus a ServiceMonitor warning on the 0.5.20 default-filter behavior of `/api/findings`.

### Changed

- **`AckError::FileTooLarge` and `AckError::EntryTooLarge` produce distinct HTTP error messages** (`"ack file size cap reached"` vs `"ack entry size cap reached"`). The HTTP status (`507 Insufficient Storage`) stays the same on both, clients matching on the status code are unaffected.
- **Binary size target relaxed** from `< 10 MB` to `< 15 MB` in `docs/LIMITATIONS.md`, `docs/design/02-NORMALIZATION.md`, and the FR mirrors. The musl statically-linked binary with mimalloc sits at `10.1 MB`, the previous target was tight enough that small additions would have pushed it over. `lto = "thin"`, `strip = true`, and `panic = "abort"` remain unchanged.

### Notes

- TOML-source conflicts (`POST` on a TOML-acked signature) and `AckError::AlreadyAcked` (daemon-side double-ack) collapse on the same series `reason="already_acked"`. The operator-visible behavior is identical (HTTP 409), so a single counter is enough.
- The unack path can occasionally fail with `FileTooLarge` or `EntryTooLarge` when the unack record itself would push the JSONL above the 64 MiB cap or exceeds the 4 KiB per-entry cap. The 0.5.21 implementation surfaces these under `reason="internal_error"` (HTTP 500) on the unack side, matching the pre-existing handler behavior. The ack side gets the new dedicated reasons.
- Helm chart `chart-v0.2.24` ships alongside this release (lockstep, `appVersion` bumped, no chart-level config change).

## [0.5.20]

Closes the runtime side of the ack workflow opened in 0.5.17. The CI TOML acks (versioned, PR-reviewed) are now complemented by a daemon HTTP API (POST/DELETE/GET) backed by a JSONL append-only file, so an SRE on call can mute a finding without a PR cycle on the application repo. The two sources are unioned at query time with TOML winning on conflict (immutable baseline shipped via PR review).

### Added

- **Three new daemon endpoints** on the existing HTTP query API: `POST /api/findings/{signature}/ack` to acknowledge a finding at runtime, `DELETE /api/findings/{signature}/ack` to revoke, and `GET /api/acks` to list the active runtime acks. Signature uses the same canonical format as the CI TOML workflow (`<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix>`), so an operator can copy a value from `/api/findings` straight into a curl call.
- **JSONL append-only persistence** at `~/.local/share/perf-sentinel/acks.jsonl` (configurable via `[daemon.ack] storage_path`). Each ack and each unack is a single line, tail-able for live audit. The file is replayed and atomically rewritten via tmp + rename at every daemon restart, so an ack/unack churn loop cannot accumulate forever. Hard caps: 64 MiB file size, 4 KiB per entry, 10 000 active acks. Created with mode `0600` on Unix.
- **Optional API key authentication** via `[daemon.ack] api_key`. When set, `POST` and `DELETE` require an `X-API-Key` header (constant-time compared via `subtle`). `GET /api/acks` and `GET /api/findings` stay unauthenticated by design (loopback reads). Empty string is rejected at config load.
- **Audit `by` field** resolved with priority order: `X-User-Id` header, then JSON body `by`, then `"anonymous"` fallback. BiDi and invisible characters are stripped (CVE-2021-42574 defense in depth).
- **`?include_acked=true` query parameter** on `GET /api/findings`. When passed, returns the full set with each acked entry annotated by an `acknowledged_by: { source, ... }` block. The `source` discriminant is `"toml"` for CI baseline acks and `"daemon"` for runtime acks, letting clients render both sources distinctly.
- **TOML and JSONL interop**: the daemon reads `.perf-sentinel-acknowledgments.toml` (path configurable via `[daemon.ack] toml_path`) at startup as an immutable baseline and unions it with the JSONL store. TOML wins on conflict so an SRE cannot accidentally override a team-agreed permanent ack from the runtime side.
- **Doc clarification**: `Report.warning_details` kinds are now documented as transient (`cold_start`, auto-clears on first batch) vs sticky (`ingestion_drops`, cumulative until daemon restart). Tables in `docs/METRICS.md` and `docs/RUNBOOK.md` plus FR mirrors. Closes the third B3 lab followup investigated 2026-05-04.
- **20+ new tests**: 16 unit tests in `crates/sentinel-core/src/daemon/ack.rs` covering replay, compaction, expiry, BiDi stripping, signature validation, file-size caps, parse errors with line numbers, and concurrent-write integrity. 7 integration tests in `daemon/query_api.rs` covering POST/DELETE/GET, the `?include_acked` flag, TOML-wins conflict resolution, and the X-API-Key auth path. 4 config tests covering the new `[daemon.ack]` section.
- **`ResolvedTomlAck` wrapper** in `daemon::query_api` pre-parses the TOML `expires_at: Option<String>` into `Option<DateTime<Utc>>` once at startup. The hot-path `lookup_ack` does a single datetime comparison per finding instead of a `chrono::NaiveDate::parse_from_str` per match.
- **`AckStore::snapshot_active`** returns `Arc<HashMap<String, AckEntry>>` (a single atomic refcount inc, no data copy). The active map sits behind `RwLock<Arc<HashMap>>`, so concurrent `GET /api/findings` polls take a cheap clone of the `Arc` and never block writers. Writers pay an O(N) HashMap clone outside the write lock to keep readers wait-free.
- **Hardened file open** on Unix: `O_NOFOLLOW` plus a `tokio::fs::symlink_metadata` pre-check (`SymlinkRefused` typed error rather than the kernel's `ELOOP`), and a post-open `permissions().mode() & 0o077` check that rejects pre-existing weak-mode files. The startup compaction unconditionally rewrites the file with mode `0o600`, eliminating any weak-permission window an attacker could plant before daemon launch.
- **`409 Conflict` on TOML-covered signatures**: a `POST` on a signature already covered by an active TOML ack returns `409 Conflict` with a message pointing the operator at the TOML file, rather than silently writing a JSONL line that would be shadowed on read.
- **`[daemon.ack] api_key` length floor**: keys shorter than 12 characters are rejected at config load. The daemon does not rate-limit `POST /ack`, so a co-resident attacker on the loopback interface could brute-force shorter keys in a tractable window. 12 characters of `[a-z0-9-]` puts the brute-force horizon past 10^17 attempts.
- **`.gitleaks.toml` allowlist** for the documented finding-signature pattern, anchored on the actual `FindingType` discriminants. Matches against the full source line via `regexTarget = "line"`, never on the bare 16-hex tail in isolation, so a real high-entropy secret nearby is still detected.

### Changed

- **`GET /api/findings` default behavior**: now filters out acked findings (CI TOML + daemon JSONL union). Pass `?include_acked=true` to restore the pre-0.5.20 behavior. Aligned with the CLI 0.5.17 `--acknowledgments` default for consistency.
- **`crates/sentinel-core/src/acknowledgments.rs`**: `is_ack_active` is now `pub(crate)` so the daemon side can reuse the TOML expiry check without duplicating the parse logic. The "deferred to 0.5.18" header comment is dropped since the runtime ack ships in this release.
- **`crate::event::truncate_field` promoted to `pub(crate)`** and reused by `daemon::ack` for the `by` and `reason` field caps (256 bytes on `by`, 1024 bytes on `reason`). Single source of truth for byte-and-char-boundary truncation across the crate.
- **`init_ack_resources` error policy split by source**: when the operator explicitly set `[daemon.ack] storage_path` or `[daemon.ack] toml_path`, a load failure on that path is fatal at startup with a typed `DaemonError`. When the path was resolved from the default, failures are logged at WARN and the daemon stays up so OTLP ingestion, `/metrics` and `/health` keep serving. The three ack endpoints return `503 Service Unavailable` until the operator fixes the default path and restarts.
- **Helm chart 0.2.22 → 0.2.23**, `appVersion` 0.5.19 → 0.5.20 (lockstep).

### Documentation

- `docs/QUERY-API.md` and `docs/FR/QUERY-API-FR.md`: full reference for the three new endpoints with curl examples, request and response shapes, status code matrix, and a TOML and JSONL interop section documenting the conflict resolution rules.
- New `[daemon.ack]` section in `docs/CONFIGURATION.md` and FR mirror with field reference and rationale (why no `/tmp` fallback, why mode `0600`, what the `api_key` length floor means). New "Acknowledging findings at runtime" section in `docs/RUNBOOK.md` and FR mirror with three curl examples.

### Notes

- Three observability followups from the B3 simulation lab were investigated and closed on 2026-05-04: process collector cfg-gating (already documented Linux-only, no action), OTLP rejected reasons extension (YAGNI confirmed, the three reasons cover all applicative rejection sites), and `warning_details` transient vs sticky semantics (doc-only clarification shipped here).

## [0.5.19]

Closes three observability gaps in the daemon surfaced by downstream validation work on the simulation lab. Standard process collector metrics are now exposed on `/metrics` on Linux, a new `perf_sentinel_otlp_rejected_total{reason}` counter quantifies OTLP backpressure, and `Report.warning_details: Vec<Warning>` adds a structured `{kind, message}` channel alongside the legacy `Report.warnings: Vec<String>` field. The three fixes are purely additive on the observability layer, no ingestion behavior changes. The release also lands the supply-chain pinning policy documentation as `docs/SUPPLY-CHAIN.md` (and its FR mirror).

### Added

- **`perf_sentinel_otlp_rejected_total{reason}`** counter on `/metrics` (`crates/sentinel-core/src/report/metrics.rs`). 3 reason labels: `unsupported_media_type` (HTTP only, `Content-Type` is not `application/x-protobuf`), `parse_error` (HTTP only, prost decode failed), `channel_full` (HTTP and gRPC, event channel saturated). All pre-warmed to 0 at startup so dashboards plot the zero-line before the first rejection. `payload_too_large` is intentionally absent: tower-http and tonic enforce the cap upstream and reject before the application handler runs.
- **Process collector metrics** on `/metrics` (Linux only): `process_resident_memory_bytes`, `process_virtual_memory_bytes`, `process_open_fds`, `process_max_fds`, `process_start_time_seconds`, `process_cpu_seconds_total`. Registered via `prometheus::process_collector::ProcessCollector::for_self()` behind `#[cfg(target_os = "linux")]` so the macOS and Windows builds do not pay for failed `/proc/self/*` reads on every scrape.
- **`Report.warning_details: Vec<Warning>`** field on the report payload, with `Warning { kind: String, message: String }` defined in the new `crates/sentinel-core/src/report/warnings.rs` module. Two `kind` values ship in 0.5.19: `cold_start` (returned by `/api/export/report` until the first batch lands) and `ingestion_drops` (computed dynamically from `otlp_rejected_total{channel_full}` when positive). Renderers prefer the structured field when non-empty and fall back to the legacy `Report.warnings: Vec<String>` (0.5.16+) otherwise.
- **`Warning::from_untrusted(kind, message)`** constructor that strips Unicode BiDi-override and invisible-format characters via `report::sarif::strip_bidi_and_invisible`. Trojan Source defense (CVE-2021-42574) for future contributors wiring a Warning sourced from an OTLP attribute or any other attacker-influenced channel. Documented as the required entry point for untrusted bytes in the module-level doc comment.
- **14 new tests** across `report::warnings`, `report::mod`, `report::metrics`, `ingest::otlp`, `daemon::query_api`, plus 1 e2e test in `crates/sentinel-cli/tests/e2e.rs` that pins the JSON shape of `Report.warning_details`. Includes a `#[cfg(not(target_os = "linux"))]` symmetric test that locks the platform gating of the process collector.
- **`crate::test_helpers::empty_report()`** factory for unit tests that need a default `Report` shape, replacing the long boilerplate at every call site.

### Changed

- **`MetricsState` caches the 3 OTLP rejection counters as `IntCounter` fields** (`otlp_rejected_unsupported_media_type`, `otlp_rejected_parse_error`, `otlp_rejected_channel_full`). `record_otlp_reject(reason)` becomes a branchless `match` plus atomic `inc()`, no per-rejection HashMap label lookup. Avoids amplifying daemon slowdown via metric overhead under a backpressure storm. The `IntCounterVec` is kept on the struct for `/metrics` rendering and tests, only the hot path uses the cached children.
- **`otlp_http_router` and `OtlpGrpcService::new` accept `Option<Arc<MetricsState>>`** as a new parameter (`crates/sentinel-core/src/ingest/otlp.rs`). `Some(metrics)` in daemon mode (passed through `daemon/listeners.rs`), `None` for batch CLI and tests so the existing call sites stay zero-cost. Each rejection site (HTTP unsupported_media_type, HTTP parse_error, HTTP channel_full, gRPC channel_full) calls `m.record_otlp_reject(reason)` when the metrics handle is present.
- **`docs/ci-templates/` `PERF_SENTINEL_VERSION` pin bumped from `0.5.17` to `0.5.18`** across `gitlab-ci.yml`, `github-actions.yml`, `github-actions-baseline.yml`, and `jenkinsfile.groovy`.

### Behavior

- **No change to ingestion behavior.** Requests that were accepted before are still accepted, rejected requests still return the same status codes (`415`, `400`, `503` HTTP, `INTERNAL` gRPC). The difference is that rejections are now visible in `/metrics` and `Report.warning_details`.
- **Backward compatibility on `Report` JSON.** The new `warning_details` field is additive via `serde(default, skip_serializing_if = "Vec::is_empty")`. Pre-0.5.19 baselines saved with `report --before <baseline.json>` parse without modification. The legacy `warnings: Vec<String>` field (0.5.16+) is preserved byte-for-byte, populated as before by the daemon cold-start path.
- **Process metrics are Linux only.** Operators on macOS and Windows hosts continue to see the `perf_sentinel_*` metrics and nothing else under `process_*`. The `prometheus` crate's `process` feature is now activated, but the registration site is gated by `#[cfg(target_os = "linux")]` so non-Linux scrapes do not pay for failed `/proc/self/*` reads.
- **Built artifacts are slightly larger.** Activating the `process` feature pulls `procfs` as a transitive dependency on Linux. A few KB on the binary, no runtime cost off the scrape path.

### Documentation

- New `docs/METRICS.md` and `docs/FR/METRICS-FR.md`: exhaustive per-metric reference for everything exposed on `/metrics`, grouped by category (process, OTLP ingestion, analysis and findings, GreenOps), with cardinality, label catalog, a per-scrape cost note for the process collector, and an exposure scope note recommending Kubernetes `NetworkPolicy` plus Prometheus mTLS when the daemon binds to `0.0.0.0`.
- New `docs/SUPPLY-CHAIN.md` and `docs/FR/SUPPLY-CHAIN-FR.md`: the pinning policy reference. Documents what gets SHA-pinned (`.github/workflows/` actions), what stays on `latest` (Helm CLI lints, lower-risk because no repo perms or secrets access), and the `docs/ci-templates` drift acceptance.
- `docs/RUNBOOK.md` and `docs/FR/RUNBOOK-FR.md` extended with two diagnostic recipes ("Diagnosing OTLP drops" and "Reading Report warnings"), including the rationale for why `payload_too_large` is not counted by the new counter.
- `README.md` and `README-FR.md` mention `warning_details` and the new `/metrics` surfaces in the daemon section, with cross-links to the new docs.

## [0.5.18]

Closes a cross-format gap surfaced in the simulation-lab CI/CD validation work: the SARIF emitter now exposes the canonical finding signature so SARIF-aware tools (GitHub Code Scanning, GitLab SAST, Sonar) can match findings against `.perf-sentinel-acknowledgments.toml` without parsing the JSON output separately. The signature appears in two places per result, `properties.signature` for parity with the existing ack metadata and `fingerprints["perfsentinel/v1"]` for SARIF v2.1.0 native deduplication. Both fields hold the same value. The release also tightens the canonical signature against Trojan Source spoofing (CVE-2021-42574) so the entire matching chain (TOML file, JSON output, SARIF output) shares the canonical form.

### Added

- **`SarifProperties.signature: Option<String>`** field on the SARIF result properties bag, populated from `Finding.signature` when non-empty. Skipped via `serde(skip_serializing_if = "Option::is_none")` on legacy baselines (pre-0.5.17 reports without the field).
- **`SarifResult.fingerprints: Option<BTreeMap<&'static str, String>>`** field on the SARIF result, single-entry map keyed by `"perfsentinel/v1"`, value is the canonical signature. SARIF v2.1.0 section 3.27.17 fingerprint, used by GitHub Code Scanning and GitLab SAST for deduplication across runs. `BTreeMap` over `HashMap` for stable JSON ordering, `&'static str` key over owned `String` to skip one allocation per finding.
- **Signature emission for acknowledged findings.** When `--show-acknowledged` is set, each acked SARIF result carries the same `properties.signature` and `fingerprints["perfsentinel/v1"]` so a tool that round-trips the SARIF output back into the ack file can copy-paste the value directly.
- **5 new tests**: 4 unit tests in `crates/sentinel-core/src/report/sarif.rs` (signature in properties, signature in fingerprints, both omitted when empty, acked finding carries both), 1 e2e test in `crates/sentinel-cli/tests/e2e.rs` (`cli_signature_consistent_across_json_and_sarif`) that runs `analyze` twice on the same fixture in JSON and SARIF mode and asserts the signature is identical across both surfaces. The e2e test guards against future format drift between the two emit paths.
- **1 unit test** in `crates/sentinel-core/src/acknowledgments.rs` (`compute_signature_strips_bidi_and_invisible_from_service_and_endpoint`) verifying that `service = "alice\u{202E}@evil.com"` and `service = "alice@evil.com"` produce the same canonical signature so a hostile span attribute cannot fork ack matching.

### Changed

- **`compute_signature` strips BiDi and invisible-format characters** from `service` and `source_endpoint` before formatting the canonical signature. Reuses the existing `strip_bidi_and_invisible` helper from the SARIF emitter, now exposed as `pub(crate)` so the discipline stays consistent across the matching chain. The hash component (template SHA-256 prefix) is unchanged. In practice no production service has BiDi characters in its name, the change is defense in depth.
- **`acknowledged_finding_to_result` mutates `SarifProperties` in place** instead of rebuilding the struct after the call to `finding_to_result`. Saves one allocation per acknowledged finding and removes the duplicate signature-capture guard that would have drifted as new fields are added to `SarifProperties` over time.
- **`docs/ci-templates/` `PERF_SENTINEL_VERSION` pin bumped from `0.5.8` to `0.5.17`** across `gitlab-ci.yml`, `github-actions.yml`, `github-actions-baseline.yml`, and `jenkinsfile.groovy`, so a fresh curl of the templates no longer pulls a stale pin.

### Behavior

- **Default behavior preserved.** Active findings continue to render the same way in JSON, HTML, and terminal output. The new SARIF fields are additive and skipped when the source signature is empty, so existing SARIF consumers parse the output unchanged. The signature value itself is identical to 0.5.17 for any finding whose `service` and `source_endpoint` are pure ASCII (the overwhelming majority of production setups). Acks created in 0.5.17 against ASCII-named services continue to match in 0.5.18 byte-for-byte.
- **Edge case for non-ASCII service names with BiDi characters.** If a `.perf-sentinel-acknowledgments.toml` entry was created in 0.5.17 against a service whose name carried BiDi or zero-width format characters (extremely rare), the entry will not match in 0.5.18 because the new signature strips those characters. Re-run `analyze --format json | jq '.findings[].signature'` to capture the new signature and update the TOML.

### Documentation

- New `docs/SARIF.md` and `docs/FR/SARIF-FR.md`: dedicated reference for the SARIF format emitted by perf-sentinel (per-result fields, tool driver, schema URL), with a cross-link to `ACKNOWLEDGMENTS.md` for the cross-format ack matching workflow.
- `docs/ACKNOWLEDGMENTS.md` and `docs/FR/ACKNOWLEDGMENTS-FR.md` gain a "SARIF integration" section explaining the two emission sites and when to pick `properties.signature` vs `fingerprints["perfsentinel/v1"]` based on the consumer.
- `README.md` and `README-FR.md` simplified the global-integration diagram to drop the dark-variant `<picture>` block in favor of a single `![](...)` rendering. Cosmetic, no scope change.

## [0.5.17]

Adds team-wide acknowledgments for findings via a new `.perf-sentinel-acknowledgments.toml` file at the root of the application repo. Two architects asked for a way to silence specific findings the team has accepted as known and intentional, so the next CI run focuses on what is new instead of re-reporting the same baseline. The decisions live in version control, every change goes through PR review, and `git log .perf-sentinel-acknowledgments.toml` is the audit trail.

This is the CI / batch path of a two-part feature. The daemon-side runtime ack (sticky entries with optional TTL stored in a local SQLite) is deferred to a later release pending architecture review.

### Added

- **`.perf-sentinel-acknowledgments.toml` file at the repo root.** TOML format, one `[[acknowledged]]` block per ack. Required fields: `signature`, `acknowledged_by`, `acknowledged_at`, `reason`. Optional: `expires_at = "YYYY-MM-DD"` for a periodic re-evaluation requirement. A typo in any required field or a malformed `expires_at` fails the run loud rather than silently widening the matched set.
- **`Finding.signature: String`** field, always emitted in JSON output. Format `<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>` (16-hex prefix, ~64 bits of collision resistance). The triple already discriminates by service and endpoint, so the hash only disambiguates templates within the same triple. The 16-char prefix is defense in depth against accidental ack masking after a SQL refactor. Operators copy-paste the value directly from `analyze --format json | jq '.findings[].signature'` into the ack file. Additive on pre-0.5.17 baselines via `#[serde(default)]`.
- **`Report.acknowledged_findings: Vec<AcknowledgedFinding>`** field, populated when the ack file matches at least one finding. Hidden from the wire payload by default (CLI clears it before emission), surfaced via `--show-acknowledged`. Each entry pairs the original `Finding` with the matching `Acknowledgment` metadata. Additive via `serde(default, skip_serializing_if = "Vec::is_empty")`.
- **Three CLI flags** on `analyze`, `report`, `inspect`, `diff`, `tempo`, `jaeger-query`: `--acknowledgments <path>` (override the default `./.perf-sentinel-acknowledgments.toml`), `--no-acknowledgments` (disable filtering, audit view), `--show-acknowledged` (include ack details in output). `inspect` omits `--show-acknowledged` because the TUI does not yet have a dedicated panel, the status footer surfaces the count.
- **SARIF properties extended** with `acknowledged: true`, `acknowledgmentReason`, `acknowledgmentBy`, `acknowledgmentAt` on results emitted from acknowledged findings (only when `--show-acknowledged`). Existing SARIF consumers parse without change because the new properties are additive. Free-text values are stripped of Unicode BiDi-override and invisible-format characters before emission, matching the `sanitize_sarif_filepath` discipline so a hostile `acknowledged_by` cannot spoof the displayed identity in GitHub / GitLab UIs.
- **16 MiB hard cap** on `.perf-sentinel-acknowledgments.toml`. A stray `--acknowledgments /dev/zero` or a multi-GB malformed TOML now fails fast with `AcknowledgmentLoadError::TooLarge` instead of exhausting process memory. Mirrors the trace-ingest payload-cap discipline.
- **Quality gate re-evaluation** after ack filtering, so an N+1 SQL critical finding that was the only blocker for `n_plus_one_sql_critical_max = 0` flips the gate from FAIL to PASS once acked. The `io_waste_ratio_max` rule is unaffected because it reads from raw spans (not findings), this asymmetry is documented in `docs/ACKNOWLEDGMENTS.md`.
- **15 unit tests** in `crates/sentinel-core/src/acknowledgments.rs` covering signature determinism + format, expired vs future vs permanent acks, gate re-evaluation, file load with valid / missing-field / malformed-date inputs, and the no-op when the file is absent.
- **6 e2e CLI tests** in `crates/sentinel-cli/tests/e2e.rs` covering the four-flag matrix (default, `--acknowledgments`, `--no-acknowledgments`, `--show-acknowledged`), signature emission in JSON output, and the no-op when the ack file is absent in the cwd.
- **New documentation**: `docs/ACKNOWLEDGMENTS.md` (EN) and `docs/FR/ACKNOWLEDGMENTS-FR.md` (FR) with the full workflow, signature format, FAQ. Cross-references from `README.md` / `README-FR.md`, `docs/CONFIGURATION.md` / FR mirror, and a new "Investigating an unexpected ack" section in `docs/RUNBOOK.md` / FR mirror.
- **17-panel Grafana dashboard** at `examples/grafana-dashboard.json` (was 8 panels), lifting coverage of registered daemon metrics to 11/11 (was 6/11). New panels: OTLP HTTP retry-after gate, sampling drops, batch processing histograms, actionable-fix coverage ratio, the cold-start `Report.warnings` signal, and three additional rate and latency views aligned with the simulation-lab `verify-grafana-dashboard` scenario. The lab's previously distinct overview dashboard is replaced by a verbatim copy of this file, with a CI parity check.

### Changed

- **`pipeline::analyze` populates `Finding.signature`** at the end of detection via the shared `acknowledgments::enrich_with_signatures(&mut findings)` helper. The daemon ingestion loop (`daemon/event_loop.rs`) does the same so live snapshots from `/api/export/report` carry signatures usable for ack matching.
- **CLI Report-from-baseline paths** (`cmd_report --input <baseline.json>`, `cmd_diff --before`, `cmd_inspect --input <baseline.json>`) call `enrich_with_signatures` after deserialization so pre-0.5.17 baselines (without the `signature` field) still match acks correctly.
- **`emit_report_and_gate` signature** now takes a `show_acknowledged: bool` argument (callers pass `false` outside the four ack-aware subcommands). For the structured sinks (JSON, SARIF) it clones the report and clears `acknowledged_findings` before emission when the flag is off, so the wire payload stays the same as in 0.5.16. The terminal sink always prints a one-line count when acks matched and only prints the per-ack detail block when the flag is on.

### Behavior

- **Default behavior preserved when no ack file exists.** A run in a directory without `.perf-sentinel-acknowledgments.toml` produces identical output to 0.5.16, no error message, no warning. The new field on `Finding` (`signature`) is additive and does not alter existing JSON consumers because they ignore unknown fields.
- **Acks apply to both sides of `diff`.** The same ack file is loaded for the `before` and `after` runs so a finding suppressed on both sides drops out of the diff entirely. An ack landing between base and PR masks the finding only from the after run, surfacing it as a "resolved finding" in the diff (the right semantics: the team chose to accept it).
- **HTML dashboard payload** embeds `acknowledged_findings` only when `--show-acknowledged` is set on `report`. The JS template does not yet visually distinguish ack rows, downstream tooling can grep the embedded JSON for ack metadata. A dedicated UI panel is on the dashboard roadmap.
- **GreenOps semantics intentional.** Acknowledged findings are excluded from the quality gate (the entire point of "won't fix / accepted" semantics) but the carbon and waste numbers (`io_waste_ratio`, per-endpoint IIS, CO2 estimates) stay unchanged. An ack is a triage decision, not a physical mitigation: the I/O work is still happening, the energy is still being burned. The dashboard reflects honest accounting, the CI alert routing is what the ack controls.

### Documentation

- `README.md` and `README-FR.md` gain an "Acknowledging known findings" / "Acquitter les findings connus" section between the score-interpretation block and the architecture diagram.
- `docs/CONFIGURATION.md` and `docs/FR/CONFIGURATION-FR.md` document the file format, loading rules, and the no-glob-no-wildcard decision.
- `docs/RUNBOOK.md` and `docs/FR/RUNBOOK-FR.md` add an "Investigating an unexpected ack" section with the three-step diagnostic recipe (`--no-acknowledgments` to compare, `--show-acknowledged` to surface metadata, `git log` for the audit trail).
- `docs/ACKNOWLEDGMENTS.md` and `docs/FR/ACKNOWLEDGMENTS-FR.md` are new dedicated docs covering the full workflow, signature anatomy, quality gate semantics, and FAQ (handling stale acks, no-glob-by-design, `inspect` and HTML limitations).
- `README.md` and `README-FR.md` now also reference the [perf-sentinel-simulation-lab](https://github.com/robintra/perf-sentinel-simulation-lab) companion repo, which validates eight operational modes end-to-end on a real Kubernetes cluster (hybrid daemon-to-batch HTML, batch over Tempo, daemon OTLP direct, multi-format Jaeger / Zipkin, calibrate, sidecar, cross-trace correlation, `pg_stat_statements` integration). Each scenario ships a Mermaid architecture diagram, the exact inputs and outputs, the required configuration, and the gotchas that surfaced during validation.

### Dependencies

- **`sha2 = "0.11.0"`** added as a direct dependency of `sentinel-core` for the canonical signature hash. The 0.11 line moved from `generic-array` to `hybrid-array`, so it does not unify with the `sha2 0.10.x` still pulled transitively by rustls and both versions ship in the binary. Acceptable trade-off for tracking the upstream RustCrypto release.
- **`chrono = "0.4"`** added (default-features off, `clock` and `serde` features) for the `expires_at` ISO 8601 date parsing and the "is this ack still active" check.

## [0.5.16]

Four findings consolidated from post-0.5.15 validation. The `/metrics` endpoint now selects its content type from the client's `Accept` header in three modes (forced OpenMetrics on explicit request, legacy fallback when the client expresses no preference, plain Prometheus when the wildcard is refused), so OpenMetrics-strict scrapers get a conformant payload during cold-start and quiet windows. The `/api/export/report` cold-start path returns `200 OK` with an empty Report envelope and a `warnings` entry instead of `503 Service Unavailable`, removing a false-positive on Kubernetes probes and on CI scripts that treated 5xx as a daemon health issue. The `MAX_JSON_DEPTH = 32` cap is now exercised at the boundary (depth-31 must parse, depth-33 must reject) for all three ingest formats. The runbook documents the `kubectl port-forward + curl` pattern required to inspect a distroless daemon image.

### Changed

- **`/metrics` content negotiation now keys on the `Accept` header.** Three modes: (1) headers containing `application/openmetrics-text` force OpenMetrics 1.0 (with `# EOF` and exemplar annotations when present) regardless of state, (2) absent / `*/*` / `*/*` mixed in (vmagent-style `text/plain;*/*;q=0.1`) preserve the 0.5.15 behavior (OpenMetrics when `has_exemplars()`, plain otherwise), (3) strict `Accept: text/plain` without `*/*` forces plain Prometheus 0.0.4 with no exemplars and no `# EOF`. The legacy fallback is intentional, it preserves vmagent and curl by default so existing Grafana exemplar pipelines do not regress.
- **`/api/export/report` cold-start path returns `200 OK` with an empty envelope.** Pre-0.5.16 returned `503 Service Unavailable` with `{"error": "daemon has not yet processed any events"}`. The new shape is a complete Report with `findings: []`, `green_summary: GreenSummary::disabled(0)`, `analysis.events_processed = 0`, and `warnings: ["daemon has not yet processed any events"]`. This is a behavior change at the HTTP status code level. Consumers that explicitly switched on 503 for cold-start detection must update their checks. Use the `warnings` field or `analysis.events_processed == 0` as the new signal.
- **OpenMetrics media-type detection is token-aware and case-insensitive**, and skips tokens explicitly refused via `q=0` per RFC 7231 section 5.3.1. Hostile or unrelated tokens such as `application/openmetrics-text-foo` do not trigger the OpenMetrics path. The `*/*` wildcard detection remains substring-based to handle the non-RFC variant some scrapers emit, where `*/*` appears as a parameter rather than a comma-separated token.

### Added

- **`Report.warnings: Vec<String>`** field, additive on pre-0.5.16 baselines via `#[serde(default, skip_serializing_if = "Vec::is_empty")]`. Populated by the daemon's cold-start path. Empty in CLI batch output (`pipeline::analyze`). The `report --before <baseline>` flow continues to parse any 0.5.x baseline.
- **6 new boundary tests on the `MAX_JSON_DEPTH = 32` guard**, two per format. `native_ingest_accepts_input_at_depth_31`, `native_ingest_rejects_input_at_depth_33`, plus the symmetric pairs for Jaeger and Zipkin. The depth-50 stress tests added in 0.5.15 stay in place.
- **9 new tests on `/metrics` Accept negotiation**, covering all three modes (forced, legacy, plain strict) at both the unit level (`MetricsState::negotiate(accept)`) and the route level (axum integration tests for explicit OpenMetrics and vmagent-style `*/*` headers).
- **One cold-start `scoring_config` propagation test** locks the Electricity Maps audit chip on the cold-start path: `green_summary.scoring_config` is re-applied from the daemon's startup config on both the cold-start and the warm path, preserving the 0.5.12 audit-trail contract.
- **Distroless inspection guide** in `docs/RUNBOOK.md` (EN) and `docs/FR/RUNBOOK-FR.md` (FR). Documents the `kubectl port-forward + curl` pattern, the kubelet TCP probe, and the three-mode Accept negotiation summary.

### Behavior

- **`Report.warnings`** is the canonical signal for "daemon is in cold-start" on `/api/export/report`. The HTML dashboard does not yet render a banner for this field (out of scope, future polish), the CLI tools and consumers can detect it programmatically.
- **Legacy `/metrics` callers unchanged.** Test helpers and CLI batch callers that invoke `MetricsState::render()` or `MetricsState::content_type()` (no Accept header) continue to receive the 0.5.15 behavior. Both helpers are now wrappers around `negotiate(None)`.
- **`/api/export/report` Prometheus counter `export_report_requests_total` continues to bump on every request**, including cold-start responses, consistent with HTTP access-log conventions and identical to the 0.5.13 behavior.
- **No SARIF, JSON CLI, terminal, or HTML output format change** beyond the additive `Report.warnings` field. Existing dashboards, baselines, and SARIF integrations parse without code change.

### Documentation

- `docs/QUERY-API.md` (EN) and `docs/FR/QUERY-API-FR.md` (FR) updated to describe the 200-with-empty-envelope cold-start path.
- `docs/RUNBOOK.md` and FR mirror gain a "Inspecting the daemon's HTTP endpoints" section with the distroless workaround.

### Compatibility

- **VictoriaMetrics / vmagent**: vmagent does not advertise `application/openmetrics-text` in its scrape `Accept` header (per VictoriaMetrics issue #9239) but does include the `*/*` wildcard in its non-RFC `text/plain;*/*;q=0.1` form. The new dispatch routes that header to the legacy mode, preserving exemplars on the Grafana click-through path.
- **Prometheus**: Prometheus advertises `application/openmetrics-text` in its scrape Accept header and now receives a fully conformant OpenMetrics 1.0 body (with `# EOF`) on cold-start and quiet windows where 0.5.15 served plain Prometheus.

## [0.5.15]

Fixes two bugs surfaced by upstream operators. The daemon's `/metrics` endpoint now produces valid OpenMetrics 1.0.0 output when exemplars are present, so a Prometheus server negotiating `application/openmetrics-text; version=1.0.0` no longer rejects the payload (`up=0` despite a successful TCP connection was the symptom). The shared `MAX_JSON_DEPTH = 32` guard is now applied uniformly across all ingest paths, closing an asymmetry where Jaeger and Zipkin inputs relied on serde_json's looser 128-frame default while native event streams and Report JSON parsing already used the tighter project ceiling.

### Fixed

- **`/metrics` endpoint emits valid OpenMetrics 1.0.0 when exemplars are present.** Two non-conformities are corrected. The mandatory `# EOF` end-of-exposition marker is appended at the end of the body (spec section 5.1: "Expositions MUST end with EOF"), and exemplar annotations now include the mandatory numeric value (`# {trace_id="..."} 1.0` instead of `# {trace_id="..."}`). Pre-0.5.15 a Prometheus server in OpenMetrics negotiation mode read the body, failed at parse time, and reported `up=0` even though `scrape_samples_scraped > 0`. Grafana exemplar click-through is unaffected because it reads only the labels block, the `1.0` value is a spec-required placeholder ignored by every consumer that worked in 0.5.14.
- **`MAX_JSON_DEPTH = 32` is now enforced uniformly across Jaeger, Zipkin and native ingest paths.** Pre-0.5.15 only the Native arm of `JsonIngest::ingest` ran the project-wide depth guard, leaving Jaeger and Zipkin payloads on serde_json's default 128-frame stack. The asymmetry let JSON-bomb attempts targeting either format bypass the project ceiling. Both formats now reject input above 32 frames with the same `payload nesting exceeds maximum depth of 32` error message as the Native and Report JSON paths.
- **`MetricsState::render` doc-comment** updated to reflect the actual error-string return on encoder failure (was advertising `# Panics`, the function does not panic). A dead defensive newline guard was dropped, the post-condition is enforced by `inject_exemplars` itself.

### Behavior

- Plain Prometheus text format (`text/plain; version=0.0.4`, served when no exemplars are recorded) is unchanged. The `# EOF` marker only appears on the OpenMetrics negotiated path where it is mandatory.
- The exemplar numeric value `1.0` is a constant dummy. The OpenMetrics 1.0 spec (section 5.1.10) requires a numeric value after the labels block, no consumer of the perf-sentinel `/metrics` surface today reads it (Grafana, Prometheus, Mimir, and Tempo exemplar tooling all key on the `trace_id` label).
- Defense-in-depth on the JSON-bomb surface: the depth guard previously protected `analyze --input native.json`, `report --input <Report JSON>`, and `inspect --input <Report JSON>` since 0.5.14. It now also protects `analyze --input jaeger.json`, `analyze --input zipkin.json`, and the OTLP / Jaeger / Zipkin fallback in `JsonIngest::ingest`.
- No daemon code path changed beyond `MetricsState::render` and `MetricsState::inject_exemplars`. The HTTP handler, the metric registry, and the exemplar recording helpers are unchanged.

### Security

- Closes a pre-existing observation from the 0.5.13 security review: Jaeger and Zipkin ingest paths previously fell through to serde_json's 128-frame default rather than the project's tighter 32-frame ceiling. Tightens JSON-bomb defense uniformly across the ingest surface.

### Compatibility

- **VictoriaMetrics / vmagent scrape path**. VictoriaMetrics has parsed Prometheus exemplars in the form `metric value # {labels} value` since 2020 (shape `foo 123 # {bar="baz"} 1` in their changelog), so the new `# {trace_id="..."} 1.0` annotation is accepted. The `# EOF` marker is part of the OpenMetrics 1.0.0 spec that VictoriaMetrics implements. Note that `vmagent` does NOT advertise `application/openmetrics-text` in its scrape `Accept` header (intentional, per VictoriaMetrics issue #9239), but the perf-sentinel daemon still selects the OpenMetrics content type based on `has_exemplars()` rather than on the request `Accept` header. The body remains parseable by vmagent in practice because the OpenMetrics surface vmagent supports is a strict superset of the perf-sentinel `/metrics` output.
- **VictoriaTraces Jaeger Query API**. The new `MAX_JSON_DEPTH = 32` guard sits in `JsonIngest::ingest` on the local file / stdin ingest path. The remote `jaeger-query` subcommand (which is the path used to fetch traces from VictoriaTraces or upstream Jaeger) goes through `crates/sentinel-core/src/ingest/jaeger_query.rs` and does not route through `JsonIngest`, so the guard does not apply and cannot reject a VictoriaTraces response. Real-world VictoriaTraces responses nest 5-6 levels deep in practice, well below the 32-frame cap if the guard ever did apply on that path.

## [0.5.14]

Fixes a doc-impacting bug in `report --input` (and `inspect --input`) for Jaeger JSON exports. The subcommand's `--help` advertises "Same format auto-detection as `analyze --input` (native JSON, Jaeger, Zipkin v2)", but the shared helper `load_report_from_input` dispatched purely on the first non-whitespace byte. A top-level `{` was sent straight to the Report-JSON parser, so a Jaeger payload (`{"data": [...]}`) failed with `Error parsing --input as Report JSON: missing field 'analysis'`. Native event arrays and Zipkin v2 inputs (top-level `[`) were already accepted, only Jaeger was broken. The fix makes the `{` branch try Report first and fall back to `JsonIngest` (which routes Jaeger via `detect_format`) on parse failure. The daemon snapshot fast path stays intact, while Jaeger exports finally honour the documented contract.

### Fixed

- **`report --input` and `inspect --input` now accept Jaeger JSON exports as documented.** Workaround pipelines like `cat traces.jaeger.json | perf-sentinel analyze --format json | perf-sentinel report --input -` are no longer needed. The `docs/ci-templates/gitlab-ci.yml` template's commented-out Pages section, which feeds `report --input` with a raw Jaeger export, becomes effectively functional with no template change required.
- **Clearer error on unrecognized top-level objects.** When the input is neither a Report JSON nor a Jaeger export, the helper now surfaces `Error: --input top-level object is neither a pre-computed Report JSON nor a Jaeger export. Underlying error: ...` instead of the misleading `missing field 'analysis'` message that hid the real disambiguation. An explicit guard on `MAX_JSON_DEPTH` runs before the Report parse so an over-deep payload exits with the dedicated nesting-depth error instead of silently falling through to the ingest fallback.

### Behavior

- **Daemon snapshot pipelines unchanged.** `curl /api/export/report | perf-sentinel report --input -` keeps the fast path. A successful Report parse short-circuits before the ingest fallback, so there is no extra cost on the most common path.
- **`green_summary` audit-trail fields flow through verbatim** on the snapshot fast path: `top_offenders`, `regions`, `transport_gco2`, `co2`, and the `scoring_config` block introduced in 0.5.12 are deserialized straight from the daemon snapshot and rendered as-is, with no re-scoring. The new test `cli_report_accepts_report_snapshot_input` pins this contract on a populated fixture.
- **Native event arrays and Zipkin v2 inputs unchanged.** They were already routed correctly via the top-level `[` branch and `JsonIngest::detect_format`. The dispatch refactor preserves that behaviour with a regression-guard test.
- **Doc-comment of `Commands::Report` extended** with one sentence acknowledging the Report JSON snapshot path. The advertised auto-detection line on Jaeger, Zipkin, native is unchanged because it was always the intended contract, only the implementation rejoined the contract.
- **No SARIF, JSON, terminal or HTML output format change.** The fix is internal to the CLI helper.

## [0.5.13]

Two UX fixes against the 0.5.12 promise. Live daemons configured with Electricity Maps now serve a fully-populated `green_summary` on `/api/export/report`, so the chip banner and the GreenOps tab render on the HTML dashboard produced by `curl /api/export/report | perf-sentinel report --input -`. The CLI input default `max_payload_size` jumps from 1 MiB to 16 MiB so this canonical pipeline does not break silently on a modest cluster snapshot (1000 ringbuffered findings already exceed 1 MiB).

### Added

- **Live `green_summary` on `/api/export/report`**. The snapshot endpoint now serves a `GreenSummary` refreshed by the daemon's event loop after each batch (regions, top offenders, avoidable I/O ratio, CO2 numbers), instead of `GreenSummary::disabled(0)`. The chip banner introduced in 0.5.12 is now visible in the HTML rendered from a live daemon snapshot, not only in `analyze --format html` on a trace file.
- **`QueryApiState.green_summary: Arc<tokio::sync::RwLock<GreenSummary>>`** shared cell. Initialized to `disabled(0)` at daemon startup, mutated by the event loop after each `score_green` call (or after the disabled-branch `disabled(total_io_ops)` build when `green_enabled = false`), read by `handle_export_report` on every snapshot request. `RwLock` was chosen over `Mutex` because the access pattern is asymmetric: writes happen at batch frequency (a few per second), reads happen at human / CI poll frequency (typically less than once per minute), and the read path benefits from concurrent access.
- **Test `process_traces_publishes_green_summary_to_cell`** asserts the contract behind the snapshot path: each batch overwrites the shared cell so live snapshots pick up the latest CO2 picture.
- **Test `handle_export_report_serves_live_green_summary_after_batch`** asserts that a value written into the cell flows back through the handler verbatim (with `scoring_config` patched on top).
- **Test `handle_export_report_returns_503_when_events_in_but_no_batch_yet`** locks the new cold-start guard.

### Changed

- **Default `max_payload_size` raised from 1 MiB to 16 MiB.** A 1000-finding ringbuffer snapshot from `/api/export/report` already exceeds 1 MiB on a modest cluster, causing `curl /api/export/report | perf-sentinel report --input -` to fail silently at the previous default. The new default sits exactly at the comfort-zone upper boundary (`warn_unusual_daemon_limits` uses `..=16 MiB` inclusive), so the default does not trigger a startup warning. The 100 MiB hard cap is unchanged, configs with an explicit smaller `max_payload_size` value are unaffected. Three pre-existing tests (`default_config_has_safe_defaults`, `parse_empty_toml_gives_defaults`, `parse_partial_toml`) and one CLI test (`load_config_returns_default_when_no_file`) updated in place.
- **Doc-comment of `handle_export_report` updated**. The bullet about `green_summary.total_io_ops` being `0` on the snapshot path is removed (no longer true). The bullet about `analysis.duration_ms` being `0` is preserved (still applies, snapshot has no single-run duration to report).

### Behavior

- **Cold-start guard slightly tightened**: returns `503` while either `events_processed_total == 0` OR `traces_analyzed_total == 0`. The previous guard fired only on `events_processed == 0`, which left a window (up to `trace_ttl_ms / 2`, default 15 seconds) where events had been ingested but the first eviction tick had not yet fired, so the green_summary cell was still `disabled(0)`. Returning a meaningless 200 in that window was confusing for an operator pulling the snapshot through `perf-sentinel report --input -` immediately after starting the daemon (the GreenOps tab would not render). The new guard waits until the first batch has actually been scored before serving 200. The `disabled(0)` initial cell value is now provably unreachable on the read path. Hardening also added `sanitize_for_terminal` wrap on `top_offenders[].endpoint`, `top_offenders[].service` and `regions[].region` in the CLI terminal renderer (`print_green_summary`), defending against ANSI / OSC 8 / control-byte injection from a hostile OTLP sender or `--input` baseline. The wrap mirrors the 0.5.10 / 0.5.11 treatment of `intensity_estimation_method` and the daemon endpoint string.
- **`scoring_config` continues to surface on snapshots** whenever Electricity Maps is configured at daemon startup (introduced in 0.5.12). It is now applied on top of the live green summary in the handler: the event loop emits the per-batch summary without the methodology metadata, the handler stitches it back from the daemon's startup config.
- **No SARIF format change.** No wire-format change to `analyze --format json` (the same field that was already emitted for batch mode is now also emitted on the snapshot path).
- **Backward compat** for explicit configs: a `max_payload_size = 1048576` line in TOML still works exactly as before. The new default only applies when the field is absent.
- The `cli_analyze_rejects_oversized_file` e2e test now pins `max_payload_size = 1048576` via a TOML config so the test stays cheap (writing a 16 MiB file just to trip the guard would balloon the test fixture).

### Documentation

- **README JSON example refreshed for the audit-grade shape.** `code_location`, `suggested_fix`, `green_summary.scoring_config` and `per_endpoint_io_ops` are now visible in the example, the CO2 model is updated to `io_proxy_v3`, the region resolves to `eu-west-3` with `monthly_hourly` intensity, and the quality gate threshold matches the current default. A reproduction snippet under the example shows the minimal TOML config that produces an audit-grade JSON output from the demo fixture.

## [0.5.12]

Surfaces the active `Electricity Maps` scoring configuration (API version, emission factor type, temporal granularity) in three places: the JSON `green_summary.scoring_config` object, a chip bandeau above the dashboard's green-regions table, and a one-line `Carbon scoring: ...` header in the terminal `print_green_summary` output. Closes the audit-trail gap left after 0.5.11 added the two TOML knobs but did not surface them in the rendered output. Operators can now verify their TOML choices took effect, and Scope 2 reporters can audit the carbon model used to produce the numbers without reading the operator's config.

### Added

- **`green_summary.scoring_config` JSON object** exposing the active Electricity Maps configuration (API version, emission factor type, temporal granularity). The field is omitted (additive on pre-0.5.12 baselines via `skip_serializing_if = "Option::is_none"`) when Electricity Maps is not configured.
- **Public `ScoringConfig` struct** in `score::carbon` with `from_electricity_maps(&ElectricityMapsConfig) -> Self`. `Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq`. Surfaced on `CarbonContext::scoring_config: Option<ScoringConfig>` and copied into `GreenSummary::scoring_config` at the `score_green` assembly point.
- **Public `ApiVersion` enum** in `score::electricity_maps::config` with `from_endpoint(&str) -> Self` (now the authoritative source of v3 / v4 / custom detection, also consumed by `warn_if_legacy_v3_endpoint`) and `as_chip_label(self) -> &'static str`. `#[serde(rename_all = "lowercase")]` so the wire form is `"v3"` / `"v4"` / `"custom"`.
- **Dashboard scoring config bandeau** above the green-regions table. Three chips render systematically when Electricity Maps is configured: API version (orange `v3` for legacy, neutral `v4` or `custom`), emission factor type (neutral `lifecycle` default, accent `direct` opt-in), temporal granularity (neutral `hourly` default, accent `5_minutes` or `15_minutes` opt-in). Tooltips mirror the deprecation and methodology guidance. Hidden when Electricity Maps is not configured.
- **Terminal `Carbon scoring: ...` header line** in `print_green_summary` output, emitted before the per-region breakdown. Format: `Carbon scoring: Electricity Maps v4, lifecycle, hourly`. Hidden when Electricity Maps is not configured. Helper `format_scoring_config_line` returns `Cow<'static, str>` with two constant arms covering the most common shapes (v4/v3 with both knobs at defaults), allocating only on opt-in combinations.
- **`Serialize` and `Deserialize` derives on `EmissionFactorType` and `TemporalGranularity`**. `EmissionFactorType` uses `#[serde(rename_all = "lowercase")]`, `TemporalGranularity` carries explicit `#[serde(rename = "5_minutes")]` and `#[serde(rename = "15_minutes")]` because the wire form starts with a digit and is not a valid Rust identifier.

### Changed

- **`warn_if_legacy_v3_endpoint` refactored** to delegate detection to `ApiVersion::from_endpoint`, single source of truth. The local `is_legacy_v3_endpoint` helper is dropped, its 5 unit tests migrate to `ApiVersion::from_endpoint` assertions in `electricity_maps/config.rs::tests`. Behavior unchanged. The `sanitize_for_terminal` wrap around the operator-supplied endpoint is preserved, defending against ANSI / OSC 8 / control-byte injection through a hostile TOML.

### Behavior

- Pre-0.5.12 JSON reports without `scoring_config` deserialize without error (additive `Option<ScoringConfig>` field, defaulted to `None`). The `report --before <baseline.json>` flow keeps working with stored baselines from any 0.5.x release.
- The daemon's `/api/export/report` snapshot endpoint still returns `GreenSummary::disabled(0)` (no live scoring), but the handler patches `scoring_config` from the daemon's loaded `Config` so the audit chip surfaces on the snapshot whenever Electricity Maps is configured at daemon startup. An operator pulling the snapshot does not get a misleading `scoring_config: null` when EM is in fact in use.
- Defense against terminal injection in the new surface: the three fields are typed Rust enums with bounded variants, so the terminal renderer skips `sanitize_for_terminal` (unlike `intensity_estimation_method` which carries a free-form `String` from `--input` JSON). The HTML chip rendering uses `textContent` and `setAttribute("title", ...)`, both of which auto-escape.
- The HTML chip helpers (`buildApiVersionChip`, `buildEmissionFactorChip`, `buildTemporalGranularityChip`) now fall through to a verbatim-text neutral chip on unknown values rather than silently rendering the default. Forward-compat: a future enum variant added in a later release renders verbatim in an older dashboard instead of masquerading as the default.
- Wire format unchanged for users not opting into Electricity Maps. URL shape, `green_summary.regions[]` rows, `top_offenders`, IIS and waste ratio are byte-identical to 0.5.11 in that case.
- The `scoring_config` object captures the `Electricity Maps` client configuration only, not the full SCI input vector. A complete strict-replay of the carbon math from a saved baseline would also need `[green] embodied_carbon_per_request_gco2`, `[green] use_hourly_profiles`, `[green] per_operation_coefficients`, `[green] include_network_transport`, `[green] network_energy_per_byte_kwh`, plus the per-region PUE drawn from the embedded provider table. Surfacing the complete methodology footprint is tracked as future work, the 0.5.12 surface closes the audit gap on the `Electricity Maps` slice specifically.

## [0.5.11]

Switches the default `Electricity Maps` API endpoint from v3 to v4, exposes two new TOML knobs that map to `Electricity Maps` v4 query parameters (`emissionFactorType` and `temporalGranularity`), and emits a one-shot deprecation warning at daemon startup when a v3 endpoint is detected. The two API versions return byte-identical responses on the `carbon-intensity/latest` endpoint perf-sentinel uses, so the version migration is transparent for downstream consumers. v3 keeps working for users who explicitly pin it in their TOML.

### Added

- **`[green.electricity_maps] emission_factor_type` TOML knob**. Maps to the `emissionFactorType` API query parameter. Accepted values: `"lifecycle"` (default, includes upstream emissions like manufacturing and transport) or `"direct"` (combustion only, preferred by some Scope 2 frameworks for stricter accountability). Unknown values trigger a `tracing::warn!` and fall back to `lifecycle`.
- **`[green.electricity_maps] temporal_granularity` TOML knob**. Maps to the `temporalGranularity` API query parameter. Accepted values: `"hourly"` (default), `"5_minutes"`, `"15_minutes"`. Sub-hour granularities require a paid `Electricity Maps` plan that exposes them, otherwise the API silently coarsens to hourly. Unknown values trigger a `tracing::warn!` and fall back to `hourly`.
- **Public `EmissionFactorType` and `TemporalGranularity` enums** in `crates/sentinel-core/src/score/electricity_maps/config.rs`, both with `from_config(Option<&str>) -> Self` (case-insensitive parsing with graceful fallback) and `as_query_value(self) -> &'static str` (URL serialization).
- **Pure helper `build_request_url`** in `crates/sentinel-core/src/score/electricity_maps/scraper.rs` that composes the full request URL from the endpoint, zone, and the two knobs. Query params are only appended when they differ from the API defaults, so the wire is byte-identical to pre-0.5.11 for users who do not opt into the knobs.

### Changed

- **Default `Electricity Maps` endpoint flipped from v3 to v4**. New configs (no explicit `endpoint` field) now target `https://api.electricitymaps.com/v4`. The new public constant `DEFAULT_ELECTRICITY_MAPS_ENDPOINT` in `crates/sentinel-core/src/score/electricity_maps/config.rs` is the single source of truth, used by the production fallback in `convert_electricity_maps_section_with_env` and by the test fixtures.
- **`api_endpoint` is now trimmed of trailing slashes** at config load time, so a copy-paste like `endpoint = "https://api.electricitymaps.com/v4/"` no longer produces a double-slash URL when the scraper appends `/carbon-intensity/latest?zone=...`.
- **BREAKING** (`perf-sentinel-core`, pre-1.0 so minor-bump allowed): `ElectricityMapsConfig` gains two new public fields `emission_factor_type: EmissionFactorType` and `temporal_granularity: TemporalGranularity`. External consumers constructing the struct directly must add the two fields.

### Deprecated

- **`Electricity Maps` API v3 endpoint usage**. Configs that pin `endpoint = "https://api.electricitymaps.com/v3"` keep loading and continue to work, but the daemon now emits a `tracing::warn!` once at startup pointing the operator at the v4 migration. To silence the warning, switch to the v4 endpoint. To deliberately stay on v3 (for example to A/B-validate against v4), keep the v3 URL and acknowledge the warning. Electricity Maps has not announced a v3 retirement date, this is forward-defense.

### Security

- **`endpoint` field sanitized at log time**. The deprecation warn passes the endpoint string through `sanitize_for_terminal` before logging, so a hostile TOML config carrying ANSI / OSC 8 / control bytes in the URL cannot inject terminal escape sequences into the daemon's log stream. Mirrors the 0.5.10 fix on `intensity_estimation_method`.
- **Unknown knob values sanitized at log time**. The same hardening applies to the fail-graceful warn path on `emission_factor_type` and `temporal_granularity`. A typo like `temporal_granularity = "WIPE\x1b[2J"` is logged with control bytes replaced by `?`.

### Behavior

- Wire format unchanged. Both versions of the API return the same JSON schema on `carbon-intensity/latest`, so `green_summary.regions[]` rows are byte-identical between the two.
- GreenOps aggregates unchanged. `avoidable_io_ops`, IIS, waste ratio, `top_offenders` are not touched.
- Default URL shape unchanged. Users not opting into the new knobs see the exact same `?zone=XX` URL as before.
- Existing scraper integration tests (mock HTTP servers on `127.0.0.1:NNNN` without `/vN` suffix) are unaffected by the new deprecation detection.

### Tests

- New `fetch_intensity_v3_and_v4_responses_parse_identically` regression test locks the byte-identical `green_summary.regions[]` parity between the v3 and v4 API responses.

## [0.5.10]

Surfaces the `intensity_estimated` and `intensity_estimation_method` fields shipped in 0.5.9 in the two user-visible rendering surfaces, so operators no longer have to parse JSON manually to know whether a carbon intensity value was measured by Electricity Maps or modeled. Scope 2 reporters and demo viewers see the distinction at a glance.

### Added

- **Dashboard "Estimated" column** on the green-regions table. New 6th column with three visual states: orange `Estimated` badge (with hover tooltip surfacing the `estimationMethod`), green `Measured` badge (with tooltip mentioning measurement provenance), or a neutral dash for rows whose `intensity_source` is not `real_time`. Both badges reuse the existing palette CSS variables (`--color-background-warning`, `--color-text-warning`, `--color-background-success`, `--color-text-success`) so dark and light themes adapt automatically.
- **Terminal estimation suffix** on every per-region line emitted by `print_green_summary`. Format: `, estimated/<METHOD>` when the method is set, `, estimated` when the flag is true without a method, `, measured` when explicitly measured, no suffix when the metadata is absent (pre-0.5.9 fixtures, non-`Electricity Maps` sources). The line layout otherwise stays identical so existing log-scrapers keep matching.

### Changed

- **Internal: `run_scraper_loop` refactor.** Split into `run_one_tick`, `fetch_zones`, `dispatch_readings` and `update_failure_counter` to drop SonarCloud cognitive complexity from 19 to under 15. Behavior is byte-identical, all 0.5.9 zone-set-level and all-or-nothing-per-shared-zone invariants preserved.

### Security

- **`intensity_estimation_method` passes through `sanitize_for_terminal` at the print sink.** A hostile `--input` JSON cannot inject ANSI / OSC 8 / control bytes that the API-side sanitizer would normally have stripped, matching the parity every other user-controlled field rendered by `print_green_summary` already enforced.

### Behavior

- No JSON or SARIF format change. Both already serialize the fields since 0.5.9, this release only changes the rendering layers.
- Backward compatible with pre-0.5.9 JSON reports: the dashboard renders a dash and the terminal omits the suffix when `intensity_estimated` is absent.
- No GreenOps aggregate change. The new metadata is read-only, never multiplied into the carbon math.

## [0.5.9]

Tightens the Electricity Maps real-time intensity scraper on two axes that surfaced when validating the integration against the official API documentation. The scraper now deduplicates zones before issuing API calls and exposes the `isEstimated` and `estimationMethod` metadata fields the API surfaces alongside `carbonIntensity`. No behavior change for users with a one-region-one-zone setup, the changes only matter when several `cloud_region` keys map to the same zone (multi-AZ setups, or staging+prod sharing a country code) or when consumers want to distinguish measured from estimated carbon intensity values for Scope 2 reporting.

### Improved

- **Per-zone API call dedup.** `run_scraper_loop` builds a `BTreeSet` of unique zones from `region_map.values()` and fetches each zone exactly once per tick, then dispatches the resulting `IntensityReading` to every `cloud_region` mapped to that zone. Previously the scraper made one API call per `cloud_region`, even when several pointed at the same zone. On quota-constrained tiers (free tier capped at one zone today) this keeps the API call count proportional to distinct zones, not to the size of `region_map`.
- **Fewer API calls on multi-AZ setups.** A `region_map` with `aws:eu-west-3 -> FR`, `local-k3d -> FR`, `aws:eu-central-1 -> DE` now hits the API twice per tick (one call per unique zone) instead of three times. Both FR cloud_regions resolve to the same intensity in the published state.

### Added

- **`isEstimated` parsing.** `CarbonIntensityResponse` now deserializes the optional `isEstimated: Option<bool>` field surfaced by the API. `Some(true)` means the value was estimated (Tier B/C zone, or temporal hole bridged by an algorithm), `Some(false)` means measured, `None` means the API did not surface the field (forward-compatibility with future API versions).
- **`estimationMethod` parsing.** `CarbonIntensityResponse` now deserializes the optional `estimationMethod: Option<String>` field surfaced by the API. Values like `"TIME_SLICER_AVERAGE"` or `"GENERAL_PURPOSE_ZONE_DEVELOPMENT"` are passed through verbatim. No whitelist is enforced, so the scraper survives the addition of new methods upstream.
- **`intensity_estimated` field on `green_summary.regions[]`.** New optional field on `RegionBreakdown` that surfaces the `isEstimated` flag when the row's `intensity_source` is `RealTime`. Absent for other sources (`Annual`, `Hourly`, `MonthlyHourly`).
- **`intensity_estimation_method` field on `green_summary.regions[]`.** New optional field on `RegionBreakdown` that surfaces the `estimationMethod` tag when the row's `intensity_source` is `RealTime`. Absent for other sources.
- **`RealTimeIntensityEntry` struct in `score::carbon`.** Public type carrying `gco2_per_kwh`, `is_estimated`, `estimation_method`. Replaces the previous `f64` value type used by `CarbonContext.real_time_intensity`. Includes a `RealTimeIntensityEntry::measured(f64)` convenience constructor for tests and callers without metadata.
- **`ElectricityMapsState::snapshot_with_metadata`.** New method returning `HashMap<String, RealTimeIntensityEntry>` for the daemon scoring path. The original `snapshot()` (returning `HashMap<String, f64>`) stays unchanged for callers that do not need the metadata.

### Changed

- **`CarbonContext.real_time_intensity` type signature.** Pre-1.0 internal API change. The field type went from `Option<HashMap<String, f64>>` to `Option<HashMap<String, RealTimeIntensityEntry>>`. In-tree callers were updated to construct entries via `RealTimeIntensityEntry::measured(value)` or the explicit struct literal when injecting estimation metadata.
- **`fetch_intensity` return type.** Pre-1.0 internal API change. The function now returns `Result<FetchedReading, EmapsScraperError>` instead of `Result<f64, EmapsScraperError>`. `FetchedReading` carries the gCO2/kWh value plus the optional estimation flags. The function is private to the scraper module.
- **`IntensityReading` (state) drops `Copy`.** The struct now carries an `Option<String>` (`estimation_method`) which prevents `Copy`. `Clone` is preserved. The struct is `pub(super)` so external consumers are not impacted.
- **`consecutive_failures` semantic is now zone-set-level** instead of request-level. With the dedup pass, a partial-success tick (zone FR succeeds, zone DE fails) resets the counter because at least one zone returned data. Only a tick where all unique zones fail will increment, matching the operator-facing intent of the diagnostic warn.

### Security

- **`estimationMethod` sanitized at the API boundary.** The value is capped at 64 bytes and rejected when the string contains control characters, defense-in-depth against log forging or downstream rendering surprises.

### Behavior

- Default behavior unchanged for the common one-region-per-zone setup. Users with a single `cloud_region -> zone` mapping see the same number of API calls per tick as before.
- Users with multiple `cloud_region` entries pointing at the same zone see fewer API calls per tick (down from N to the number of unique zones).
- The wire format of `green_summary.regions[]` is additive: new fields are optional and `skip_serializing_if = "Option::is_none"`, so existing consumers continue to deserialize and render the breakdown without changes.
- Empty `region_map`: the scraper now skips the tick instead of incrementing `consecutive_failures` and eventually firing a misleading "3 consecutive failures" warning. No real API call was attempted, no failure recorded.
- Stale-data precedence on partial failure preserved: when one zone fails mid-tick, the missed cloud_regions retain their previous reading (and previous metadata) until the next successful fetch. Their `last_update_ms` is not refreshed so the staleness filter eventually evicts them at the configured threshold.

## [0.5.8]

Adds a fourth `Strict` mode to the sanitizer-aware classification heuristic introduced in 0.5.7. Under `Strict`, both signals (ORM instrumentation scope AND timing-variance) must fire conjointly to reclassify a sanitized SQL group as `n_plus_one_sql`. The default stays `Auto` (either signal fires) so production users on Spring Data, EF Core and similar stacks see no behavior change. Operators who want to preserve `redundant_sql` precision on legitimate cached identical queries (legacy polling loops, unmemoized config lookups served from row cache) can opt in with `[detection] sanitizer_aware_classification = "strict"`.

### Added

- `[detection] sanitizer_aware_classification = "strict"`. New TOML mode that requires both signals (ORM scope + timing variance) to fire conjointly. Surfaces `n_plus_one_sql` only on N+1 patterns whose row-level cache state spreads the per-span durations enough to clear the empirical CV `> 0.5` threshold. Recommended when actionable `redundant_sql` findings are valuable signal that should not be silently absorbed. See `docs/CONFIGURATION.md` and `docs/design/04-DETECTION.md` for the recall/precision trade-off.
- `SanitizerAwareMode::Strict` enum variant and `classify_sanitized_sql_group_strict` public function in `crates/sentinel-core/src/detect/sanitizer_aware.rs`. The new function takes the same `(spans, scopes)` arguments as `classify_sanitized_sql_group` and returns `LikelyNPlusOne` only when `has_orm_scope && timing_variance_suggests_n_plus_one`.

### Changed

- `classify_sanitized_sql_group_indexed` (the hot-path entry point used by `detect_n_plus_one`) now takes a `mode: SanitizerAwareMode` parameter and dispatches between OR-logic (`classify_sanitized_sql_group`) and AND-logic (`classify_sanitized_sql_group_strict`) based on the mode.
- `classify_sanitized_sql_group` (the OR-logic public entry) now expresses its decision as a single boolean OR rather than two sequential `if` returns. Behavior unchanged.
- **`classify_sanitized_sql_group_indexed` dispatch is now exhaustive** (no `_` wildcard): `Strict` matches one arm, `Auto | Always | Never` matches the other. A future fifth variant on `SanitizerAwareMode` will fail to compile here rather than silently fall through to the OR logic.

### Behavior

- Default behavior unchanged: `Auto` still emits on either signal.
- New mode is opt-in only via `[detection] sanitizer_aware_classification = "strict"` in `.perf-sentinel.toml`.
- Findings reclassified under `Strict` carry the same `classification_method = "sanitizer_heuristic"` JSON marker as findings reclassified under `Auto` or `Always`.
- `Strict` does NOT change the `green_summary` aggregates: both `n_plus_one_sql` and `redundant_sql` weight identically into `avoidable_io_ops`, and the `score::compute` dedup key is `(trace_id, template, source_endpoint)`. Only the per-finding `type` label and the suggestion text differ between modes.
- Under `Always`, `detect_n_plus_one` now short-circuits before running the verdict computation, since the verdict was already ignored by the emit gate. Pure cleanup, no observable behavior change.
- Under `Strict`, the timing-variance threshold becomes load-bearing: it is the only gate that lets a sanitized group reach `LikelyNPlusOne` once the ORM scope check has passed. Real ORM-induced N+1 against a fully warm row cache (for example 100 lookups by primary key with all rows in `shared_buffers`) can cluster within ±10% (CV around 0.1) and stay silent under `Strict`. The 0.5 threshold is preserved across modes pending empirical validation. If real traffic shows the threshold to be too restrictive under `Strict`, the right follow-up is exposing a `[detection] sanitizer_aware_min_cv` knob rather than picking a new global default.

## [0.5.7]

Sanitizer-aware classification of SQL N+1 vs redundant. OpenTelemetry agents collapse SQL literals to `?` by default, which previously caused every ORM-induced N+1 to be misclassified as `redundant_sql` with the wrong remediation. A new heuristic recovers the correct classification, gated by an opt-in/opt-out TOML toggle. Findings reclassified by the heuristic carry a `classification_method` marker so consumers can spot where it is firing.

### Added

- `[detection] sanitizer_aware_classification = "auto" | "always" | "never"` (default `"auto"`). Controls a second-pass heuristic in `detect_n_plus_one` that activates when every span in a `(event_type, template)` group has an empty `params` vector and a `?` placeholder in its template (the on-wire signature of an OTel-sanitized SQL group). Two signals decide reclassification: a case-insensitive `instrumentation_scopes` match against a list of ORM markers (Spring Data, Hibernate, JPA, Micronaut Data, JDBI, R2DBC, EF Core, SQLAlchemy, Django, ActiveRecord, GORM, sqlx, Sequelize, Prisma, TypeORM, Mongoose, SeaORM, Diesel), and a fallback on the coefficient of variation of `duration_us` (threshold `0.5`, requires at least 3 spans). `auto` requires either signal, `always` reclassifies any sanitized group with `>= n_plus_one_min_occurrences` spans, `never` reproduces pre-0.5.7 behavior. See `docs/CONFIGURATION.md` and `docs/design/04-DETECTION.md`.
- New optional field `classification_method` on the `Finding` struct (JSON `classification_method`), with values `direct` (omitted when `None`) and `sanitizer_heuristic`. Findings emitted by the heuristic are stamped `sanitizer_heuristic`, every other finding leaves the field absent. The new `ClassificationMethod` enum is parallel to `Confidence`, not nested in it.
- **New `sanitizer_aware` module** in `crates/sentinel-core/src/detect/` exposing `SanitizerAwareMode`, `SanitizerVerdict`, `looks_sanitized`, `has_orm_scope` (allocation-free word-bounded ASCII match), `collect_scopes`, `timing_variance_suggests_n_plus_one` and `classify_sanitized_sql_group`. All functions are pure, with no allocation on the gate (`looks_sanitized_indexed`) so the heavy verdict only runs on groups that already pass the fast sanitized-shape check.

### Fixed

- N+1 SQL groups whose literals were collapsed to `?` by the OTel agent's statement sanitizer are now classified as `n_plus_one_sql` instead of `redundant_sql`. Affects every ORM stack with a real OTel agent (Java JPA, Java Quarkus, .NET EF Core, Python SQLAlchemy, Ruby ActiveRecord, Go database/sql, Node.js Prisma, Rust Diesel/SeaORM, ...) running with the default sanitizer ON. Operators get the correct remediation (batch fetch, `@EntityGraph`, eager loading) instead of "cache result or deduplicate".
- `detect_redundant` now accepts the slice of N+1 findings already produced for the same trace and skips templates that fired N+1, so the same template is never double-reported as both `n_plus_one_sql` and `redundant_sql`.

### Security

- Unknown `sanitizer_aware_classification` values warn with the offending value sanitized (control characters replaced, length capped at 32 bytes) and fall back to `"auto"`.

### Behavior changes

- Users with the OpenTelemetry SQL sanitizer ON (the default) will see findings move from `redundant_sql` to `n_plus_one_sql` on upgrade, with `classification_method = sanitizer_heuristic`. This is not a regression: the new classification carries the correct remediation. Operators who want to keep the pre-0.5.7 behavior can set `[detection] sanitizer_aware_classification = "never"`.
- `crates/sentinel-core` API: `detect::detect_n_plus_one` gains a `mode: SanitizerAwareMode` parameter and `detect::detect_redundant` gains an `n_plus_one_findings: &[Finding]` parameter. `DetectConfig` and `Config` gain a `sanitizer_aware_classification` field. `PerTraceFindingArgs` gains a `classification_method` field. External callers must update accordingly.

## [0.5.6]

Framework-aware suggestions now fire on Spring Boot + Spring Data JPA stacks. 0.5.5 shipped the `code.*` parent walker to recover attributes from ancestor spans, but `suggested_fix` still came back null on every JPA finding because the walker would stop on the user's Spring Data repository span where `code.namespace` is the user's class (for example `com.example.OrderRepository`), not a framework package. 0.5.6 closes the gap with two complementary signals: the OpenTelemetry instrumentation scope chain (`io.opentelemetry.spring-data-3.0`, `io.opentelemetry.hibernate-6.0`, etc.) captured at ingest, and the canonical Spring Data naming convention (`*Repository`, `*Repo`, `*Dao`) recognized on the user class itself. Either signal alone is enough to surface the JPA-specific `suggested_fix`. The same machinery applies to Quarkus reactive, Quarkus non-reactive and Spring WebFlux, so framework detection generalizes beyond JPA.

### Added

- **OpenTelemetry instrumentation scope chain captured per `SpanEvent`.** New optional `instrumentation_scopes: Vec<String>` field on `SpanEvent` and `Finding`. Populated at OTLP ingest by walking the parent chain (bounded by 8 hops) and collecting each unique `scopeSpans[].scope.name` leaf to root. Empty on Jaeger / Zipkin / native JSON ingest where no scope info is carried. Skipped from JSON serialization when empty, so the wire format stays backward-compatible with 0.5.5 consumers.
- **Scope-based framework detection in `detect_framework`.** New `SCOPE_RULES` table maps OTel scope short-names to internal frameworks: `spring-data` and `hibernate` to JPA, `hibernate-reactive` to Quarkus reactive, `quarkus` to Quarkus non-reactive, `spring-webflux` and `r2dbc` to WebFlux. Order matters (reactive variants ranked above the catch-all hibernate rule). The scope check runs before the namespace heuristics, so it dominates whenever the agent emits scope info. Boundary-aware match requires the `io.opentelemetry.` prefix and a segment boundary (`-` or end of string) after the needle, which excludes third-party tracers that happen to contain a framework name in their identifier (`com.acme.quarkus-monitoring` no longer matches the `quarkus` rule).
- **User-code naming convention hints in `JAVA_RULES`.** Three new `LastSegmentEndsWith` patterns (`Repository`, `Repo`, `Dao`) in the JPA rule. Catches Spring Data repositories whose `code.namespace` is `com.example.OrderRepository` rather than a Spring framework package. Existing framework-package substrings (`org.springframework.data.jpa`, `org.hibernate`, etc.) keep priority and still match first.
- **Namespace-only fallback when `code.filepath` is absent.** OTel agents often emit `code.namespace` on a parent span without `code.filepath`. 0.5.5 returned `None` from the detector in that case, so no fix attached. 0.5.6 falls back to iterating every language's rules in order (Java, C#, Rust) when the filepath is missing, returning the first match. No language-generic fallback fires here because we cannot pick one without a filepath signal.
- **`(RedundantSql, JavaJpa)` mapping in the `FIXES` table.** Was missing in 0.5.5 even though detection could identify JPA: the observed finding type is `redundant_sql`, so the lookup returned `None`. The new entry recommends `@Cacheable` on the repository or service method, or sharing the `EntityManager` within the request via `@Transactional` so Hibernate's first-level cache deduplicates the read. Reference URL points at the Spring Cache abstraction docs.

### Fixed

- **Cross-trace slow findings now carry `code_location` and `instrumentation_scopes` from the worst trace.** `detect_slow_cross_trace` previously emitted these fields as `None` / empty even when a representative span existed in the entries. The internal entry tuple now carries `&SpanEvent` so the framework-detection path runs on cross-trace findings too, matching the per-trace detectors' contract. JPA, Hibernate and friends are now recognized on cross-trace slow findings.

### Security

- **Sanitize `instrumentation_scopes` at the ingest boundary.** New `sanitize_string_vec` helper drops elements with ASCII control characters, truncates each scope to 256 bytes, and caps the Vec at 8 elements. Closes the regression introduced when the field landed without going through the canonical `sanitize_span_event` path: an attacker-supplied OTLP payload could otherwise stuff a 1 MB control-char-laden scope name into every event, amplified again on each `Finding.instrumentation_scopes.clone()`. The cap matches the OTLP parent-walk bound and also fires on the JSON ingest path which has no structural depth bound.

### Changed

- **Single-pass attribute classifier in `convert_span`.** Replaces ~14 separate linear scans over the OTLP attribute list with one pass that fills a `ClassifiedAttrs<'_>` struct via `match` on the key. ~13x fewer key comparisons per span at typical 30-attribute HTTP spans, with no allocation regression.

## [0.5.5]

OTLP ingest hardening. Three independent fixes that together unblock the default OTel Collector setup and restore framework-aware suggestions on JPA, EF Core and Diesel stacks.

### Fixed

- Walk parent span chain to recover `code.*` attributes. OTel auto-instrumentation typically attaches `code.namespace`, `code.function` and friends to the user-frame span (controller, service), not to the inner JDBC or HTTP-client span. perf-sentinel previously read these attributes only from the I/O span itself, so `JAVA_RULES`, `CSHARP_RULES` and `RUST_RULES` never fired on stacks that delegate I/O to a driver. The walk is bounded at depth 8 to stay safe on malformed parent chains. `suggested_fix` now appears on JPA, Hibernate, EF Core and Diesel findings even when the agent emits nothing on the leaf span.
- Accept gzip-compressed OTLP/HTTP exports on `POST /v1/traces`. The OTel Collector ships gzip by default, which previously triggered HTTP 400 on every export and forced users to set `compression: none`. perf-sentinel now wires `tower-http`'s `RequestDecompressionLayer` outside the existing `DefaultBodyLimit`, so `Bytes` extraction caps the decompressed payload at `[daemon] max_payload_size` and tower-http streams the decode with backpressure (no gzip-bomb amplification). Uncompressed clients keep working unchanged. The OTLP/gRPC path was already covered by tonic and is not affected.

### Changed

- Read OpenTelemetry semconv v1.33.0 stable code attribute names (`code.function.name`, `code.file.path`, `code.line.number`) with a fallback to the legacy names (`code.function`, `code.filepath`, `code.lineno`, `code.namespace`). When only the stable FQ function name is present, the namespace is derived by splitting on the last `.`, which keeps `JAVA_RULES`'s segment-anchored substring matching firing on `org.springframework.data.jpa` and friends. An explicit legacy `code.namespace` always wins over the derived value.

### Performance

- **Single-pass span attribute classifier on the OTLP hot path.** `convert_span` previously ran ~14 separate linear scans over the attribute list (one per `get_str_attribute` lookup). It now classifies the full set in a single iteration with a `match` on the key. At typical 30-attribute HTTP spans the saving is ~13x fewer key comparisons per span. The parent walk for `code.*` no longer re-scans the leaf span attributes, since classification already produced them.

### Security

- **Compressed body wire-cap on OTLP/HTTP.** A new `RequestBodyLimitLayer` is layered as the outermost middleware on `/v1/traces`, so the request flow is now `RequestBodyLimit (compressed wire bytes) -> RequestDecompression -> DefaultBodyLimit (decompressed bytes via the Bytes extractor) -> handler`. With `Content-Length` set, compressed bodies above `max_payload_size` get a clean 413 before any decompression CPU is spent. Closes the amplification path where a relaxed `max_payload_size` could let an attacker burn ~100 ms of decompress CPU per request with a near-pathological compression ratio.
- **Drop `code.*` span attributes containing control characters.** ANSI escapes, NULs, newlines and other ASCII control bytes inside `code_function`, `code_filepath` or `code_namespace` are now silently dropped at the canonical `sanitize_span_event` boundary, mirroring the existing posture for `cloud.region`. Closes a defense-in-depth gap where attacker-controlled ancestor spans could feed control bytes into `code.*` (now read via the parent walk) and surface them in TUI/CLI output or tracing logs.

### Documentation

- **OTLP/HTTP gzip support documented** in `docs/INSTRUMENTATION.md` and `docs/FR/INSTRUMENTATION-FR.md` under "Production: via OpenTelemetry Collector". Notes that perf-sentinel accepts gzip natively, no `compression: none` workaround required, and that the decompressed body still respects `[daemon] max_payload_size`.

## [0.5.4]

Interactive HTML report shipping in CI. The PR sticky comment can now link to a per-PR dashboard deployed to GitHub Pages, GitLab Pages or Jenkins HTML Publisher, with companion templates for the trunk baseline and the per-PR cleanup. Also bundles the CLI parity round (terminal-safe rendering, enriched `diff` output, quality-gate rules surfaced in the dashboard, Correlations panel in `inspect`, trim notice on `report`) and the `INTEGRATION.md` split into `INSTRUMENTATION.md` + `CI.md`.

### Added

- Optional interactive HTML report linked from the PR sticky comment when GitHub Pages is enabled on the consumer repository. The report is the single-file HTML dashboard produced by `perf-sentinel report` (Findings, Explain, pg_stat, Diff, Correlations, GreenOps) deployed to `gh-pages/perf-sentinel-reports/pr-<N>/index.html`. The sticky comment gets a `📊 Interactive report (Diff view)` link that opens on `#diff` when a baseline is available, `#findings` otherwise.
- Two companion templates under `docs/ci-templates/`: `github-actions-baseline.yml` (refreshes `gh-pages/perf-sentinel-reports/baseline.json` on every push to `main`) and `github-actions-report-cleanup.yml` (removes `pr-<N>/` on PR close). Both opt-in, documented in `docs/INTEGRATION.md` "Interactive report via GitHub Pages". Deploy path uses plain `git push` against `gh-pages` with the built-in `GITHUB_TOKEN`, no third-party deploy action.
- Reference implementations of the baseline and cleanup workflows in the perf-sentinel repo itself at `.github/workflows/perf-sentinel-baseline.yml` and `.github/workflows/perf-sentinel-report-cleanup.yml`. The baseline dogfoods the tool on `tests/fixtures/n_plus_one_sql.json`. Per-PR report generation in `ci.yml` is deferred, so the cleanup is currently a no-op on this repo.
- **`inspect` accepts a Report JSON file in addition to event arrays.** Auto-detect on the top-level shape (`[` for events, `{` for a Report). Lets you pipe a daemon `/api/export/report` snapshot or an `analyze --format json` artifact straight into the TUI for cross-panel navigation, without re-running the full pipeline.
- **Correlations panel in `inspect` TUI.** Layout shifts from three panels to four: Traces, Findings, Correlations, Detail. Exposes the cross-trace pairs that the HTML dashboard's Correlations tab already shows, navigable with `Tab` / `j` / `k`. Closes the last significant terminal-vs-dashboard gap.
- **Trim notice on `report --max-traces-embedded`.** When the dashboard caps embedded traces under the 5 MB JSON budget, the CLI now logs `Embedded N of M traces in the dashboard (K trimmed for file size). Use --max-traces-embedded <higher> to keep more.` Previously the trim was silent on the CLI and only visible as a banner inside the rendered HTML.
- **Quality-gate rules and confidence surfaced in the HTML dashboard.** Findings tab shows the rule that matched the gate alongside a confidence badge per finding (matches the SARIF / CI output).
- **Trivy image scan and Gitleaks secret scan in CI.** Trivy gates the multi-arch image build, Gitleaks audits every push to a tracked branch.

### Fixed

- **Terminal output sanitized through a new `text_safety` module.** ANSI escapes, OSC 8 hyperlinks, NULs and other C0/C1 control bytes from user-controlled fields (templates, services, suggestions, span names) are replaced with `?` before printing. Closes a CLI-side terminal injection vector. Shared between `render` (analyze, diff, top-findings) and `explain`.
- **`diff` text output enriched.** Prints the full template, occurrence count, severity and services per regression. Previously truncated, requiring a dashboard handoff to see what changed.
- **`analyze` writes to stdout when no `--output` flag is given.** The CI template snippets that piped via `--output` referenced a flag the binary did not surface for that subcommand. Templates now use a redirect.

### Tests

- New TUI tests around the four-panel layout, the empty-state hint when correlations are unavailable, and the `inspect` Report-mode auto-detection.

### Docs

- CI templates (`docs/ci-templates/github-actions.yml`, `gitlab-ci.yml`, `jenkinsfile.groovy`) and the "Quality-gate philosophy" section of `docs/INTEGRATION.md` + `docs/FR/INTEGRATION-FR.md` now implement and document the PR-blocks / trunk-informational split. On a pull request the gate blocks as before. On a push to the default branch the SARIF is still uploaded and the PR comment / Code Quality / Warnings NG surfaces still fire, but the build stays green. Closes the "main stays red after merge" anti-pattern reported on Teams.
- GitLab Pages and Jenkins HTML Publisher paths documented in `docs/INTEGRATION.md` + `docs/FR/INTEGRATION-FR.md` "Interactive report via GitLab Pages" and "Interactive report via Jenkins HTML Publisher" sections. GitLab path ships two opt-in blocks in `docs/ci-templates/gitlab-ci.yml`: `perf-sentinel-pages-simple` for Free tier (single default-branch deployment) and `perf-sentinel-pages` for Premium/Ultimate (per-MR `path_prefix` deployments, 30-day `expire_in` backstop, immediate cleanup on MR close/merge). Jenkins path documents the HTML Publisher plugin with a stable sidebar URL at `${BUILD_URL}perf-sentinel/`. Corrected two latent bugs in the existing `perf-sentinel-pages` block (`environment.url` doubling on MR deployments, baseline fetch URL stripped via `${CI_PAGES_URL%/mr-[0-9]*}` instead of appending `PAGES_PATH_PREFIX` twice).
- GitHub Actions template hardening: added tier-gating note (Pages on GitHub Free requires a public repo), `**Fork PR limitations**` subsection explaining the `continue-on-error: true` tolerance on the sticky comment step and the `pull_request_target` + `workflow_run` upgrade path, and a `**Concurrency trade-off**` subsection flagging that the workflow-level `gh-pages-deploy` group also serializes non-Pages runs. All five gh-pages-touching workflows (main consumer template, baseline + cleanup consumer templates, and both dogfood counterparts) now share the `concurrency.group: gh-pages-deploy` lock and a `workflow_dispatch:` trigger for manual re-runs. Action pins refreshed to latest stable (`marocchino/sticky-pull-request-comment` v2.9.0 → v3.0.4, `taiki-e/install-action` v2.75.18 → v2.75.21, `github/codeql-action/upload-sarif` in the consumer template v3.26.6 → v4.35.2 aligned with the dogfood workflows).
- Jenkinsfile template hardening: documented six trap conditions in `docs/INTEGRATION.md` + `docs/FR/INTEGRATION-FR.md` "Interactive report via Jenkins HTML Publisher" and `docs/ci-templates/jenkinsfile.groovy`. Added a `**Jenkins pipeline requirements**` subsection (MultiBranch Pipeline + branch-source plugin, Linux agent) and a `**Configuring Jenkins to render the interactive report**` subsection covering the default CSP block on inline CSS/JS (Resource Root URL recommended for Jenkins 2.200+, `hudson.model.DirectoryBrowserSupport.CSP` system property as fallback). Template now sets `agent { label 'linux' }`, an `options { timeout(30 min) + disableConcurrentBuilds() }` block, `enabledForFailure: true` on `recordIssues` so the SARIF panel renders on failed PR builds, plus header-comment minimum versions for Warnings Next Generation (>= 9.11.0 for the SARIF tool) and HTML Publisher (>= 1.10 for CSP compatibility).
- New `**Sampling and detection accuracy**` subsection under "Production: via OpenTelemetry Collector" → "Sampling and filtering" in `docs/INTEGRATION.md` + `docs/FR/INTEGRATION-FR.md`. Documents that head-based sampling silently breaks count-based detections (N+1, chatty service, fanout, pool saturation, serialized parallelizable calls), that within a kept trace all spans are preserved (per-trace not per-span), that tail-based sampling stays compatible because the policies that surface anti-patterns are the same as those used for incident review, that CI runs should keep 100% of traces, and that `pg-stat` mode is sampling-immune because `pg_stat_statements` aggregates counters server-side regardless of what the application tracer captured. Replaces the previous one-line note that understated the issue.
- `docs/INTEGRATION.md` (1685 lines, 22 H2 sections) split into three balanced docs (~530-650 lines each), strict EN+FR parity. New `docs/INSTRUMENTATION.md` + `docs/FR/INSTRUMENTATION-FR.md` collect Kubernetes deployment, cloud provider integrations, OTel Collector production setup with sampling guidance, required span attributes and per-language instrumentation (Java, Quarkus, .NET, Rust). New `docs/CI.md` + `docs/FR/CI-FR.md` collect CI mode, the GitHub / GitLab / Jenkins recipes with their interactive HTML report subsections, the quality-gate philosophy, the SARIF surfaces overview and the `diff` subcommand for PR regression detection. INTEGRATION.md keeps the topology overview, the four quick starts, the input/output formats, the daemon HTTP API, advanced carbon scoring, Tempo and Jaeger ingestion and troubleshooting. Cross-doc nav blocks added to all three EN docs and all three FR docs. Anchor `#kubernetes-deployment` follow-through fixed in `docs/HELM-DEPLOYMENT.md` + `docs/FR/HELM-DEPLOYMENT-FR.md` (now points to `INSTRUMENTATION.md`). Per-language ref split in `docs/RUNBOOK.md` + `docs/FR/RUNBOOK-FR.md` and OTLP-guides ref split in `docs/design/00-INDEX.md` + `docs/FR/design/00-INDEX-FR.md`.
- Stale-ref sweep after the INTEGRATION.md split. README + README-FR per-language pointer redirected to `INSTRUMENTATION.md` / `INSTRUMENTATION-FR.md` and CI recipes pointer added to `CI.md` / `CI-FR.md`. CONTRIBUTING.md `PERF_SENTINEL_VERSION` bump-target list redirected to `CI.md`. `charts/perf-sentinel/values.yaml` raw-manifest reference redirected to `INSTRUMENTATION.md`. The five CI templates (`github-actions.yml`, `github-actions-baseline.yml`, `github-actions-report-cleanup.yml`, `gitlab-ci.yml`, `jenkinsfile.groovy`) now point at `CI.md` for the "Interactive report via …" and "Fork PR limitations" subsection callouts and at `CI.md` / `CI-FR.md` for the full integration guide and quality-gate philosophy. `docs/design/05-GREENOPS-AND-CARBON.md` + FR mirror tightened to point at the "Finding confidence field" subsection of `INTEGRATION.md`. Pre-existing relative-path depth bug fixed in `docs/FR/INTEGRATION-FR.md` (lines 233 and 262) and `docs/FR/INSTRUMENTATION-FR.md` (line 234), where `(../examples/...)` was resolving to a non-existent `docs/examples/` from the FR depth, now correctly `(../../examples/...)`. Sticky-comment example link `[Interactive report (Diff view)](...)` and FR equivalent rewritten as `**Interactive report (Diff view)** → \`https://<owner>.github.io/<repo>/perf-sentinel-reports/pr-<N>/index.html#diff\`` so the rendered doc no longer shows a broken clickable link to the literal string `...`.
- Tables of contents added to `docs/LIMITATIONS.md` (27 sections), `docs/INTEGRATION.md` (15), `docs/HELM-DEPLOYMENT.md` (13), `docs/QUERY-API.md` (6) and `docs/CONFIGURATION.md` (6), with strict FR parity in `docs/FR/`. Each ToC is a `## Contents` (EN) or `## Sommaire` (FR) bold-span block listing every H2 with a one-line description. Brings the docs in line with the existing `RUNBOOK.md`, `CI.md` and `INSTRUMENTATION.md` pattern. `ARCHITECTURE.md` left as-is (5 H2s, 172 lines, scannable without ToC).

## [0.5.3]

Authenticated Prometheus support on the daemon's three outbound scrapers (cloud_energy, pg_stat, scaphandre). Unlocks Grafana Cloud, Grafana Mimir and any Prometheus sitting behind bearer/basic auth without a local port-forward. Env vars take precedence over the config / flag value, matching the existing `PERF_SENTINEL_EMAPS_TOKEN` precedence for Electricity Maps.

### Added

- Optional `auth_header` on `[green.cloud]` and `[green.scaphandre]` TOML sections, plus `--auth-header` on the `pg-stat` subcommand and `--pg-stat-auth-header` on the `report` subcommand. The environment variables `PERF_SENTINEL_CLOUD_AUTH_HEADER`, `PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER`, and `PERF_SENTINEL_PGSTAT_AUTH_HEADER` take precedence over the config / flag value. A startup warning nudges toward the env var when the value is supplied via the config file or flag. The parsed value is marked `sensitive`, hyper redacts it from debug output and HTTP/2 HPACK tables, and each config struct ships a manual `Debug` impl that redacts the field. A malformed header disables the scraper subsystem at startup with a `tracing::error!` rather than retrying silently. See `docs/INTEGRATION.md` "Authenticated Prometheus endpoint" subsections.
- Config-load validation for `auth_header`. Malformed values fail at TOML load time with a clear `[green.scaphandre] auth_header: ...` or `[green.cloud] auth_header: ...` error instead of silently disabling the scraper at spawn.

### Changed

- **BREAKING** (`perf-sentinel-core`, pre-1.0 so minor-bump allowed): `http_client::fetch_get` and `ingest::pg_stat::fetch_from_prometheus` gain an `auth` / `auth_header` parameter. `score::cloud_energy::config::CloudEnergyConfig` and `score::scaphandre::config::ScaphandreConfig` gain an `auth_header: Option<String>` field and drop their derived `Debug` impl in favor of a manual redacting one. External consumers calling these directly must pass `None` (or add `auth_header: None` to struct literals) for the current behaviour.
- `Config::validate_scaphandre` extracts the `process_map` validation into a sibling `validate_scaphandre_process_map`, matching the existing `validate_cloud_endpoint` / `validate_cloud_services` split for cloud_energy. Drops the function from SonarCloud `rust:S3776` cognitive complexity 16 to ~7.

### Fixed

- `ingest::auth_header` is now compiled whenever any of the `daemon`, `tempo`, or `jaeger-query` features is active (previously only the latter two). Prevents a bare `cargo publish -p perf-sentinel-core` from failing to resolve `AuthHeader` references introduced in the config validation path.
- **Cleartext HTTP plus auth header emits `tracing::warn!`.** When a configured scraper endpoint starts with `http://` and an `auth_header` is set, a warning fires once at startup so a typo is caught before the credential traverses the network in the clear.

### Tests

- New on-wire tests for `http_client::fetch_get`, `pg_stat::fetch_from_prometheus`, the cloud scraper and the scaphandre scraper that spawn a `TcpListener`, capture the request bytes and assert the `Authorization` header lands on the socket. Shared harness extracted into `test_helpers::spawn_capture_server`, consolidating ~110 lines of duplicated listener + mpsc scaffolding.
- Shared `assert_debug_redacts_secret!` macro unifies the three config-Debug regression tests (cloud_energy, scaphandre, electricity_maps). Catches any accidental `#[derive(Debug)]` re-introduction on the config structs.

### Docs

- New "Authenticated Prometheus endpoint" subsections under the Scaphandre, cloud-native energy and pg_stat sections of `docs/INTEGRATION.md` plus French parity in `docs/FR/INTEGRATION-FR.md`.

## [0.5.2]

Two new trace ingestion surfaces. `perf-sentinel jaeger-query` queries any backend that speaks the Jaeger query HTTP API, covering Jaeger upstream and Victoria Traces in one subcommand. Both `jaeger-query` and the existing `tempo` gain `--auth-header` (curl-style `"Name: Value"`) and `--auth-header-env NAME` (env-var read, no `ps` exposure) so backends sitting behind an auth proxy no longer require a local port-forward. Also consolidates the Helm chart work: chart is on Artifact Hub, signed with Cosign, ships SLSA build provenance and an SPDX SBOM.

### Added

- `jaeger-query` subcommand: query any backend speaking the Jaeger query HTTP API (Jaeger upstream and Victoria Traces) and analyze the returned traces with the standard pipeline. Mirrors the existing `tempo` subcommand with the same `--endpoint / --trace-id / --service / --lookback / --max-traces / --format / --ci` flags. Single HTTP round trip per search since Jaeger returns full traces in the response (no per-trace fanout). See `docs/INTEGRATION.md` "Jaeger query API integration" for full recipes.
- `--auth-header "Name: Value"` flag on both `tempo` and `jaeger-query` subcommands. Attaches a single curl-style header to every backend request so authenticated Jaeger / Victoria Traces / Tempo deployments can be queried without a local forward. The parsed value is marked `sensitive`, hyper redacts it from debug output and HTTP/2 HPACK tables, and the subcommand never logs the value.
- `--auth-header-env NAME` on both subcommands. Reads the header line from the named environment variable instead of from `argv`, so the credential never appears in `ps` / `/proc/<pid>/cmdline`. Mutually exclusive with `--auth-header`.
- Shared `ingest::lookback` parser extracted from `tempo` and reused by `jaeger-query` so the two subcommands accept the exact same `--lookback` syntax (`1h`, `30m`, `2h30m`) without duplication.
- Helm chart for Kubernetes deployment (`charts/perf-sentinel/`), deployable as Deployment, DaemonSet or StatefulSet. See `docs/HELM-DEPLOYMENT.md` (EN) and `docs/FR/HELM-DEPLOYMENT-FR.md` (FR).
- Example values composing the chart with the upstream OTel Collector chart under `examples/helm/`.
- Helm chart publication to OCI registry at `oci://ghcr.io/robintra/charts/perf-sentinel`, signed with Cosign (keyless) and shipped with SLSA level 3 provenance.
- GitHub Actions workflow `helm-release.yml` automates chart publication on tags matching `chart-v*`.
- GitHub Actions workflow `helm-ci.yml` validates every PR touching the chart: `helm lint`, `helm template` across all three workload modes, `kubeconform` on rendered manifests, and a guard that fails PRs that modify the chart without bumping `Chart.yaml:version`.
- Dedicated chart changelog at `charts/perf-sentinel/CHANGELOG.md`.
- Helm chart is listed on Artifact Hub. `charts/perf-sentinel/artifacthub-repo.yml` ships the repository metadata and is pushed to the OCI registry under the special `artifacthub.io` tag on every release. See `docs/HELM-DEPLOYMENT.md` for the registration flow.
- SPDX SBOM generated and attested on every chart release via `anchore/sbom-action` plus `actions/attest`, attached to the GitHub Release as `perf-sentinel-chart-<version>.spdx.json` and queryable via `gh attestation verify --predicate-type https://spdx.dev/Document/v2.3`.
- helm-ci now self-tests the version-bumped guard script via `scripts/test/check-chart-version-bumped-test.sh`.

### Fixed

- Cleartext HTTP plus auth header emits a `tracing::warn!`. When the endpoint starts with `http://` and an auth header is present, the subcommand logs a warning before sending the request so a `http://` vs `https://` typo is caught before the credential traverses the network in the clear.
- Helm chart SLSA provenance is now reliably attached to published chart releases. Migrated from `slsa-framework/slsa-github-generator` (reusable workflow) to `actions/attest-build-provenance`, which integrates with `gh release create` without diverging onto ephemeral draft releases. Users verify provenance with `gh attestation verify --repo robintra/perf-sentinel`. See `docs/HELM-DEPLOYMENT.md` for the updated recipe.

### Changed

- **BREAKING** (`perf-sentinel-core`, pre-1.0 so minor-bump allowed): the public ingest functions `tempo::search_traces`, `tempo::fetch_trace`, `tempo::ingest_from_tempo`, `jaeger_query::search_and_fetch_traces`, `jaeger_query::fetch_trace`, and `jaeger_query::ingest_from_jaeger_query` gain an `auth: Option<&AuthHeader>` (or `auth_header: Option<&str>` for the top-level fns) parameter. External consumers calling these directly must pass `None` for the current behaviour.
- `scripts/check-chart-version-bumped.sh` now also asserts that a matching `## [NEW_VERSION]` section exists in `charts/perf-sentinel/CHANGELOG.md` on HEAD and was not present on the PR base. Catches chart version bumps that forget the changelog entry.
- `docs/HELM-DEPLOYMENT.md` (and its French parity) reorganized with a "Software supply chain" top-level section grouping Cosign, SLSA build provenance, SBOM, and an OCI-based `gh attestation verify oci://...` recipe alongside the tarball-based one.

### Security

- **Strict `AuthHeader` validation.** 8 KiB input cap, non-empty name and value, RFC 7230 character restrictions (no CR/LF, no non-visible ASCII), and a blocklist of framing and authority headers (`Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`, `Proxy-Connection`) to keep a malicious environment-variable expansion from hijacking the request shape.

### Tests

- 57 new tests across the ingest layer. 16 on `AuthHeader` parsing (bearer / custom / trim / forbidden names / empty name / empty value / CRLF / oversized input / case-insensitive blocklist / internal-tab preservation / Debug redaction), 8 on the shared lookback parser (overflow on multiplication, overflow on addition, happy paths), 4 on the shared URL helpers, wire-level asserts that the `Authorization` header actually lands on the socket for both `jaeger-query` (single connection) and `tempo` (dual connection through the parallel fetch fanout), plus the remaining coverage for error paths (malformed JSON, 404, 500, timeout, invalid endpoint, credentials rejected, missing service or trace id).

### Documentation

- **`docs/LIMITATIONS.md` plus French parity**: the "Query-API subcommands" section rewritten. Two new sub-sections describe `--auth-header` usage (with the `ps` visibility note and one-header-per-invocation constraint) and the `--auth-header-env` alternative. Validation rules (8 KiB cap, non-empty name and value, forbidden header names, RFC 7230 restrictions) are listed for transparency.

## [0.5.1]

HTML dashboard polish pass plus a browser-level regression suite. Focused on rough edges users reported against 0.5.0: deep-link hashes that only applied on fresh page loads, indistinguishable browser tabs when juggling several reports, Correlations rows that were silently inert, accessibility gaps on tabs and chips. Also surfaces the previously hardcoded `pg_stat` top-N as a `--pg-stat-top` flag on `report`, and adds a `prefers-color-scheme` auto mode to the theme toggle.

### Added

- **HTML dashboard: Copy link button.** New toolbar button sits next to Export CSV on Findings, pg_stat, Diff and Correlations tabs. Copies `location.href` (with the current deep-link hash) to the clipboard. Async Clipboard API with a textarea+execCommand fallback for `file://` origins and older browsers. Flashes `Copied` for 1.5 seconds on success.
- **HTML dashboard: dynamic page title.** Browser tab title now shows the input filename as `perf-sentinel: <filename>` so multi-tab browsing of several reports is distinguishable. Path components are stripped, HTML-sensitive characters are escaped. Stdin input falls back to the static default.
- **HTML dashboard: auto theme mode.** Theme toggle gains a tri-state (`auto` / `dark` / `light`) with `auto` as the new default. Auto follows the OS `prefers-color-scheme` media query and updates live when the OS setting changes. Forced modes stay available via click-to-cycle. Legacy sessions that stored `dark` or `light` keep those forced values.
- **HTML dashboard: resolved diff findings clickable.** Resolved rows in the Diff tab now route clicks through a shared empty-state helper explaining that the trace lives in the baseline report, not the current run. Replaces the previous silently-inert behavior.
- **HTML dashboard: correlation rows clickable.** When the daemon-produced `sample_trace_id` field is present on a correlation, the row becomes clickable and opens Explain with the captured trace. Falls back to the shared empty-state when the trace did not make it into `embedded_traces`.
- **HTML dashboard: Powered by perf-sentinel credit** centered under the footer, linking to the GitHub repo. `target="_blank"` plus `rel="noopener noreferrer"` to block reverse-tabnabbing. CSP-compatible (top-level navigation is unaffected by `default-src 'none'`).
- **Daemon: sample trace id on each correlation.** `CrossTraceCorrelation` carries an `Option<String>` trace id of the most recent target-side finding that completed each pair. Serialized only when populated so v0.5.0 baselines stay byte-identical and deserialization is backward-compatible via `serde(default)`.
- **CLI: `--pg-stat-top N` on `report`.** Overrides the hardcoded top-10 cap per pg_stat ranking. `u32` range validator rejects `0` at the clap level, a post-parse guard rejects the flag when no `--pg-stat` or `--pg-stat-prometheus` source is supplied. Note: with `--pg-stat-prometheus` the value also widens the upstream scrape since the scrape size and the ranking cut share the same bound, large values may hit exporter query-complexity caps. The stand-alone `pg-stat` subcommand stays untouched.
- **Browser regression tests.** New `crates/sentinel-cli/tests/browser/` Playwright suite covers tab-switch keyboard nav, Findings → Explain cross-nav, Explain → pg_stat deep-link, `/` search filter, CSV export blob content, deep-link hash on fresh load, hashchange on in-page paste, Copy link clipboard write, ARIA tablist semantics. Ten specs in one Chromium project. New `browser-tests` CI job runs in parallel with `check`, pins `actions/setup-node@v6.4.0` and `actions/upload-artifact@v7.0.1` by commit SHA.

### Changed

- **HTML dashboard: WAI-ARIA-compliant tabs and chips.** Tab strip is now a proper `role="tablist"` with `role="tab"` buttons, `aria-selected`, `aria-controls`, and tabindex rotation. Each panel carries `role="tabpanel"` + `aria-labelledby`. Arrow keys (Left/Right/Up/Down), Home, and End move focus between tabs; Space/Enter activates the focused tab. The existing g-prefixed shortcuts keep working. pg_stat ranking chips form a radiogroup with `aria-checked`; Findings severity chips split into a radiogroup (severity) and a group of toggles with `aria-pressed` (service).
- **HTML template: Rust twin of `csvEscape` retired.** The hand-written spec function in `html.rs` was a drift hazard against the JS helper that actually runs in the browser. CSV escape correctness is now exercised exclusively by the new Playwright suite against the real JS in a real browser.

### Fixed

- **HTML dashboard: deep-link hashes apply on in-page paste.** Pasting a new `#hash` into the URL bar of an already-loaded dashboard now restores the view. Before 0.5.1 the hash was read only once at boot so interactive paste was a silent no-op. A `_lastWrittenHash` guard prevents the new `hashchange` listener from re-entering `applyHash` on its own writes.
- **Dashboard sink: `input_label` sanitization, Content-Security-Policy and placeholder ordering.** The `<title>{{PAGE_TITLE}}</title>` and the embedded `input_label` both now pass through `sanitize_input_label`, which strips Unicode `Cc` (control) and `Cf` (format: BiDi overrides, LSEP, BOM) characters before the HTML-escape pass. A `<meta http-equiv="Content-Security-Policy">` tag locks the dashboard to `default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'`, closing the class of defects where a future sink change might leak an external reference. The placeholder injection order in `html::inject` now goes title first, JSON second, so a malicious title cannot shift the JSON payload boundary.
- **`--pg-stat-top` range-guarded at `1..=10_000` at the clap level.** Without the cap, a user passing `--pg-stat-top 2_000_000_000` would OOM the process on ingest. On the Prometheus path (`--pg-stat-prometheus`) the scrape size widens to `max(top_n, 200)` so the secondary rankings (by_calls / by_mean_time / by_io_blocks) still see the full hot-spot distribution instead of the top-N by `seconds_total` only.
- **Daemon correlator: `sample_trace_id` length cap and identical-write short-circuit.** Truncated at 128 bytes on a UTF-8 character boundary so a pathological emitter cannot blow up the `PairState` memory budget. The update path short-circuits when the incoming `trace_id` already matches the recorded value, avoiding a `to_string()` allocation on the common case of a single trace emitting multiple findings in one ingest batch.
- **Browser test suite: `http-server` binds `127.0.0.1` explicitly.** Without `-a 127.0.0.1`, `http-server` defaults to `0.0.0.0` and would expose the dashboard fixture to every interface on the CI runner for the duration of the suite. The browser README now documents the constraint in both EN and FR.
- **Dashboard demo pipeline hardening.** The demo's `cycleTo` throws when the theme cycle never reaches the requested target, `themeFor` throws on a project name that does not end with `-dark` or `-light` (instead of silently writing a light still under a dark name), and the `case` in `build-gif.sh` matches the exact Playwright output directory (not a loose glob that could catch the stills projects).
- **All eleven SonarCloud findings cleared.** Cognitive complexity on `Correlator::record_co_occurrences` drops below the 15-point cap by extracting `update_sample_trace_id` and `truncate_to_utf8_boundary`. Six `tabindex="0"` attributes drop from `role="tabpanel"` containers (WAI-ARIA APG says a tabpanel with focusable children should not be focusable itself). `Array.indexOf >= 0` becomes `Array.includes`, `parentNode.removeChild` becomes `childNode.remove`, `window.addEventListener` becomes `globalThis.addEventListener`, and an explicit `return 0` lands in the `convert_webm` bash helper.

### Security

- **`rustls-webpki` bumped to 0.103.13 against RUSTSEC-2026-0104** (reachable panic in certificate revocation list parsing). `rustls-webpki` lands transitively through `rustls 0.23 -> tokio-rustls + hyper-rustls` used by the daemon's TLS stack. Patch-level bump, semver-compatible, `cargo audit` green.

### Documentation

- **`docs/img/report/` grows a dashboard tour**: `dashboard_dark.gif` and `dashboard_light.gif` (one Playwright tour per primary theme, generated via `npm run demo` in the browser-tests dir) plus per-tab still frames in both themes (`findings`, `explain`, `pg-stat`, `diff`, `correlations`, `cheatsheet`). The root README and README-FR serve the correct variant via a `<picture>` tag with `prefers-color-scheme`, matching the pattern already used for the logo and architecture diagram. The browser-tests README documents the regen cadence so casual doc tweaks do not balloon the repo with fresh blobs.

## [0.5.0]

Bundles the full HTML dashboard polish pass (CSV export, deep-link hash, session-scoped persistence, `?` cheatsheet modal, vim-style tab shortcuts, extended Esc ladder, Findings pagination) plus the security, performance and SonarCloud review follow-ups on top.

### Added

- **`report` subcommand: single-file HTML dashboard.** `perf-sentinel report --input traces.json --output report.html` produces a self-contained HTML file with six possible tabs (Findings, Explain, GreenOps unconditionally, pg_stat / Diff / Correlations when the relevant data is present). Works offline from `file://`, zero CDN / fonts / external resources, no build step. The sink embeds findings-only traces and trims to a ~5 MB target keeping the highest-IIS traces first. `--input` accepts a trace file, a pre-computed `Report` JSON, or `-` for stdin (auto-detects array-of-events vs Report object, BOM-tolerant). User-controlled data is injected inside a `<script type="application/json">` block, rendered exclusively via `Element.textContent`, and guarded by a build-time `no_forbidden_apis_in_template` test that rejects `innerHTML`, `insertAdjacentHTML`, `outerHTML`, `eval`, `new Function`, `DOMParser`, `createContextualFragment` and `setAttribute("on*")`. The `</` substring is escaped to `<\/` in the serialized payload so a user value cannot close the script block early. Implemented in `crates/sentinel-core/src/report/html.rs` and `html_template.html`, full design rationale in `docs/design/07-CLI-CONFIG-RELEASE.md`.
- **`--pg-stat FILE` and `--pg-stat-prometheus URL` flags on `report`.** Embed a PostgreSQL `pg_stat_statements` hotspot tab alongside the trace findings, with four rankings driven by a sub-switcher in the dashboard (by_total_time, by_calls, by_mean_time, by_io_blocks). The Prometheus path reuses the one-shot `fetch_from_prometheus` helper. The two flags are mutually exclusive at the clap level.
- **`--before baseline.json` flag on `report`.** Compare the current run against a baseline Report and light up a Diff tab with four sections: new findings (clickable, open Explain), resolved findings, severity changes and per-endpoint I/O op deltas. Identity matching is `(finding_type, service, source_endpoint, pattern.template)`.
- **Daemon endpoint `GET /api/export/report`.** Returns the daemon's current state as a `Report` JSON snapshot, shape-identical to `analyze --format json`. Pipe `curl -s http://daemon:4318/api/export/report | perf-sentinel report --input -` for a browser dashboard of live production state. Cold-start behavior: HTTP 503 with `{"error": "daemon has not yet processed any events"}` until the first OTLP batch has been processed, so a fresh daemon never renders misleading zero-counter views. The snapshot is not atomic across `findings` and `correlations`, the two collections can be one batch apart.
- **Fourth pg_stat ranking: `by_io_blocks`** (`shared_blks_hit + shared_blks_read`). Cache-pressure signal that complements `by_total_time`: flags queries that touch the most shared-buffer pages regardless of whether they were hit or miss. The stable ranking order is `[by_total_time, by_calls, by_mean_time, by_io_blocks]`, new rankings are appended and existing indices never reassign so the HTML sub-switcher and any other position-indexed consumer stay stable.
- **HTML dashboard: Export CSV per tab.** New Export CSV button on every listable tab (Findings, pg_stat, Diff, Correlations) downloads the currently filtered view as RFC 4180-escaped CSV. Templates containing commas, double quotes or newlines round-trip safely. Explain and GreenOps stay export-less by design. Formula-injection guard (OWASP CSV) prefixes an apostrophe on any cell starting with `=`, `+`, `-`, `@` or a tab so Excel and LibreOffice cannot execute hostile SQL templates on open.
- **HTML dashboard: deep-link hash.** URL fragment encodes the active tab, search term, pg_stat ranking and Findings filter chips. Sharing a URL restores the exact filtered view. Hash writes use `history.replaceState` so back/forward history stays clean. Key allowlist prevents URL-driven prototype pollution on `state`.
- **HTML dashboard: session-scoped persistence.** Theme (dark/light) and last-active pg_stat ranking now persist across refreshes within the same browser tab via `sessionStorage`. Tab-scoped on purpose to avoid `file://` origin collisions between unrelated reports.
- **HTML dashboard: native `<dialog>` cheatsheet and vim-style tab shortcuts.** Press `?` to open a WAI-ARIA-compliant dialog. New `g`-prefixed shortcuts (`g f`, `g e`, `g p`, `g d`, `g c`, `g r`) switch tabs, with autorepeat guarded. Hidden tabs are silent no-ops.
- **HTML dashboard: Esc clears filter chips.** The Esc priority ladder gains two tiers: close the cheatsheet (highest) and clear active Findings filter chips (lowest), on top of the existing search-close and back-from-Explain behavior.
- **HTML dashboard: Findings pagination.** The 500-row initial cap now reveals more via a `Show N more findings` button instead of silently truncating. Filters, search and hash apply all reset the visible count.
- **CI: release workflow version-drift guard.** A new `check-versions` job refuses to run the build / release / publish-crate / docker jobs if the pushed tag does not match `workspace.package.version` in the root `Cargo.toml` and every `crates/*/Cargo.toml`. Logic lives in `scripts/check-tag-version.sh` so it can also run locally before tagging.

### Changed

- **HTML report sink: O(N) trim-to-size.** Rewrote `trim_to_size_target` from a quadratic re-serialize-per-pop loop to a two-phase prefix-sum scan over per-trace JSON lengths. Linear in the payload now, removes the worst-case minutes-of-CPU path on pathological trace counts.
- **Daemon export endpoint: 32-bit saturation observable.** `events_processed` / `traces_analyzed` overflow on a 32-bit target now logs a `tracing::warn!` before saturating, instead of silently clamping to `usize::MAX`.
- **HTML template: prototype-pollution hardening.** Five user-controlled-key lookup maps (`indexTracesById`, service chips, `byId`, `childrenByParent`, `pgStatByTemplate`) switched from `{}` to `Object.create(null)` so a hostile `trace_id = "__proto__"` cannot corrupt the lookup chain.
- **`Report` tree now derives `Deserialize`.** Cascade across `Report`, `Analysis`, `GreenSummary`, `QualityGate`, `QualityRule`, `TopOffender`, `CarbonReport`, `CarbonEstimate`, `RegionBreakdown`, `IntensitySource` so a saved Report JSON can be fed back into `perf-sentinel report --before baseline.json` for diff mode. Three `&'static str` fields (`CarbonEstimate::model`, `CarbonEstimate::methodology`, `RegionBreakdown::status`) become `String` for serde round-trip. A module-level invariant comment in `crates/sentinel-core/src/report/mod.rs` pins the "every new field must be `Option<T>` or carry `#[serde(default)]`" rule so stored baselines keep parsing across future minor versions. `deny_unknown_fields` is deliberately not applied, the trade-off is documented.

### Fixed

- **HTML template: SonarCloud hygiene.** Full sweep of the template, addressing 41 SonarCloud findings in one pass. Native `<dialog>` replaces `role="dialog"`; cheatsheet table gets a real `<thead>`; `document.body.removeChild(a)` becomes `a.remove()`; `parseInt` becomes `Number.parseInt`; `indexOf(x) >= 0` patterns become `.includes(x)` or `.startsWith(x)`; `x != null ? x : fallback` ternaries become `x ?? fallback`; `window.sessionStorage` becomes `globalThis.sessionStorage`; the IIFE cognitive complexity drops from 28 to below 15 via boot-helper extraction.

### Documentation

- **New documentation for the v0.5.0 surface** across EN and FR mirrors. `docs/INTEGRATION.md` gains a dedicated HTML dashboard section with the keyboard ladder, pagination and sharing semantics. `docs/CONFIGURATION.md` adds the `report`, `tempo` and `calibrate` rows to the subcommand table. `docs/QUERY-API.md` documents `GET /api/export/report` and its cold-start contract. `docs/RUNBOOK.md` adds an `/api/export/report returns 503 or an empty report` troubleshooting entry. `docs/LIMITATIONS.md` documents the CSV formula-injection guard. The design docs add the four-ranking section, the `--pg-stat-prometheus` integration paragraph, the `/api/export/report` snapshot semantics and the `report` subcommand ergonomics. Mermaid diagrams `cli-commands.mmd` and `query-api.mmd` updated to depict the new subcommands and endpoint (SVGs regenerated).
- **`SECURITY.md`** grows six HTML-dashboard-specific bullets covering `textContent`-only rendering, the `</` script-tag escape, prototype-pollution hardening via `Object.create(null)`, the CSV formula-injection guard, the deep-link hash allowlist, and offline self-contained output with zero CDN or font fetches. Supported-versions table bumped to `0.5.x`.
- **`CONTRIBUTING.md`** grows a `Release process` section pointing at `scripts/check-tag-version.sh` and listing every non-Cargo.toml file that also takes a version bump.
- **README and README-FR** gain dedicated `HTML dashboard report` and `PR regression diff` subsections with representative command-line examples.
- **`CHANGELOG.md`** introduced at the repo root in Keep a Changelog format, seeded with the `[0.5.0]` section.

## [0.4.8]

Performance release focused on the allocator used by the Linux release binaries. Swapping musl's built-in malloc for `mimalloc` on static builds closes the musl-vs-glibc throughput gap observed on v0.4.6, and then some. Same binary portability guarantees as v0.4.6 and v0.4.7 (statically linked, runs on any distribution, inside `FROM scratch` images).

### Changed

- **Musl Linux release binaries now use `mimalloc` as the global allocator** via a target-gated dependency (`[target.'cfg(target_env = "musl")'.dependencies] mimalloc = "0.1.49"`) and a `#[cfg(target_env = "musl")] #[global_allocator]` declaration in `crates/sentinel-cli/src/main.rs`. musl's built-in malloc is noticeably slower than glibc's under allocator contention, mimalloc closes that gap and overshoots it. Benchmark on aarch64 Linux over 500 iterations of the 78-event demo dataset: 2.00M events/sec on musl + mimalloc vs 1.54M on glibc vs 1.08M on musl alone (v0.4.6 baseline). RSS cost is about +21% (42 MB vs 34 MB), the expected tradeoff for a faster allocator with larger arenas. Avoids maintaining a dual glibc/musl release matrix to recover the perf delta. No user-visible change on macOS, Windows, or any future Linux-gnu target, where the system allocator stays in place.

### Documentation

- New "Allocator on musl builds" subsection in `docs/design/07-CLI-CONFIG-RELEASE.md` + FR counterpart, under the "Release profile" section: allocator choice, v0.4.6 baseline numbers, v0.4.8 mimalloc results, RSS tradeoff, and the rationale for a target-gated dependency over an opt-in feature flag.
- Per-constant and per-function doc comments trimmed in `crates/sentinel-core/src/ingest/tempo.rs` (`SEARCH_TIMEOUT`, `FETCH_TRACE_TIMEOUT`, `FETCH_CONCURRENCY`, `drain_fetch_set`) and `crates/sentinel-core/src/daemon/json_socket.rs` (ENOENT handling). The detailed rationale now lives in the design docs added in v0.4.7, code comments keep a single-line pointer to the doc section. No behavior change.

## [0.4.7]

Operator-experience release focused on Tempo ingestion. Search-then-fetch runs that previously took two and a half minutes sequentially now complete in ten to twenty seconds, long lookback windows no longer silently drop traces, a degraded Tempo surfaces as a single classified summary line instead of a wall of `ERROR`, and Ctrl-C preserves partial results. Plus an auto-rerun workflow that absorbs transient GitHub Actions infrastructure hiccups and a `FROM scratch`-image log-level fix.

### Added

- **New `auto-rerun` workflow** (`.github/workflows/auto-rerun.yml`) that automatically reruns failed jobs of the `CI` and `Release` workflows exactly once, to absorb transient GitHub Actions infrastructure hiccups (action tarball 5xx, runner DNS flap, apt mirror blip, ephemeral container registry timeout). Capped at one retry via `github.event.workflow_run.run_attempt < 2`, so a second consecutive failure stays red and requires human triage. Scoped to `CI` and `Release` only. Permissions kept minimal: the workflow floor is `contents: read`, the `rerun` job alone gets `actions: write` (required by `gh run rerun`). Logs a `::notice::` annotation on every trigger so reruns are visible in the Actions UI.
- New unit test `classify_fetch_error_buckets_every_hard_failure_variant` asserts every hard-failure `TempoError` variant (`Timeout`, `Transport`, `HttpStatus`, `ProtobufDecode`, `BodyRead`, `JsonParse`) maps to its dedicated bucket and that upstream variants (`InvalidEndpoint`, `NoTracesFound`) fall through to the intentional "other" catch-all. New integration test `ingest_from_tempo_drains_mixed_per_trace_outcomes` exercises the drain loop end-to-end with three concurrent per-trace outcomes (HTTP 500, HTTP 404, 200 with empty protobuf).

### Changed

- **`ingest_from_tempo` parallelizes `fetch_trace` calls** via `tokio::task::JoinSet` with a concurrency cap of 16 in-flight requests (internal semaphore, not user-configurable). The previous sequential loop paid the full Tempo round-trip latency per trace: at ~1.5 s per call over a WAN link, a 100-trace search-then-fetch took ~2 m 30 s end-to-end, parallelism collapses that to 10-20 s. Mirrors the pattern already used by `score::cloud_energy::scraper` for per-service Prometheus CPU queries.
- **Tempo fetch loop aggregates per-trace failures into a single categorized summary** instead of emitting one `ERROR` line per failed trace. Per-trace failures now log at `debug` (trace_id + error still captured under `RUST_LOG=sentinel_core::ingest::tempo=debug`), the loop finishes with one summary line whose severity matches the worst class seen (`warn` if only `TraceNotFound` skips occurred, `error` otherwise). Counts are bucketed by error kind (`timeout`, `transport`, `http_status`, `protobuf_decode`, `body_read`, `json_parse`, `task_panic`) so downstream tooling (Loki, CloudWatch) can alert on the right signal.
- **Tempo fetch loop handles Ctrl-C cleanly** via `tokio::signal::ctrl_c()` polled alongside `JoinSet::join_next` in a `tokio::select!`. On interrupt `set.abort_all()` flags every in-flight task, already-completed traces are preserved, and an explicit `warn` line surfaces the partial-result state. A new `TempoError::Interrupted` variant disambiguates operator abort from the generic `NoTracesFound`, so CI quality-gate paths can treat the two at different severities.
- **Tempo HTTP timeout split between search and single-trace fetch**. The single 5 s `REQUEST_TIMEOUT` covering every Tempo API call becomes `SEARCH_TIMEOUT = 5 s` for `/api/search` (fail fast on a broken endpoint) and `FETCH_TRACE_TIMEOUT = 30 s` for `/api/traces/{id}` (trace bodies can legitimately be many MiB on a wide fan-out request). On a production-scale run with 24 h lookback the old 5 s cap was dropping tens of traces per 100-trace batch with "request timed out". Thirty seconds matches the Grafana Tempo datasource default.

### Fixed

- **`run_json_socket` demotes "parent directory missing" to info level** instead of `error`. In minimal container images (`FROM scratch`, distroless static) the parent directory of the Unix NDJSON socket may not exist, and the Unix socket is not the canonical ingestion route in those deployments anyway (OTLP gRPC/HTTP is). The `ErrorKind::NotFound` path is now an actionable info log explaining how to enable the feature, all other bind failures (permission denied, stale socket, address-in-use) keep surfacing as errors. `crates/sentinel-core/src/daemon/json_socket.rs`.
- **`TempoError::HttpStatus` now includes the failing URL** (redacted via `http_client::redact_endpoint` to strip embedded credentials). A 404 on `/api/search` previously surfaced as the opaque "Tempo returned HTTP 404", the message now carries the queried URL, immediately revealing common misconfigurations: a URL pointing at Grafana instead of Tempo, a missing reverse-proxy path prefix, or a daemon pointing at `tempo-querier` when only `tempo-query-frontend` exposes the HTTP query API on a microservices deployment. Variant changed from tuple to struct form (`HttpStatus { status: u16, url: String }`).

### Documentation

- `docs/INTEGRATION.md` + FR counterpart: new "Tempo in microservices mode (`tempo-distributed`)" subsection documenting the `tempo-query-frontend` vs `tempo-querier` distinction that causes `/api/search` to 404 when `--endpoint` points at the wrong component.
- `docs/LIMITATIONS.md` + FR counterpart: Tempo ingestion limitations rewritten for parallel fetch (cap 16, not user-configurable), split timeouts (5 s search, 30 s fetch) and Ctrl-C partial-result behavior. The obsolete "sequential fetching" bullet removed.
- `docs/design/06-INGESTION-AND-DAEMON.md` + FR counterpart: new "Tempo ingestion" design section covering the parallel-fetch rationale, the timeout split and the select-based Ctrl-C handling, filling a gap open since Tempo became a supported ingest source in v0.3.1.
- `docs/RUNBOOK.md` + FR counterpart: two new troubleshooting entries, "Daemon running but not reachable from clients" (bind-address-in-container, `--network host`, port-mapping, firewall triage) and "`perf-sentinel tempo` returns 404 or times out" (endpoint misconfiguration and capacity-driven timeouts).

## [0.4.6]

Container and Linux-release hardening release. The official Docker image and the published Linux binaries now work out of the box on any distribution (they used to fail silently on anything other than the exact glibc version of the CI runner, glibc 2.39), and the daemon is reachable from the host when run in a container. No library or detection-logic changes, all fixes are in the deployment path.

### Added

- **`--listen-address`, `--listen-port-http` and `--listen-port-grpc` flags on `perf-sentinel watch`**. Override the corresponding `[daemon]` config keys without a config file, primarily so container and Kubernetes deployments can bind `0.0.0.0` from the command line while `127.0.0.1` stays the secure default for local use. CLI overrides are applied after `load_config` and the full `Config::validate` pass is re-run, so `validate_listen_addr` still emits the non-loopback security warning. `crates/sentinel-cli/src/main.rs`.
- **`musl-smoke` CI job** in `.github/workflows/ci.yml` builds the release profile against `x86_64-unknown-linux-musl` on every PR, asserts the produced binary is fully static via `file` and runs `--version` to catch runtime-init regressions. Surfaces dependencies that break musl (new C-FFI crate, `ring` toolchain change, missing `musl-tools`) at PR time instead of tag-push time. `aarch64-unknown-linux-musl` is not duplicated since `cross 0.2.5` is pinned in `release.yml` and the amd64 smoke catches dependency-level regressions.
- Two new e2e tests in `crates/sentinel-cli/tests/e2e.rs`: `cli_watch_help_documents_listen_address_override` (the three new flags appear in `watch --help`) and `cli_watch_listen_address_override_starts_cleanly` (the daemon spawns on non-default ports 14317/14318 and stays alive past 500 ms).

### Changed

- **`Dockerfile` splits ENTRYPOINT from CMD**: `ENTRYPOINT ["/perf-sentinel"]` + `CMD ["watch"]` instead of the combined `ENTRYPOINT ["/perf-sentinel", "watch"]`. Users can now override the subcommand cleanly (`docker run image analyze ...`, `docker run image query ...`) without `--entrypoint`. Also corrects a latent bug in the three `examples/docker-compose-*.yml` files passing `command: ["watch", ...]`, which the old ENTRYPOINT resolved to a duplicate `/perf-sentinel watch watch ...`.
- **Linux release binaries now target musl** (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) instead of glibc. Binaries are fully statically linked and run on any distribution (Alpine, Debian, RHEL, any Ubuntu) regardless of the host glibc version, and inside `FROM scratch` images. Previously the `ubuntu-latest` runner's glibc (2.39) was baked into every release artifact, so the binaries refused to start on Debian bookworm (glibc 2.36), Ubuntu 22.04 (2.35), CentOS Stream 9 and inside the official `FROM scratch` Docker image itself. `release.yml` installs `musl-tools` on the native amd64 build (needed by `ring`'s `build.rs`), the aarch64 path keeps `cross 0.2.5` which ships its own musl toolchain. Artifact names unchanged. Benchmark on aarch64: 1.08M events/sec under musl vs 1.47M under glibc (both well above the documented 100k events/sec target), RSS effectively identical.

### Fixed

- **Docker quickstart in `README.md` and `README-FR.md` now produces a reachable daemon**. Previously `docker run --rm -p 4317:4317 -p 4318:4318 ghcr.io/robintra/perf-sentinel:latest` started the daemon bound to `127.0.0.1` inside the container: the mapped ports appeared open on the host but any connection was refused at the app level. The quickstart now reads `docker run ... watch --listen-address 0.0.0.0` with a paragraph explaining the default bind and the reverse-proxy / NetworkPolicy recommendation for real deployments.
- **CI and release workflows install the cross-compile target on the toolchain that actually runs**. `rust-toolchain.toml` pins the Rust version, but the `dtolnay/rust-toolchain` action with `targets:` was adding the requested target to the `stable` channel, leaving the pinned toolchain without musl support and failing every Linux musl build with `error[E0463]: can't find crate for core`. The musl-smoke job and the release workflow now add the target to the pinned toolchain via an explicit `rustup target add` step.
- **`musl-smoke` static-link verification accepts both phrasings GNU `file` uses** for fully static Rust binaries. Rust's musl target emits a Position Independent Executable that modern `file` on Ubuntu 24.04 reports as "static-pie linked", while older `file` (Debian bookworm and earlier) reports "statically linked". The original grep matched only the second wording, producing a false positive on modern runners. The check is now a regex alternation plus an explicit negative assertion against "dynamically linked" to catch any future regression that reintroduces a glibc dependency.

### Security

- **Official Docker image attack surface reduced**. The `FROM scratch` image now contains only a fully static musl binary: no libc, no `ld-linux`, no shell, no package manager. Combined with the existing `USER 65534` (nobody) directive, the runtime environment is the minimal viable one for an OTLP daemon.

## [0.4.5]

CI and security hardening release. Adds a supply-chain-pinned Dependabot, surfaces Clippy findings in the GitHub Security tab via SARIF, publishes a `SECURITY.md` disclosure policy, and polishes the above-the-fold README so the first screen reads like a product demo instead of a license disclaimer. Zero Rust source changes, zero binary behavior changes.

### Added

- **Dependabot configuration** (`.github/dependabot.yml`): weekly (Monday 06:00 Europe/Paris) GitHub Actions updates, grouped to keep PR noise in check (`ci-actions`, `docker-actions`, `security-actions`, `other-actions`). Cargo dependencies are deliberately not tracked: `cargo audit` (see `security-audit.yml`) already covers the security angle and dozens of minor/patch crate bumps would drown the review queue for a solo project. Crate updates stay manual via `cargo update`.
- **Code Scanning workflow** (`.github/workflows/code-scanning.yml`): runs Clippy with `--message-format=json`, converts to SARIF via `clippy-sarif`, and uploads to GitHub Code Scanning so Clippy warnings show up as actionable entries in the repo Security tab. Purely complementary to `ci.yml` (which remains the authoritative quality gate with `-D warnings`), the job does not fail the build. Triggered on push/PR against `main` (limited to Rust and workflow paths so README-only pushes do not consume CI minutes) and on a weekly cron to catch drift.
- **`SECURITY.md`** at the repo root: responsible-disclosure policy, supported-versions matrix (latest minor only pre-1.0), response-time SLAs, and an explicit out-of-scope list (self-hosted daemon running as `0.0.0.0` behind no firewall, user misconfigurations that leak their own traces, and similar). Linked from the repo Security tab.

### Changed

- **All third-party GitHub Actions pinned to SHA + version comment** across `ci.yml`, `release.yml`, `security-audit.yml`, and the new `code-scanning.yml` (pattern: `uses: actions/checkout@<40-char-sha> # v4.1.1`). Protects against compromised tag re-pushes, a real attack vector after the `tj-actions/changed-files` incident. Dependabot keeps the SHAs fresh without defeating the pinning: each bump PR updates both the SHA and the trailing version comment.
- **CI path filters** (`ci.yml`, `code-scanning.yml`): docs-only pushes no longer trigger the Rust toolchain install + `cargo test` + `llvm-cov` pipeline. Rust test runs fire only when `crates/**`, `tests/**`, `Cargo.toml`, `Cargo.lock`, `.github/workflows/**`, or `sonar-project.properties` (CI only) change. Saves ~7 minutes of CI per README tweak. The weekly cron on `code-scanning.yml` still catches drift when no code lands.
- **SonarCloud integration tightened** (`ci.yml` + `sonar-project.properties`): the scan step is gated on `env.SONAR_TOKEN != ''`, so Dependabot PRs (which do not receive repo secrets on the `pull_request` event) no longer fail the pipeline with "SONAR_TOKEN is not set". `sonar.rust.clippy.reportPaths` is wired to the `clippy-report.json` produced by `ci.yml`, and the server-side Clippy auto-run is disabled (`sonar.rust.clippy.enable=false`) to avoid duplicate noisy warnings.
- **`cargo-llvm-cov` install** migrated from `cargo install --locked cargo-llvm-cov` to `taiki-e/install-action` with `tool: cargo-llvm-cov`. Uses the upstream prebuilt binary, cuts ~2 minutes off every `ci.yml` run on a cold cache.
- **`codeql-action` bumped from v3.35.2 to v4.35.2** in the `security-actions` Dependabot group. Only the SARIF upload step in `code-scanning.yml` is affected.
- **README restructured** (`README.md` + `README-FR.md`): the sample `analyze` output now appears above the fold, ahead of the feature matrix. Redundant carbon-scoring disclaimers removed from the intro (they already live in the dedicated GreenOps section). Image references switched to absolute `raw.githubusercontent.com` URLs so previews render correctly on crates.io and on forks. Comparison table rebalanced (perf-sentinel vs p6spy, SpotBugs, SonarQube rules and others) to remove marketing gaps and keep each row factual.

## [0.4.4]

Docs-only patch fixing two stale references left over from v0.4.3 and the crate rename. No code changes, no binary behavior changes.

### Documentation

- **Runbook `/health` guidance corrected** (`docs/RUNBOOK.md` + `docs/FR/RUNBOOK-FR.md`): both runbooks still claimed there was no dedicated `/health` or `/ready` endpoint and pointed Kubernetes probes at `/metrics`. Accurate before v0.4.3, wrong since v0.4.3 shipped the dedicated `GET /health` liveness endpoint. The runbooks now point operators at `/health` (always exposed, independent of `[daemon] api_enabled`, lock-free, lighter than scraping `/metrics`) and clarify there is no separate `/ready` endpoint because the daemon accepts ingestion from the first tick, so liveness and readiness collapse into one probe.
- **`cargo install` command corrected** in `README.md`, `README-FR.md`, `docs/design/07-CLI-CONFIG-RELEASE.md`, and `docs/FR/design/07-CLI-CONFIG-RELEASE-FR.md`: the install snippet said `cargo install sentinel-cli` but the published crate on crates.io is `perf-sentinel`. Following the old snippet literally failed with a "could not find" error.
- Minor French-runbook polish: a few curly quotes normalized to straight quotes in `docs/FR/RUNBOOK-FR.md` and `docs/FR/design/04-DETECTION-FR.md` for consistency with the rest of the FR docs.

## [0.4.3]

Patch release: adds a liveness `/health` endpoint on the daemon and bumps the Rust toolchain to 1.95.0. No breaking changes.

### Added

- **`GET /health` liveness endpoint** on the daemon HTTP port (default 4318), alongside `/v1/traces`, `/metrics` and the `/api/*` query surface. Returns `200 OK` with `{"status":"ok","version":"<pkg_version>"}`. Stateless, holds no locks, and cannot false-negative under ingestion load. Always exposed, independent of `[daemon] api_enabled`, so it is safe to wire directly into a Kubernetes `livenessProbe`, a load-balancer health check, or a systemd `ExecStartPost` smoke test. `crates/sentinel-core/src/daemon/health.rs`.

### Changed

- **Rust toolchain raised from 1.94.1 to 1.95.0** (both `rust-toolchain.toml` and the workspace MSRV in `Cargo.toml`). Enables idiomatic use of the newly stabilized `Duration::from_mins` and `Duration::from_hours` constructors, existing `Duration::from_secs(60 / 300 / 600 / 3600)` sites migrated to satisfy the new `clippy::duration_suboptimal_units` lint under `-D warnings`. Also picks up the `sort_unstable_by_key(|b| Reverse(b.1))` pattern in `detect/chatty.rs` and removes unnecessary trailing commas in `format!` invocations in `score/carbon.rs` (clippy 1.95.0).

### Documentation

- `README.md` + `README-FR.md`, `GUIDED-TOUR.md`, `docs/QUERY-API.md` + FR, `docs/design/06-INGESTION-AND-DAEMON.md` + FR: `/health` added to the daemon HTTP surface enumeration, with a note that it is always exposed, independent of `[daemon] api_enabled`.
- `ENTERPRISE-JAVA-INTEGRATION-FR.md`: new Kubernetes probes sub-section with ready-to-copy `livenessProbe` + `readinessProbe` YAML on `/health`, plus a fourth `curl -sf /health` smoke test in the quick-verification flow.
- `Dockerfile` and `examples/docker-compose-collector.yml` + `docker-compose-sharded.yml`: healthcheck commands and comments migrated from `/metrics` to `/health`, the purpose-built liveness surface, cheaper than scraping the full Prometheus metrics payload.

## [0.4.2]

Feature release wrapping up Phase 6 (cross-trace correlation, query API, source code attributes, Prometheus pg_stat) and Phase 7 (CI templates, actionable framework-specific fixes, `diff` subcommand, TLS handshake hardening). No breaking changes to the Report JSON shape.

### Added

- **Daemon query API** on port 4318 with five endpoints (`/api/status`, `/api/findings`, `/api/findings/{trace_id}`, `/api/explain/{trace_id}`, `/api/correlations`) and a new `perf-sentinel query` CLI subcommand to consume them. Retained findings ring buffer with `[daemon] max_retained_findings` (default 10k, `0` disables). Full stability contract in `docs/QUERY-API.md`.
- **`perf-sentinel diff --before old.json --after new.json`** subcommand compares two trace sets and emits new/resolved findings, severity changes, and per-endpoint I/O op deltas. Text, JSON or SARIF output. Primary use case: PR regression detection.
- **Cross-trace temporal correlation** (opt-in via `[daemon.correlation] enabled = true`): detects recurring co-occurrences between findings from different services over a rolling window. Output via `/api/correlations` and the NDJSON stream.
- **Actionable framework-specific fixes** attached to findings via `code.namespace` + file extension: v1 Java/JPA, v2 Java reactive (WebFlux, Quarkus reactive) + C# EF Core + Rust Diesel/SeaORM, v3 Helidon SE/MP + Quarkus non-reactive. Rendered in text, JSON (`suggested_fix: { pattern, framework, recommendation, reference_url }`) and SARIF (`fixes[0].description.text`).
- **OTel source code attributes**: findings now carry a `code_location` with `function`, `filepath`, `lineno`, `namespace` extracted from `code.*` span attributes on OTLP, Jaeger and Zipkin. SARIF `physicalLocations` enable inline PR annotations in GitHub/GitLab code scanning.
- **Automated pg_stat ingestion** via `perf-sentinel pg-stat --prometheus http://prometheus:9090`, scraping `postgres_exporter` metrics instead of parsing a file.
- **CI templates** in `docs/ci-templates/` for GitHub Actions, GitLab CI and Jenkins, SHA-pinned, SARIF-wired, ready to copy-paste.

### Changed

- **`daemon/mod.rs` split into six focused submodules** (`sampling.rs`, `tls.rs`, `json_socket.rs`, `listeners.rs`, `event_loop.rs`, plus the existing `findings_store.rs` and `query_api.rs`). Pure mechanical reorganization.
- **`score/mod.rs` split into three focused submodules** (`carbon_compute.rs`, `region_breakdown.rs`, plus the `mod.rs` orchestrator). Pure mechanical reorganization.
- **`score_green` returns a 3-tuple** `(Vec<Finding>, GreenSummary, Vec<PerEndpointIoOps>)` so the per-endpoint counter is built inline from the `endpoint_stats` map already computed for `top_offenders`. Single O(N) span pass.
- **`count_endpoint_stats` keys on `(service, endpoint)`** instead of endpoint-only. Two services serving the same path stay distinct in both `top_offenders` and `per_endpoint_io_ops`.
- **Shared `detect::TraceIndices`** precomputes `children_by_parent` + `span_index` once per trace, the fanout and serialized detectors reuse it instead of rebuilding.
- **`apply_sampling` allocation**: batches of 16 events or fewer use a stack-allocated cache with zero heap allocation.
- **Correlator cap enforcement**: `enforce_pair_cap` drops to 90% of the cap in one amortized pass and clones only the evicted keys.

### Fixed

- **`namespace_matches` enforces trailing segment boundaries**, not just leading. Hints like `io.helidon` no longer false-match `io.helidongrpc.Foo`, and `Microsoft.EntityFrameworkCore` no longer matches `Microsoft.EntityFrameworkCoreCache.Provider`.
- **`perf-sentinel diff --output <file>` no longer leaks ANSI escape codes into the file** when launched from an interactive terminal. `write_diff_text` takes an explicit palette, `emit_diff` forces `no_colors()` whenever `output` is set.
- **Electricity Maps default endpoint** harmonized to `https://api.electricitymaps.com/v3` across both code paths.
- **CI templates SHA-pinned** (`docs/ci-templates/github-actions.yml` and `gitlab-ci.yml`). The GitLab Code Quality jq was reading non-existent `code_location.code_filepath` / `code_location.code_lineno` fields, fixed to the v0.4.1 stability contract (`filepath`, `lineno`).

### Security

- **OTLP HTTP `/v1/traces` now validates `Content-Type`** and returns HTTP 415 on requests that do not declare `application/x-protobuf`.
- **Defense-in-depth JSON nesting cap** at 32 levels for the native ingest path, protecting against pathological `[[[...]]]` payloads.
- **TLS listener handshake hardening**: `tls_tcp_incoming` runs each handshake in its own task so a stalled peer no longer blocks the accept loop, and both TLS listeners (gRPC and HTTP) cap concurrent handshakes and live connections at `TLS_MAX_INFLIGHT = 128`. Closes a pre-auth DoS surface.
- **`read_events` TOCTOU closed**: the CLI caps reads via `.take(max + 1)`, same in `cmd_calibrate` for the energy CSV.
- **Release workflow `permissions:` tightened per-job**, the workflow floor is now `contents: read`.

### Documentation

- New `docs/QUERY-API.md` + `docs/FR/QUERY-API-FR.md` with per-endpoint reference, captured curl responses, Prometheus/Grafana use cases and a stability contract.
- New "CI integration recipes" section in `docs/INTEGRATION.md` + FR linking to the three CI templates.
- New "Actionable fixes" section in `docs/design/04-DETECTION.md` + FR with the per-language rule tables and the segment-boundary matcher rule.
- New "Diff subcommand" section in `docs/design/07-CLI-CONFIG-RELEASE.md` + FR, and a new "PR regression detection" section in `docs/INTEGRATION.md` + FR with a ready-to-copy GitHub Actions workflow.
- `docs/ARCHITECTURE.md` + FR and `docs/design/00-INDEX.md` + FR updated to reflect the daemon/score submodule splits.
- New TLS handshake concurrency cap note in `docs/LIMITATIONS.md` + FR, the `docs/CONFIGURATION.md` + FR TLS rows mention the 128-handshake cap.

## [0.4.1]

Patch release: makes config loading robust on Windows, adds soft startup warnings for unusual daemon-limit values and tightens the bench percentile calculation. No breaking changes.

### Added

- **Startup comfort-zone warnings for daemon limits**. `validate_daemon_limits` and `validate_detection_params` now emit a one-shot `tracing::warn!` when `max_payload_size`, `max_active_traces`, `max_events_per_trace`, `max_retained_findings`, `trace_ttl_ms`, or `max_fanout` falls outside its recommended comfort zone (e.g. `max_active_traces` outside 1,000-100,000). The warning includes the field name, the value, the boundary crossed, and a one-line explanation of the practical consequence (eviction pressure, ingest latency, detection noise). Hard caps are unchanged, the warning is informational only. A new `config_defaults_sit_inside_every_comfort_zone` test locks the invariant that `Config::default()` never triggers a startup warning.
- 21 new tests across `config.rs` and the bench percentile module: UNC paths (raw + pre-escaped), inline comments after path values, every entry of `TOML_PATH_STRING_KEYS`, TOML literal strings, the fallback branch, a pathological 10,000-backslash input, percentile edges (n=1, n=2, n=101, empty), comfort-zone behavior on each daemon limit, and `max_retained_findings = 0` still disabling the store.

### Changed

- **`escape_toml_path_backslashes`** split into three named helpers (`copy_until_backslash`, `skip_backslash_run`, `backslash_emit_len`), bringing SonarCloud cognitive complexity from 20 to under 15.
- **`find_basic_string_end`** rewritten with a linear consecutive-backslash counter instead of a per-quote backward lookbehind, closing a worst-case O(n²) on inputs full of `\`. A regression test feeds it a 10,000-backslash run.

### Fixed

- **Windows-style config paths in TOML basic strings**. New `normalize_toml_path_strings` pre-processor in `config.rs` rewrites bare backslashes inside path-keyed values (`hourly_profiles_file`, `calibration_file`, `json_socket`, `tls_cert_path`, `tls_key_path`) so `json_socket = "C:\temp\sock"` parses as a literal path instead of triggering a TOML escape error. Already-escaped pairs (`C:\\temp\\sock`), TOML literal strings (`'C:\temp\sock'`), and UNC prefixes (`\\server\share`) round-trip correctly. Falls back transparently to the original input if normalization breaks parsing, with a `tracing::debug!` line for diagnosability.
- **`max_retained_findings` was unbounded**. A typo like `max_retained_findings = 999999999999` would have OOM-ed the daemon at the first stored finding. Now capped at 10,000,000, with `0` still documented to disable the store entirely.
- **Bench `p99` off-by-one**. `compute_latency_percentiles` was computing `p99_idx = ceil(len * 0.99)` (a 1-based rank) and indexing into the 0-based vector, biasing the reported p99 by one position. Now correctly subtracts 1 with `saturating_sub`, guards against empty slices (returns `(0.0, 0.0)`) and bounds `p50_idx` symmetrically with `p99_idx`.
- **Daemon-only build broke after a `feature = "tempo"` gating regression**. `spawn_one_shot_server`, `http_200_text`, and `http_status` in `test_helpers.rs` are now gated under `any(feature = "daemon", feature = "tempo")` so `cargo check --features daemon` (without `tempo`) keeps compiling. `http_200_bytes` stays under `tempo` only since it is the only consumer.
- **UTF-8 safety in `normalize_toml_path_line`**. The opening `"` is now pushed explicitly instead of relying on an inclusive byte-range slice that could panic on a multi-byte char before the value.

### Documentation

- `docs/CONFIGURATION.md` and `docs/FR/CONFIGURATION-FR.md`: hard ranges added to each `[daemon]` field row, new "Comfort zones and startup warnings" sub-section with a band table per field.
- `docs/design/07-CLI-CONFIG-RELEASE` (EN + FR): bench percentile snippet updated to reflect the off-by-one fix.

## [0.4.0]

Phase 6 release: turns the daemon from a pattern detector into an insight engine. Four headline features, plus hardening.

### Added

- **Cross-trace temporal correlation (daemon mode)**. New `detect/correlate_cross.rs` module with a `CrossTraceCorrelator` that detects recurring co-occurrences between findings from different services within a rolling window (default 10 minutes). Uses Algorithm R reservoir sampling (capped at 256 samples per pair) seeded by a deterministic `SplitMix64` + FNV-1a endpoint hash so sampled lags stay reproducible across runs. Incremental `source_totals` and `select_nth_unstable_by_key` bounded eviction keep the per-tick cost O(occurrences) instead of rebuilding every tick. Opt-in via `[daemon.correlation] enabled = true` with `window_minutes`, `lag_threshold_ms`, `min_co_occurrences`, `min_confidence`, and `max_tracked_pairs` knobs.
- **OTel source code attributes**. Findings now carry a `code_location` field populated from `code.function`, `code.filepath`, `code.lineno`, and `code.namespace` span attributes, supported across OTLP (gRPC + HTTP), Jaeger, and Zipkin. The CLI renders a new `Source:` line on findings (`namespace.function (filepath:lineno)`, omitting absent parts), and SARIF v2.1.0 output gains `physicalLocation` entries when `filepath` is present, enabling inline annotations in GitHub and GitLab code scanning views. Hostile filepath values (literal and percent-encoded `..` traversal, absolute paths, URL schemes, overlong UTF-8, BiDi / invisible Unicode) are rejected in the SARIF sanitizer to close Trojan Source (CVE-2021-42574) vectors.
- **Automated pg_stat ingestion from Prometheus**. New `perf-sentinel pg-stat --prometheus http://prometheus:9090` scrapes `pg_stat_statements_seconds_total` via the Prometheus HTTP API and produces the same `PgStatReport` as the existing file-based path. Zero new dependencies, reuses the `http_client` module introduced in v0.3.0. Endpoints are validated at config load (scheme must be `http`/`https`, userinfo rejected) and redacted in error messages. File-based `--input traces.csv` continues to work unchanged.
- **Daemon query API and `query` subcommand**. The daemon exposes its internal state via five HTTP endpoints on the existing port 4318 (alongside `/v1/traces` and `/metrics`): `GET /api/findings` (filterable by `service`, `type`, `severity`, `limit`, capped at 1000), `GET /api/findings/{trace_id}`, `GET /api/explain/{trace_id}` (tree with findings inline, served from the in-memory trace window), `GET /api/correlations` (active cross-trace correlations), and `GET /api/status` (uptime, active traces, stored findings count, version). A new `FindingsStore` ring buffer (default 10000, configurable via `[daemon] max_retained_findings`) retains recent findings for querying. A new `perf-sentinel query --daemon http://localhost:4318 <action>` CLI subcommand queries these endpoints with five sub-actions (`findings`, `explain`, `inspect`, `correlations`, `status`), rendering colored terminal output by default and `--format json` when scripting. `inspect` fetches explain trees in parallel via `tokio::task::JoinSet` (concurrency 16) so the TUI opens in ~300 ms on 100 traces instead of ~5 s sequentially. Gated by `[daemon] api_enabled` (default `true`), no-auth threat model documented in `docs/LIMITATIONS.md`.
- **TUI `Source:` line**. The `inspect` TUI detail panel renders the same `Source:` line as the CLI text output when findings carry a `code_location`.

### Changed

- **Breaking**: `DaemonError::TlsConfig(Box<dyn std::error::Error>)` replaced by a typed `TlsConfigError` enum with five concrete variants (`ReadCert`, `ReadKey`, `ParseCerts`, `ParseKey`, `ServerConfig`). Callers that matched on the boxed error must now match on the enum. Source chains are preserved via `#[source]`.
- **Breaking**: all public error enums are now `#[non_exhaustive]` (`DaemonError`, `TlsConfigError`, `ConfigError`, `PgStatError`, `JsonIngestError`, `JaegerIngestError`, `ZipkinIngestError`, `TempoError`, `FetchError`, `CalibrationError`, `SarifError`). External `match` expressions on these types must include a catch-all arm going forward, letting subsequent minor releases add variants without a major bump.
- `SpanEvent` gains four optional fields (`code_function`, `code_filepath`, `code_lineno`, `code_namespace`) and `Finding` gains an optional `code_location` field. All marked `#[serde(default, skip_serializing_if = "Option::is_none")]`, so JSON consumers that ignore unknown fields are unaffected.
- **Runtime reuse**. `perf-sentinel query` and `pg-stat --prometheus` use the parent `#[tokio::main]` runtime directly instead of constructing a nested `Runtime` (which would have panicked at runtime).
- **Cognitive complexity reduction**. Four functions flagged by SonarCloud's `rust:S3776` rule were split into named helpers without behavioral change: `print_findings` (from 33 to under 15, extracted into `print_finding_entry`, `print_finding_impact`, `format_code_location`, and severity helpers), `cmd_query` (from 62 to under 15, one helper per action plus `build_findings_path` and `print_pretty_json`), `daemon::run` (from 19 to under 15, extracted `ingest_event_batch`, `evict_expired_traces`, `flush_evicted`, `shutdown_listeners`, and a `ServiceMeter` struct), `CrossTraceCorrelator::ingest` (from 18 to under 15, extracted `evict_stale`, `record_co_occurrences`, `enforce_pair_cap`).

### Performance

- **Daemon query API bounds**. Endpoint responses cap at 1000 items to bound payload size under pathological queries. The findings store clones outside the lock to minimize hold time, short-circuits on `max_size == 0`, and sizes the initial `VecDeque` capacity at `min(max_size, 4096)`. The `/api/explain/{trace_id}` endpoint reads from the in-memory trace window and runs `detect::detect()` inline, with no per-request disk I/O.

### Documentation

- Design docs updated EN + FR: `04-DETECTION` (cross-trace correlation algorithm), `06-INGESTION-AND-DAEMON` (daemon query API), `07-CLI-CONFIG-RELEASE` (query subcommand, pg-stat Prometheus flag). New Mermaid diagram `query-api.mmd` with light + dark SVG exports, wired into `docs/ARCHITECTURE.md` and `docs/design/06-INGESTION-AND-DAEMON.md` and their FR counterparts.
- `docs/CONFIGURATION.md`, `docs/LIMITATIONS.md`, `GUIDED-TOUR.md`, `ENTERPRISE-JAVA-INTEGRATION-FR.md` updated with Phase 6 content.
- `docs/img/analyze/*` and `docs/img/inspect/*` regenerated so the new `Source:` line shows on every n+1, redundant, and slow finding. Demo fixture `tests/fixtures/demo.json` gained plausible `code.*` attributes per trace (repository and client classes, Java filepaths, line numbers).

## [0.3.2]

### Changed

- **SonarCloud code duplication reduced** from 41 blocks (736 lines) to 29 blocks (444 lines, all in `carbon_profiles.rs` data tables) via five refactorings: an `impl_energy_state!` macro in `energy_state.rs` eliminating the identical `ScaphandreState` / `CloudEnergyState` wrapper impls, a `build_per_trace_finding()` helper in `detect/mod.rs` eliminating the duplicated `Finding` struct literal between `n_plus_one.rs` and `slow.rs`, an `emit_report_and_gate()` helper in `main.rs` eliminating the duplicated format/emit/gate logic between `cmd_analyze` and `cmd_tempo`, `FetchError` + `fetch_get()` in `http_client.rs` eliminating the duplicated HTTP fetch logic between `scaphandre/scraper.rs` and `cloud_energy/scraper.rs`, and a `test_scrape_fixture()` helper in `scaphandre/tests.rs` eliminating duplicated test setup.
- **SonarCloud cognitive complexity**: `process_traces()` refactored from 19 to under 15 by extracting `record_slow_durations()` and `emit_findings_and_update_metrics()`.

### Security

- **RUSTSEC-2025-0134**: removed the `rustls-pemfile` dependency (archived, unmaintained since August 2025). PEM parsing now uses `rustls-pki-types::pem::PemObject`, which was already in the dependency tree via `rustls`. Zero new dependencies, one direct dependency removed.

## [0.3.1]

Additive release on top of v0.3.0. No breaking changes, default behavior unchanged.

### Added

- **Score interpretation bands**: IIS and waste ratio now carry human-readable labels (`healthy` / `moderate` / `high` / `critical`) in CLI output and JSON reports.
- **Tempo trace ingestion**: new `perf-sentinel tempo` subcommand to pull traces from Grafana Tempo for post-hoc analysis.
- **Calibrate mode**: new `perf-sentinel calibrate` to tune energy coefficients from real RAPL measurements.
- **Electricity Maps API**: real-time carbon intensity via opt-in API integration.
- **30+ hourly carbon profiles**: seasonal x hourly profiles for 30+ cloud regions (AWS, GCP, Azure).
- **Per-operation energy coefficients**: SQL verb and HTTP payload size weighting for more accurate carbon estimates.
- **Network transport energy**: opt-in cross-region network energy term (Mytton et al. 2024).
- **Three new detectors**: chatty service, connection pool saturation, serialized-but-parallelizable calls.
- **Finding confidence field**: `ci_batch` / `daemon_staging` / `daemon_production` for perf-lint IDE integration.
- **Cloud SPECpower energy**: opt-in CPU% + SPECpower interpolation for AWS/GCP/Azure VMs.
- **Scaphandre RAPL integration**: opt-in per-process energy measurement via Prometheus scraping.
- **TLS for daemon OTLP receivers**: optional TLS on gRPC (4317) and HTTP (4318) via `tls_cert_path` / `tls_key_path`.
- **Sharded daemon deployment**: validated horizontal scaling via OTel Collector `loadbalancingexporter`, with a correctness test and a docker-compose example.
- **Slow duration histogram**: new `perf_sentinel_slow_duration_seconds{type}` Prometheus histogram for accurate global percentiles across sharded instances.

## [0.3.0]

Major release focused on carbon scoring reliability, ecosystem coverage, and two new subcommands. The largest release since v0.2.0, covering the Phase 5 roadmap work (carbon reliability + ecosystem expansion). 10 commits between v0.2.3 and v0.3.0, 1012 tests passing (up from 476 at v0.2.3).

### Added

- **Carbon scoring precision ladder**. Six energy backends now coexist with a clear precedence: `electricity_maps_api` (real-time grid intensity via opt-in HTTPS API) > `scaphandre_rapl` (per-process RAPL on Linux bare metal) > `cloud_specpower` (CPU% + SPECpower interpolation for AWS / GCP / Azure VMs, ~180 instance types embedded from the Cloud Carbon Footprint coefficient tables) > `io_proxy_v3` (monthly x hourly profiles for 4 regions) > `io_proxy_v2` (hourly profiles for 30+ regions) > `io_proxy_v1` (flat annual, universal fallback). All six can be mixed in the same run, the top-level `co2.model` tag reflects the highest-precision source seen.
- **SCI v1.0 alignment (ISO/IEC 21031:2024)**. Carbon estimates now follow the Software Carbon Intensity specification from the Green Software Foundation. Total is `(E x I) + M [+ T]` summed over analyzed traces, with embodied carbon `M = traces * embodied_per_request_gco2` and optional network transport `T` for cross-region HTTP calls (default 0.04 kWh/GB, per Mytton, Lunden & Malmodin 2024). Every `CarbonEstimate` carries a `{ low, mid, high }` confidence interval (2x multiplicative) and a `methodology` tag so downstream consumers can tell SCI-numerator values from per-R intensity values.
- **Multi-region scoring**. The OTel `cloud.region` attribute is read per span, scores are bucketed by region and returned as `green_summary.regions[]` sorted by `co2_gco2` descending. Per-service overrides via `[green.service_regions]`. 30+ cloud regions now have embedded hourly profiles sourced from ENTSO-E, national TSOs, EIA, Hydro-Quebec and AEMO data (see the data sources list below).
- **Per-operation energy coefficients**. The proxy carbon model now weights energy per I/O operation by type: SQL `SELECT` 0.5x, `INSERT`/`UPDATE` 1.5x, `DELETE` 1.2x, HTTP payload size tiers (small <10 KB 0.8x, medium 10 KB-1 MB 1.2x, large >1 MB 2.0x). Coefficients sourced from the Xu et al. and Tsirogiannis et al. foundational DBMS energy benchmarks, cross-validated against the Siddik et al. DBJoules (2023) per-operation measurement study that confirmed 7-38% inter-operation variance.
- **Three new detectors**: `chatty_service` (too many HTTP outbound calls per trace), `pool_saturation` (sweep-line peak concurrency detection for connection pool contention), and `serialized_calls` (Weighted Interval Scheduling DP for parallelizable sibling spans). Seven detectors now run per trace, up from four.
- **Interpretation bands**. `io_intensity_score` and `io_waste_ratio` ship with a stable `healthy` / `moderate` / `high` / `critical` classification, rendered as colored parentheticals in the CLI and as `*_band` fields in the JSON report. Thresholds are anchored on the detector constants (the N+1 detector's `CRITICAL_OCCURRENCE_THRESHOLD` and `Config::default().n_plus_one_threshold`) via drift-guard tests that fail at build time if either side moves without the other.
- **`perf-sentinel tempo`** (feature `tempo`, on by default). Query a Grafana Tempo HTTP API for post-hoc trace analysis, either by trace ID or by service + lookback window. Fetches OTLP protobuf, decodes via `prost`, and runs the standard analysis pipeline. Supports `--format text|json|sarif` and `--ci` quality-gate mode. Dedicated 64 MiB body cap (`MAX_TRACE_BODY_BYTES`) for realistic traces with hundreds of spans.
- **`perf-sentinel calibrate`**. Tune the proxy model's energy-per-op coefficients from real RAPL / wattmeter measurements. Correlates a trace file with an energy CSV (`power_watts` or `energy_kwh`) by service and time window, writes a calibration TOML that the main config loads via `[green] calibration_file`. The model tag gains a `+cal` suffix when active (e.g. `io_proxy_v2+cal`). Energy CSV capped at 64 MiB.
- **Three new Prometheus metrics**: `perf_sentinel_service_io_ops_total{service}` (per-service op counter feeding the scraper snapshot-diff path), `perf_sentinel_scaphandre_last_scrape_age_seconds`, `perf_sentinel_cloud_energy_last_scrape_age_seconds`.
- **Test coverage from 476 to 1012 (+536)**. The five network-facing scrapers (`daemon`, `tempo`, `cloud_energy`, `electricity_maps`, `scaphandre`) gained mock-HTTP integration coverage: happy path, 4xx / 5xx handling, malformed bodies, timeouts, body-cap enforcement, graceful abort on invalid URI.

### Changed

- **Breaking**: `CarbonEstimate` and `CarbonReport` restructured for SCI v1.0. `CarbonContext.scaphandre_snapshot` renamed to `energy_snapshot` (now a `HashMap<String, EnergyEntry>` carrying both the coefficient and the model tag). `CarbonEstimate` gains `sci_numerator_with_model` / `operational_ratio_with_model` constructors, the pre-existing `sci_numerator` / `operational_ratio` still work. No compat shim.
- **Breaking**: `[green] region` renamed to `[green] default_region` in `.perf-sentinel.toml`. The legacy flat name still loads with a deprecation warning.
- New JSON fields on the report: `green_summary.io_waste_ratio_band`, `top_offenders[].io_intensity_band`, `green_impact.io_intensity_band`, plus the structured `co2` object with `low` / `mid` / `high` / `model` / `methodology`, and the `green_summary.regions[]` per-region breakdown. Downstream consumers should ignore unknown fields, stability contract in `docs/LIMITATIONS.md#score-interpretation`.
- **Cognitive complexity reduction**. Nine functions flagged by SonarCloud's `rust:S3776` rule were split into named helpers without behavioral change: `compute_carbon_report` (from 160 to below 15, extracted into 9 span/region/model helpers plus three supporting structs), `load_custom_profiles`, `validate_cloud_services`, `extract_exe_label`, `detect_pool_saturation`, `longest_non_overlapping`, `validate_green`, `parse_energy_csv`, `parse_iso8601_utc_to_ms`. 1012 tests still green, zero clippy warnings under `-D warnings` + `clippy::pedantic`.
- **Daemon shutdown hygiene**. Ctrl-C now aborts all spawned tasks (gRPC, HTTP, JSON socket, Scaphandre scraper, cloud energy scraper, Electricity Maps scraper) before draining the window, so stray `tracing::error!` lines no longer leak after the "Shutting down" message.

### Performance

- `daemon::build_tick_ctx` returns `Cow<CarbonContext>` and borrows the base context when no scraper produced fresh data (no per-tick clone in the common case).
- `daemon::apply_sampling` keys its decision cache on a u64 FNV-1a hash instead of a `String` clone (zero heap allocations for cache keys on bursts).
- `score::count_endpoint_stats` merges its double-probe HashMap pass into a single span loop via a `last_seen_trace` sentinel.
- `score::cloud_energy::scraper` parallelizes per-service Prometheus CPU% queries via `tokio::task::JoinSet` (1024 services at 20 ms used to serialize to ~20 s, now collapses to a single tail latency).

### Security

- Calibrate energy CSV capped at 64 MiB to prevent local DoS.
- Three `unsafe { std::env::remove_var }` blocks removed from `config.rs` tests via env-lookup dependency injection, `sentinel-core` is back to zero `unsafe` blocks.
- Predictable `/tmp` test paths replaced with `tempfile::TempDir` (symlink-TOCTOU safe, auto-cleanup on drop).
- `daemon::run_json_socket` gains a symlink pre-check so a local attacker cannot point the socket path at a victim file.
- `ingest_from_tempo` endpoint validator now rejects `@` only in the authority section (not in the path or query string), so `?owner=foo%40example.com` style URLs are accepted.
- **RUSTSEC-2026-0097 (`rand` unsound)**: `rand` bumped from 0.9.2 to 0.9.3 (inside the patched range). A second `rand 0.8.5` entry remains in `Cargo.lock` as an optional subdep of `ratatui-termwiz` (a termwiz backend perf-sentinel never activates, `crossterm` is used), documented as non-applicable in `audit.toml` at the repo root with the full exposure analysis. perf-sentinel cannot trigger the unsoundness regardless: `tracing_subscriber::fmt()` is used without a custom logger, so none of the five UB preconditions can be satisfied.

### Documentation

- Carbon estimates now rest on an auditable chain of public standards, reference datasets, and peer-reviewed methodology: SCI v1.0 (ISO/IEC 21031:2024, Green Software Foundation, `co2.total` is the SCI numerator, not the per-R intensity), Cloud Carbon Footprint (annual grid intensity per cloud region, per-provider PUE values, SPECpower tables for ~180 instance types), Electricity Maps (annual average intensities 2023-2024 for the `io_proxy_v1` baseline, plus the real-time API backend), ENTSO-E Transparency Platform (hourly generation and load data behind the European monthly x hourly profiles), national TSOs (RTE eCO2mix for France, Fraunhofer ISE energy-charts.info for Germany, National Grid ESO Carbon Intensity API for the UK, EirGrid, TenneT, Svenska Kraftnat, Elia, Fingrid, Terna, REE, PSE and Statnett for the rest of Europe), EIA Open Data API for US balancing authorities (PJM, CAISO, BPA), Hydro-Quebec for Canada, AEMO NEM / OpenNEM for Australia, and Scaphandre for per-process RAPL measurement.
- Academic methodology cited inline: Xu et al. (foundational DBMS per-operation energy benchmark) and Tsirogiannis et al. (SIGMOD 2010 companion benchmark establishing verb-level coefficients), Siddik et al. DBJoules (2023, 7-38% inter-operation variance), Guo et al. (ACM Computing Surveys 2022 field survey), the IDEAS 2025 real-time SQL energy estimation framework (direction of travel for future `calibrate` improvements), Mytton, Lunden & Malmodin (Journal of Industrial Ecology 2024, source of the 0.04 kWh/GB default and the 0.03-0.06 kWh/GB range behind the configurable `network_energy_per_byte_kwh` field), and the Boavizta API / HotCarbon 2024 embodied-carbon model behind the `embodied_per_request_gco2` default. References are enumerated with inline citations in `score/carbon.rs` (top-level doc comment) and `score/carbon_profiles.rs` (per-region source comments), the exhaustive per-region list lives in `docs/design/05-GREENOPS-AND-CARBON.md`.

## [0.2.3]

Patch release with a TTL eviction robustness fix, improved observability, and documentation updates.

### Added

- **OTLP span index warning**: a `tracing::warn!` is now emitted when the 100k span index cap is reached, helping operators diagnose degraded parent resolution.

### Fixed

- **TTL eviction clock skew resilience**: eviction now scans the full LRU cache instead of stopping at the first non-expired entry, preventing expired traces from being missed when `SystemTime` goes backward (NTP adjustments).

### Documentation

- Correlation/streaming design docs (EN + FR) updated with the new full-scan eviction logic and the OTLP warning.
- Score dedup comment corrected to reflect the actual dedup key `(trace_id, template, source_endpoint)`.

## [0.2.2]

Hardening release with input validation, test coverage expansion, architecture diagrams, and documentation improvements.

### Added

- **55 new edge-case tests** across 7 modules (421 to 476 tests): Jaeger/Zipkin (malformed payloads, missing fields, null values, empty arrays), SQL normalizer (comments, unterminated quotes, dollar quotes, truncation, UTF-8), HTTP normalizer (fragments, URL-encoded segments, double slashes, param cap), OTLP (empty IDs, missing `service.name`, no resource), slow detection (exact threshold boundaries, 5x severity boundary, `min_occurrences=1`), config (port validation edges, TTL bounds, sampling rate 0.0/1.0), SARIF (empty findings, special characters, missing `green_impact`).
- Bug report and feature request issue templates, plus a CODEOWNERS file.

### Changed

- **`DetectConfig`**: `impl From<&Config>` eliminates 4 duplicated construction sites.
- **OTLP**: `build_span_index()` extracted to reduce cognitive complexity (SonarCloud S3776 fix).
- **Report header**: parameterized title, "report" for `analyze`, "demo" for `demo`.
- **`TopOffender`**: removed the redundant `io_ops_per_request` field.
- **SQL tokenizer**: simplified `step_in_double_quote`, pre-allocated the params `Vec`.

### Fixed

- **SQL normalizer UTF-8 fix**: replaced byte-to-char casting with `&str` slice extraction, fixing corruption for multi-byte characters in string literals and dollar-quoted strings.
- **Timestamp parsing fix**: `parse_timestamp_ms` now computes milliseconds since Unix epoch (not midnight) using the days_from_civil algorithm, fixing false negatives for traces spanning midnight.
- **Fractional timestamp panic**: `frac[..3]` replaced with `frac.get(..3)` to prevent a panic on non-ASCII input.

### Security

- **SQL normalizer**: queries exceeding 64 KB truncated at a char boundary before normalization.
- **HTTP normalizer**: query parameters capped at 100 to prevent unbounded allocation.
- **OTLP ingestion**: span index capped at 100k spans per resource.
- **pg_stat ingestion**: CSV and JSON parsing hard-limited at 1,000,000 entries.
- **Config validation**: `listen_port_http` and `listen_port_grpc` validated in range 1-65535, `trace_ttl_ms` bounded to 100 ms to 1 h.
- **Unix socket**: permission enforcement is now fatal, the daemon refuses to start if `0o600` cannot be set.
- **Metrics**: lock poisoning handled gracefully via `PoisonError::into_inner`, `render()` returns a fallback instead of panicking.

### Documentation

- **6 Mermaid architecture diagrams** (`.mmd` sources + rendered SVGs): pipeline batch architecture, daemon streaming architecture, auto-format detection flow, detection orchestration, OTLP two-pass conversion, CLI commands overview. Integrated into the ARCHITECTURE, CONFIGURATION and INTEGRATION docs (EN + FR).
- Phase 5 roadmap and an enhanced comparison table in the README.
- Corrected port 4317 to 4318 in the `INTEGRATION.md` sidecar example.

## [0.2.1]

Patch release with correctness fixes, hardening, and project maintenance.

### Added

- Bug report and feature request issue templates, plus a CODEOWNERS file.

### Changed

- **Report header**: `format_colored_report` parameterized with a title, shows "report" for `analyze` and "demo" for `demo`.
- **Config validation**: `validate_listen_addr` returns `Result<(), String>` for consistency with sibling validators.
- **`TopOffender`**: removed the redundant `io_ops_per_request` field (identical to `io_intensity_score`).

### Fixed

- **SQL normalizer UTF-8 fix**: replaced byte-to-char casting (`b as char`) with `&str` slice extraction, fixing corruption for multi-byte characters in string literals and dollar-quoted strings.
- **Timestamp parsing fix**: `parse_timestamp_ms` now computes milliseconds since Unix epoch (not midnight) using the days_from_civil algorithm, fixing false negatives for traces spanning midnight.

### Performance

- Pre-allocations: `Vec` in the SQL normalizer, `HashMap` in the redundant and slow detectors.

### Security

- CI workflows now skip on fork PRs to prevent malicious code execution.

### Documentation

- Phase 5 roadmap and an enhanced comparison table in the README.
- Corrected accuracy for Hypersistence Optimizer, Datadog, New Relic and Digma in the comparison table.
- Demo assets regenerated (`docs/img/`) to reflect the updated CLI output.

## [0.2.0]

Second release. Adds multi-format trace ingestion, interactive inspection, SARIF export, and deeper detection capabilities.

### Added

- **Explain mode** (`perf-sentinel explain --trace-id ID`): colored span tree view with JSON output, depth guard (256), and cyclic parent protection.
- **TUI inspect mode** (`perf-sentinel inspect`): 3-panel ratatui interface (traces, findings, detail + span tree) with a cached tree per trace.
- **SARIF v2.1.0 export** (`--format sarif`), compatible with GitHub/GitLab code scanning integration.
- **Jaeger and Zipkin ingestion**: Jaeger JSON and Zipkin JSON v2 formats with auto-detection via byte-level heuristics.
- **pg_stat_statements ingestion** (`perf-sentinel pg-stat`): CSV/JSON auto-detection, top-N rankings by `total_exec_time`/`calls`/`mean_exec_time`, cross-reference with trace findings via `--traces`.
- **Fanout detection**: new `ExcessiveFanout` finding type with configurable `max_fanout` (default 20, range 1-100,000).
- **Cross-trace slow percentiles**: p50/p95/p99 via the nearest-rank algorithm.
- **Grafana exemplars**: OpenMetrics exemplar annotations on the `findings_total` and `io_waste_ratio` metrics.

### Changed

- **SQL tokenizer**: double-quoted identifiers and PostgreSQL dollar-quoted strings (`$$`/`$tag$`) now supported, CTEs and `CALL` confirmed working.
- **ID validation**: `sanitize_id()` with char-boundary-aware UTF-8 truncation (`MAX_ID_LENGTH=128`) at the normalize boundary.
- **Pipeline**: `analyze_with_traces()` extracted to reduce duplication, `sort_findings()` shared between the pipeline and inspect.
- `parent_span_id` added to `SpanEvent` for tree building and fanout detection.
- `is_avoidable_io()` is now the single source of truth for waste classification.
- SonarCloud complexity fixes across the SQL tokenizer, bench, and normalize modules.
- Security audit workflow updated to `actions-rust-lang/audit` v1.2.7.

### Performance

- Throughput above 100k events/sec, RSS under 5 MB idle and under 20 MB loaded (10k traces), binary under 10 MB (stripped, LTO), latency under 1 ms per event.

### Documentation

- OTel Collector config (`otel-collector-config.yaml`) with batch, tail_sampling, and filter examples.
- Docker Compose with healthchecks.
- Contributor Covenant Code of Conduct v2.1.
- Generic domain examples in tests and fixtures.

## [0.1.0]

First public release. Lightweight polyglot performance anti-pattern detector for runtime traces.

### Added

- **N+1 detection** for SQL queries and HTTP calls (same template, different params, within a configurable time window).
- **Redundant call detection**: exact duplicate queries or calls within a trace.
- **Slow operation detection** with configurable thresholds (per-trace, recurring template).
- **SQL normalization**: homemade tokenizer replacing literals, UUIDs and IN lists with placeholders.
- **HTTP normalization**: numeric and UUID path segments replaced, query params stripped.
- **GreenOps scoring**: I/O Intensity Score (IIS) per endpoint, I/O Waste Ratio, top offenders.
- **Carbon estimation**: optional gCO2eq conversion based on cloud region (embedded intensity table, no network calls).
- **OTLP ingestion**: gRPC (port 4317) and HTTP (port 4318) with both legacy and stable OTel semantic conventions.
- **CI quality gate**: configurable threshold rules with exit code 1 on failure.
- **Daemon mode** (`perf-sentinel watch`): streaming analysis with LRU/TTL trace management.
- **Prometheus metrics** exposed on `/metrics` in daemon mode.
- **Colored terminal output** with TTY detection, JSON output for CI.
- **Benchmark mode** (`perf-sentinel bench`): throughput and latency measurement with p50/p99.

### Performance

- Throughput above 100k events/sec, RSS under 5 MB idle and under 20 MB loaded (10k traces), binary under 10 MB (stripped, LTO), latency under 1 ms per event.

### Documentation

- Integration guide (Java, .NET, Rust, OTel Collector), configuration reference, architecture and design docs shipped at launch.
