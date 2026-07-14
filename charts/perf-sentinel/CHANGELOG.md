# Changelog

All notable changes to the perf-sentinel Helm chart are documented in
this file. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
From version 0.9.0 the chart `version` tracks the perf-sentinel
application version. Both the chart `version` and `appVersion` move in
lockstep, replacing the earlier independent `0.2.x` chart line.

## [0.9.10]

### Changed

- `appVersion` bumped to `0.9.10`. The self-contained HTML dashboard is
  reskinned to the hi-fi design handoff: DM Sans and JetBrains Mono
  replace Geist as the embedded brand fonts (OFL-1.1, latin subsets),
  flat pastel fills replace every gradient, and the sidebar nav, buttons,
  cards, modals and daemon status dots move onto the theme tokens.
  Overview KPI cards become semantic and clickable, the Findings card
  taking a solid color driven by the highest severity present. No chart
  template change, this bump tracks the new appVersion.

### Added

- `appVersion` bumped to `0.9.10`. The dashboard gains sortable tables
  (shareable through the `tsort` hash key), a comfort/compact density
  toggle persisted in the browser, search-match highlighting, and an
  `Undo` button on the acknowledgment toast. No chart template change,
  this bump tracks the new appVersion.

### Fixed

- `appVersion` bumped to `0.9.10`. The Acknowledgments tab badge no
  longer shows a stale `0` next to the live count, the trace tree tints
  its highlight by finding severity instead of always red, and in live
  mode only the Ack button swallows row clicks. No chart template change,
  this bump tracks the new appVersion.

## [0.9.9]

### Fixed

- `appVersion` bumped to `0.9.9`. `slow_sql` now fires for services
  instrumented by the PHP OTel contrib packages (Symfony + Doctrine +
  PDO), which split each query across a statement-bearing `SELECT orders`
  span (`db.query.text`, ~0 ms) and its statement-less `Doctrine::execute`
  sibling (the real duration), with `db.system.name` only on the pdo
  child. OTLP conversion previously dropped every duration-bearing span as
  `missing_db_statement`, so those services never produced a slow SQL
  event. A stitch pass now re-joins each query into one event carrying the
  statement and the real duration; merged spans count under the new
  `merged_db_span` reason of `perf_sentinel_otlp_spans_filtered_total`.
  Single-layer emitters (Laravel, Django, Rails) are unchanged. No chart
  template change, this bump tracks the new appVersion.

## [0.9.8]

### Added

- `appVersion` bumped to `0.9.8`. OTLP ingest now admits RPC spans (OTel
  RPC semconv `rpc.system`, `rpc.service`, `rpc.method`, the standard
  gRPC shape). These carried neither `db.statement`/`db.query.text` nor
  `http.url`/`url.full`, so the I/O filter dropped every one as non-I/O
  and the topological and occurrence detectors were blind on gRPC-heavy
  fleets. RPC spans are keyed on `rpc.system` and modeled as outbound
  calls (target `rpc.service/rpc.method`, span-name fallback), admitting
  only `SpanKind::Client` so the inbound handler span is not
  double-counted. Findings surface under the `_http` types. No chart
  template change, this bump tracks the new appVersion.

### Changed

- **Breaking (HTTP acknowledgment signatures reset).** The HTTP
  normalizer keeps the callee host in the grouping template for
  DNS-addressed calls (`GET user-svc/api/users/{id}` instead of
  `GET /api/users/{id}`), so two calls to the same path on different
  backend services no longer collapse into a false `redundant_http`.
  IP-literal authorities keep deduping load-balanced replicas, the port
  and userinfo are dropped, a trailing DNS root dot is canonicalized,
  and the host is lowercased. Because the finding signature hashes
  `pattern.template`, every outbound-HTTP finding's signature changes, so
  existing HTTP acknowledgments must be re-created (SQL and RPC findings
  are unaffected). No chart template change.
- `artifacthub.io/images` annotation bumped to
  `ghcr.io/robintra/perf-sentinel:0.9.8` to keep the Artifact Hub
  display metadata in lockstep with `appVersion`.

## [0.9.7]

### Fixed

- `appVersion` bumped to `0.9.7`. Batch `analyze --input` now accepts
  OTLP/JSON list attributes serialized the canonical protobuf JSON way,
  with the empty repeated field omitted (`"arrayValue":{}` or
  `"kvlistValue":{}`). Previously one such attribute failed the whole
  file with a `missing field values` error and a non-zero exit, so
  nothing was analyzed. No chart template change, this bump tracks the
  new appVersion.
- `artifacthub.io/images` annotation bumped to
  `ghcr.io/robintra/perf-sentinel:0.9.7` to keep the Artifact Hub
  display metadata in lockstep with `appVersion`.

## [0.9.6]

### Changed

- `appVersion` bumped to `0.9.6`. The embedded carbon-intensity table
  switches its nationally-gridded rows from Electricity Maps
  consumption-based 2023-2024 means to Ember yearly generation-based
  national data (latest year per country), regenerated semiannually by
  the new `refresh-datasets` workflow through a reviewed PR. Low-carbon
  grids move the most (Sweden 8 to 35.4, Finland 8 to 57.5, Norway 7 to
  28.1 gCO2eq/kWh), the generation-vs-consumption accounting difference
  documented in `docs/METHODOLOGY.md`. Hourly profiles are renormalized
  to the new annual levels, shapes unchanged. The SPECpower instance
  table is now generated from the CCF coefficient CSVs (the m6a/c6a
  families align to the CCF EPYC 3rd Gen coefficient, 26 rows lose
  hand-rounding slips of at most 0.1 W). Carbon outputs shift for the
  affected regions. `SPECPOWER_VINTAGE` is unchanged, so operator TOMLs
  pinning `specpower_table_version` are unaffected. No chart template
  change, this bump tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to
  `ghcr.io/robintra/perf-sentinel:0.9.6` to keep the Artifact Hub
  display metadata in lockstep with `appVersion`.

## [0.9.5]

### Added

- New `PerfSentinelMemoryPressureRejecting` alert in the opt-in
  `PrometheusRule`, state-based on `perf_sentinel_ingest_memory_pressure == 1`,
  so it keeps firing while the daemon holds ingest to protect RSS even
  after exporters stop retrying. Its remediation points at
  `resources.limits.memory` or more replicas, not the ingest queue. Pairs
  with the new daemon `[daemon] memory_high_water_pct` knob.

### Changed

- `PerfSentinelIngestRejecting` scoped to `reason!="memory_pressure"`, so it
  still alerts on `channel_full`, `parse_error`, and `unsupported_media_type`
  while memory-pressure rejections are owned by the new dedicated alert.
- appVersion 0.9.5. The binary adds OTLP JSON auto-detection to batch
  `analyze --input` (single object or Collector `file` exporter NDJSON)
  and a new `mysql-stat` subcommand ranking MySQL hotspots from
  `performance_schema.events_statements_summary_by_digest` exports, with
  a matching `mysql_stat` tab in the self-contained HTML dashboard. An
  opt-in daemon memory-pressure guard (`[daemon] memory_high_water_pct`,
  default off) rejects OTLP ingest when cgroup v2 memory crosses the
  configured high-water mark, bounding RSS against OOM. The toolchain
  moves to Rust 1.96.1.

## [0.9.4]

### Changed

- `appVersion` bumped to `0.9.4`. The daemon binary adds PHP framework-aware `suggested_fix`: findings on Laravel/Eloquent and Symfony/Doctrine stacks (detected via the native `io.opentelemetry.contrib.php.*` instrumentation scopes or a `.php` source path) carry framework-specific remediation, with a PHP generic fallback across all ten anti-patterns. The sanitizer-aware N+1 classifier now treats the Laravel and Doctrine scopes as ORM markers, so an obfuscated Eloquent/Doctrine N+1 classifies as `n_plus_one_sql` under the default `auto` mode. dd-trace-php bridged through the Collector `datadogreceiver` carries no framework signal. No chart template change, this bump tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.9.4` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.9.3]

### Changed

- `appVersion` bumped to `0.9.3`. The daemon binary adds a Datadog / dd-trace ingestion path: teams with no OpenTelemetry SDK can bridge dd-trace through an OTel Collector running the `datadogreceiver`, and perf-sentinel reads the SQL from `dd.span.Resource` natively. It also recognizes the stable OTel 1.27+ `db.system.name` attribute across the OTLP, Jaeger and Zipkin ingest paths, with consistent engine-label canonicalization. No chart template change, this bump tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.9.3` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.9.2]

### Changed

- `appVersion` bumped to `0.9.2`. The daemon binary adds Ruby/Active Record framework-aware suggestions, drops non-SQL datastore spans (Redis, MongoDB and similar) at ingestion under a dedicated `non_sql_datastore` OTLP filter reason, tokenizes MySQL backtick identifiers, and stops embedding the raw `db.statement` in the self-contained HTML report. No chart template change, this bump tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.9.2` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.9.1]

### Changed

- `appVersion` bumped to `0.9.1`. The daemon binary updates `opentelemetry_sdk` to 0.32.1, resolving CVE-2026-48504 (unbounded memory allocation in W3C Baggage propagation). No chart template change, this bump tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.9.1` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.9.0]

### Changed

- Chart version realigned to track the perf-sentinel application version. From this release the chart `version` and `appVersion` move in lockstep (both `0.9.0`), replacing the previously independent `0.2.x` chart line.
- `appVersion` bumped to `0.9.0`. The self-contained HTML dashboard is rebuilt as an application shell (sidebar navigation, Overview landing page, Findings master/detail with the Explain trace tree inline, severity-tinted KPI cards, syntax highlighting, embedded Geist fonts). No chart template change, this bump tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.9.0` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.63]

### Changed

- `appVersion` bumped to `0.8.14`. The HTML dashboard dark theme darkens its secondary panel background from `#2c2c2c` to `#212121`, giving metric cards, rows and the topbar more contrast against the primary background. Cosmetic, dark theme only. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.14` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.62]

### Removed

- Dropped the `PerfSentinelFindingsStoreNearCap` alert from the `PrometheusRule`. The findings store is a bounded ring buffer that evicts the oldest entry by design, and `perf_sentinel_findings_total` keeps the authoritative cumulative count, so reaching the cap is normal operation rather than data loss. On a busy daemon the gauge pins at its cap and the alert fired continuously, which is noise. The eviction signal that does matter, `PerfSentinelCorrelatorEvicting` (cross-trace correlations actually dropped), is kept. No `appVersion` change.

## [0.2.61]

### Changed

- `appVersion` bumped to `0.8.13`. The daemon's carbon report now emits the SCI per-functional-unit intensity (`co2.sci_per_trace`) alongside the numerator footprint, each detector maps to the RGESN 2024 criteria it relates to, and the periodic disclosure schema (`perf-sentinel-report/v1.3`) gains an interpretive ESRS E1 datapoint crosswalk and per-pattern RGESN criteria. All additions are backward compatible. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.13` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.60]

### Added

- Opt-in `PrometheusRule` (`prometheusRule.enabled`, off by default) packaging the daemon's loss and saturation alerts so "alerts the moment a problem appears" works out of the box instead of being a build-it-yourself wiring exercise. The `perf-sentinel.rules` group covers the daemon being unreachable (`absent(perf_sentinel_active_traces)`), OTLP rejection, analysis shedding, analysis-queue saturation, the findings store nearing its cap, correlator-pair eviction, and service-cardinality overflow, with each alert's `description` naming the `[daemon]` knob to raise. Per-backend energy-scraper staleness alerts are gated behind `prometheusRule.energyScrapers` (off by default, only meaningful when an energy backend is configured), and `prometheusRule.additionalRules` appends custom rules verbatim. No application change, `appVersion` stays `0.8.12`.
- Opt-in `PodDisruptionBudget` (`podDisruptionBudget.enabled`, off by default) for voluntary-disruption protection during node drains and cluster upgrades. The default is `maxUnavailable: 1` rather than `minAvailable: 1`, since the daemon runs single-replica and a `minAvailable: 1` PDB would block every drain and wedge node maintenance. Set `minAvailable` only for a trace-aware sharded topology.

## [0.2.59]

### Changed

- The post-install notes now warn when `workload.replicas` exceeds 1 on a non-DaemonSet workload. perf-sentinel keeps per-trace and correlation state in memory, per pod, with no shared state, so a round-robin Service splits one trace's spans across pods and silently degrades N+1 and correlation detection. The note points operators at `replicas=1` or trace-aware routing (consistent hashing by `trace_id` via the OTel Collector `loadbalancingexporter`).
- The default resource requests and limits are raised to match the measured daemon footprint (requests `50m`/`64Mi`, limits `500m`/`256Mi`). The daemon idles near 17Mi but holds 150-190Mi RSS under sustained ingestion, so the previous `64Mi` limit risked an OOMKill under load. The raw-manifest baseline in `docs/INSTRUMENTATION.md` is mirrored. No application change, `appVersion` stays `0.8.12`.

## [0.2.58]

### Changed

- `appVersion` bumped to `0.8.12` to track the `query monitor` Trends tab fix. The Energy, Carbon and headroom charts now plot in a fixed-width time window instead of compressing the curves as history accumulates, so each new sample scrolls them leftward at a constant rate and the time-span axis labels stay put. Before the window fills, its left part stays empty rather than zooming in on the few points collected so far. CLI/TUI-only change, no daemon behavior change. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.12` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.57]

### Fixed

- Enabling StatefulSet persistence now wires the daemon's durable state to the mounted PVC. The ConfigMap points `[daemon.ack] storage_path` at `/var/lib/perf-sentinel/acks.jsonl` and `[daemon.archive] path` at `/var/lib/perf-sentinel/archive.ndjson` whenever `workload.kind` is `StatefulSet` and `workload.statefulset.persistence.enabled` is true, so runtime acknowledgments and the public-disclosure archive survive pod restarts and rescheduling. Previously the volume mounted at `/var/lib/perf-sentinel` but no config pointed at it, so the PVC stayed empty, the ack store fell back to a non-writable default under `readOnlyRootFilesystem`, and disclose archiving was off. The injection is gated on the volume actually being mounted, since an unwritable `[daemon.archive]` path is a fatal startup error. No application change, `appVersion` stays `0.8.11`.

## [0.2.56]

### Changed

- `appVersion` bumped to `0.8.11` to track mouse-drag-resizable panels in the interactive TUIs: the `inspect` drill-down (Traces, Findings, Correlations, Detail) and the `query monitor` Trends tab let you drag panel borders to redistribute space, `m` toggles mouse capture (opt-in, so native terminal copy-paste stays available when off), `r` resets the layout, and hovering a border highlights it with a handle glyph (the in-app stand-in for a resize cursor, since a terminal cannot change the OS mouse pointer). Panel sizes are per-session. CLI/TUI-only change, no daemon behavior change. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.11` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.55]

### Changed

- `appVersion` bumped to `0.8.10` to track `perf-sentinel demo --tui` and `--html`: the bundled demo can now open the interactive TUI report or write the self-contained HTML dashboard in addition to the colored terminal report. The `--html` output is a full showcase with every dashboard tab populated from embedded fixtures (findings across all detector types, Explain span trees, GreenOps, a pg_stat ranking, a Diff against an embedded baseline run, and synthesized cross-trace correlations), and the embedded demo dataset now exercises all ten detector types across the three severity levels. CLI-only change, no daemon behavior change. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.10` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.54]

### Changed

- `appVersion` bumped to `0.8.9` to track the demo quality-gate annotation: `perf-sentinel demo` now marks its failed gate verdict as informational ("Quality gate: FAILED (informational in demo, would exit 1 under analyze --ci)"), since the demo never enforces the gate and always exits 0. The annotation is console-only and demo-only, `analyze --ci` exit behavior and every machine export (JSON, SARIF, HTML, NDJSON) are unchanged. The documented crates.io install command now recommends `cargo install perf-sentinel --locked` for reproducible installs. CLI-only change, no daemon behavior change. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.9` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.53]

### Changed

- `appVersion` bumped to `0.8.8` to track `query monitor`, a new read-only live operator TUI (`perf-sentinel query --daemon <URL> monitor`) with five Tab-cycled tabs (settings-advisor hints, the effective energy/carbon mix per service and per region, live Trends charts for energy, carbon and runtime headroom, per-backend energy-scraper health, and every `[daemon]` parameter with its default and an explanation), and the daemon surface behind it: two new read-only endpoints (`GET /api/config`, an explicit allowlist that summarizes TLS and ack secrets to booleans and never echoes paths or keys, and `GET /api/energy`, per-backend scraper health), an extended `GET /api/status` (runtime caps and live queue, window and findings depths), and six new scalar `/metrics` gauges (`perf_sentinel_energy_kwh`, `perf_sentinel_carbon_gco2`, `perf_sentinel_max_active_traces`, `perf_sentinel_analysis_queue_capacity`, `perf_sentinel_max_retained_findings`, `perf_sentinel_stored_findings`) with three matching Grafana panels (energy and carbon per scoring window, runtime headroom against a 90% threshold). Terminal-output sanitization now also strips BiDi reordering and invisible characters, and the self-contained HTML dashboard embed is lighter (about 6.4 MB down to 5.0 MB on a 15k-finding report). All additions are backward compatible, older daemons degrade gracefully. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.8` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.52]

### Changed

- `appVersion` bumped to `0.8.7` to track four new daemon loss-observability counters (`perf_sentinel_otlp_spans_received_total`, `perf_sentinel_otlp_spans_filtered_total{reason}`, `perf_sentinel_service_io_ops_overflow_total`, `perf_sentinel_correlator_pairs_evicted_total`), a primary-source refresh of the embedded carbon data (Paris 56 to 41 gCO2eq/kWh, Sao Paulo 62 to 96, Belgium 187 to 165, the eu-central-1 hourly profile rescaled to current grid levels, generic PUE 1.2 to 1.5), and the high-scale hardening from the new limit-testing campaign: the cross-trace correlator admission-controls pairs inside a batch (a 1500-service topology used to OOM the 256Mi pod in about a minute, now flat at ~57 MiB), and the OTLP routes bound concurrent decode so `/health` stays responsive under saturation floods (excess requests wait on an in-process semaphore on both the HTTP and gRPC paths, and the ingest enqueue rejects after a 2-second bounded wait, so senders get fast retryable rejections (HTTP 503, gRPC UNAVAILABLE) instead of the kubelet restarting a functional daemon). The daemon also gains a settings advisor: `/api/export/report` emits `tuning` entries in `Report.warning_details` when lifetime counters show a config knob undersized for the observed load, naming the knob, its current value and the suggested adjustment. Carbon outputs shift for the affected regions. Deployments under sustained saturation should give the liveness probe headroom (`timeoutSeconds: 5`, `failureThreshold: 5`). No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.7` to keep the Artifact Hub display metadata in lockstep with `appVersion`.

## [0.2.51]

### Changed

- `appVersion` bumped to `0.8.6` to track configurable daemon queue depths (`[daemon] ingest_queue_capacity` and `analysis_queue_capacity`, both default 1024, range 1 to 1,048,576) and an `Arc`-shared carbon context that drops a per-batch clone on the streaming hot path. No chart template change, this bump only tracks the new appVersion.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.6` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.50]

### Changed

- `appVersion` bumped to `0.8.5` to track graceful in-flight window drain on `SIGTERM`: a normal Kubernetes pod termination (rolling update, scale-down) now flushes the daemon's streaming window through detection instead of dropping it, matching the existing `SIGINT` (Ctrl+C) behavior. Only an ungraceful kill (`SIGKILL` after the grace period, OOM) still loses it, so keep `terminationGracePeriodSeconds` above the configured window duration to benefit. The Windows daemon binary and core test suite also build and run now. Template surface is unchanged, no migration needed beyond the chart bump. No config or daemon wire change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.5` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.49]

### Changed

- `appVersion` bumped to `0.8.4` to track the new `man` subcommand (renders the manual page to stdout) and the usage-example blocks added to every user-facing command's `--help`. Template surface is unchanged, no migration needed beyond the chart bump. No config or daemon wire change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.4` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.48]

### Changed

- `appVersion` bumped to `0.8.3` to track the temporal-coverage continuity signal added to the periodic public disclosure (`aggregate.temporal_coverage`, schema `perf-sentinel-report/v1.2`), the in-band `coverage_basis` provenance marker, and the reserved `integrity.cross_period_log` hook. The schema change is additive, v1.1 and v1.0 consumers are unaffected. Template surface is unchanged, no migration needed beyond the chart bump. No config or daemon wire change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.3` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.47]

### Changed

- `appVersion` bumped to `0.8.2` to track the two-tier avoidable energy and carbon breakdown added to the periodic public disclosure (a canonical N+1 threshold pinned in the binary that the operator cannot configure, next to the operational threshold, schema v1.1) and the ratatui 0.30.1 TUI backend bump. Template surface is unchanged, no migration needed beyond the chart bump. The report JSON schema gains the additive v1.1 waste tiers, v1.0 consumers are unaffected. No config or daemon wire change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.2` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.46]

### Changed

- `appVersion` bumped to `0.8.1` to track a patch release with no daemon-facing change. The SQL tokenizer slice path gains a debug-only char-boundary assertion (compiled out in release) as a refactor guard, disclose state initialization is simplified, and the release tooling and docs are extended (Helm chart release automation script, a design note on why the release profile omits `overflow-checks`). Template surface is unchanged, no migration needed beyond the chart bump. All changes are additive, no config, default CLI output, daemon wire, or report JSON change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.1` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.45]

### Changed

- `appVersion` bumped to `0.8.0` to track the unified multi-view inspect TUI: two new views (Analyze, the GreenOps summary, and Explain, the full-screen annotated span tree) join the existing Inspect browser with an Enter/Esc drill-down, plus `analyze --tui` / `explain --tui` launch flags and a live Analyze view fed from `/api/export/report`. Template surface is unchanged, no migration needed beyond the chart bump. All changes are additive, no config, default CLI output, daemon wire, or report JSON change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.8.0` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.44]

### Changed

- `appVersion` bumped to `0.7.8` to track the daemon's `suggested_fix` coverage improvements (vendor-specific OTel scope recognition for .NET EF Core and Quarkus, service-name fallback) and the HTTP N+1 sanitizer-aware classification. Template surface is unchanged, no migration needed beyond the chart bump. All changes are additive, no config, CLI, daemon wire, or report JSON change.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.7.8` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.43]

### Changed

- `appVersion` bumped to `0.7.7` to track the daemon's bare-driver n+1 sql fix in strict sanitizer-aware mode and the µs-precision fix applied to the `serialized_calls` and `pool_saturation` detectors. Template surface is unchanged, no migration needed beyond the chart bump. Operators with strict `quality_gate` rules should review the upgrade notes in the daemon `CHANGELOG.md` for the recommended re-baseline step on `n_plus_one_sql_critical_max`, `serialized_calls_critical_max` and `pool_saturation_critical_max`.
- `artifacthub.io/images` annotation bumped to `ghcr.io/robintra/perf-sentinel:0.7.7` to keep the Artifact Hub display metadata in lockstep with `appVersion`. Runtime image selection is unaffected (templates already resolve to `.Chart.AppVersion` when `values.yaml` `image.tag` is empty).

## [0.2.42]

### Changed

- Re-publication of chart-v0.2.41 to recover the supply-chain attestations
  that were skipped on the original release. `appVersion` stays at `0.7.6`,
  template surface is byte-for-byte identical to chart-v0.2.41, the only
  diff is this `version:` bump and this CHANGELOG entry. The chart-v0.2.41
  helm-release workflow run succeeded the `helm push` step but tripped on
  an empty digest returned by `crane digest` immediately after, which
  stopped the job and skipped the downstream Cosign keyless signature,
  SPDX SBOM, and SLSA Build L3 provenance attestation jobs. The chart
  binary was already on GHCR, but operators verifying provenance with
  `cosign verify` against chart-v0.2.41 fail. chart-v0.2.42 runs the full
  pipeline against the same template content so the attestations exist
  for installs going forward. Operators on chart-v0.2.41 can upgrade
  to chart-v0.2.42 in place, the rendered manifests are identical, no
  `helm upgrade` migration is needed beyond the chart bump. The
  workflow regression is fixed in [#33](https://github.com/robintra/perf-sentinel/pull/33),
  the digest is now parsed from `helm push` output directly instead of
  a separate `crane digest` call that races against GHCR's
  eventually-consistent tag index.

## [0.2.41]

### Changed

- `appVersion` bumped to `0.7.6`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.6`. The
  `artifacthub.io/images` annotation is updated in lockstep. The 0.7.6
  binary ships two breaking changes on the Scaphandre and Redfish
  energy sources. `[green.scaphandre].process_map` becomes a typed
  table per service with `exe_contains` plus optional
  `cmdline_contains` substrings, replacing the previous flat
  `"service" = "exe"` form. Substring matching on both labels
  disambiguates co-located JVMs or .NET assemblies sharing a runtime.
  `[green.redfish].endpoints` becomes a typed table per chassis with
  `url` plus `schema` enum (`legacy_power` or `environment_metrics`),
  the top-level `power_path` field is removed. EnvironmentMetrics
  (`PowerWatts.Reading`) is now natively supported alongside the
  legacy `/Power` resource. Both new struct shapes deny unknown TOML
  fields, so a typo or a stale legacy field fails at config load
  rather than degrading silently. Operators upgrading from 0.7.5 must
  rewrite the affected sections (only operators who explicitly
  configured `[green.scaphandre]` or `[green.redfish]` are
  impacted, default configs are unaffected). The Scaphandre scraper
  also gains a zero-sample warn-once net mirroring the Kepler net
  delivered in 0.7.5, and the staleness gauge now climbs from boot on
  failure for parity with Kepler. No chart-level template diff,
  `values.yaml` schema is byte-for-byte identical to chart-v0.2.40.

## [0.2.40]

### Fixed

- `appVersion` bumped to `0.7.5`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.5`. The
  `artifacthub.io/images` annotation is updated in lockstep. The 0.7.5
  binary aligns the Kepler scraper to the Kepler v2 upstream metric
  names: the container variant now reads
  `kepler_container_cpu_joules_total` (was
  `kepler_container_joules_total`), and the `metric_kind = "process"`
  value replaces the previous `process_package` / `process_dram`
  values that targeted metrics Kepler never published. The Process
  variant reads `kepler_process_cpu_joules_total` keyed by the `comm`
  label. Operators with `metric_kind = "process_package"` or
  `metric_kind = "process_dram"` in their config get a clear error at
  startup pointing at `metric_kind = "process"`. The Redfish BMC
  integration is unchanged. No chart-level template diff,
  `values.yaml` schema is byte-for-byte identical to chart-v0.2.39.

## [0.2.39]

### Added

- `appVersion` bumped to `0.7.4`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.4`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.7.4 binary adds two opt-in measured-energy sources that extend
  the carbon attribution stack on ARM64 and bare-metal hosts. Kepler
  (`kepler_ebpf`) reads eBPF per-container or per-process joule
  counters, either directly from a Kepler exporter or through
  Prometheus when Kepler runs as a DaemonSet. Redfish BMC
  (`redfish_bmc`) reads chassis wall-plug watts from the baseboard
  management controller and attributes them per service
  proportionally to ops-deltas, with operator-supplied CA bundle
  support (`ca_bundle_path`) for self-signed certificates. The
  carbon precedence chain becomes `electricity_maps_api >
  scaphandre_rapl > kepler_ebpf > redfish_bmc > cloud_specpower >
  io_proxy_v3 > io_proxy_v2 > io_proxy_v1`. Two new Prometheus
  gauges are exposed,
  `perf_sentinel_kepler_last_scrape_age_seconds` and
  `perf_sentinel_redfish_last_scrape_age_seconds`. Both sources are
  off by default, the `+cal` calibration suffix continues to apply
  to proxy models only.

## [0.2.38]

### Changed

- `appVersion` bumped to `0.7.3`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.3`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.7.3 binary refreshes the GreenOps reference data shipped with
  the chart: per-provider PUE constants aligned with the latest
  sustainability reports (AWS 1.135 to 1.15, GCP 1.10 to 1.09,
  Azure 1.185 to 1.17, generic unchanged at 1.2), and cloud instance
  coefficients aligned with the `ccf-coefficients` 2026-04-24
  snapshot across AWS, GCP, and Azure (about 390 instance types).
  New AWS families covered: `m8a` / `c8a` (EPYC Turin, proxied to
  Genoa pending an upstream CCF correction), `m8i` / `c8i` (Emerald
  Rapids), `r7a` (Genoa memory-optimized). New GCP family: `c4a`
  (Axion ARM). Memory-optimized SKUs now carry an additive DRAM
  premium (0.02 W/GB idle, 0.05 W/GB max). The binary `SPECpower`
  vintage is surfaced in periodic disclosure reports under
  `methodology.calibration_inputs.binary_specpower_vintage` and
  cross-checked against the operator-declared
  `specpower_table_version` for Official intent reports.

## [0.2.37]

### Added

- `appVersion` bumped to `0.7.2`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.2`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.7.2 binary adds the `hash-bake` subcommand: it reads a periodic
  disclosure report, computes the canonical `content_hash` applying
  the `POST_SIGN_FIELDS` blanching, writes the hash into
  `integrity.content_hash`, and saves the result via an atomic
  temp+rename. Intended for test fixture generation and debugging
  workflows where a report needs a hash matching what perf-sentinel
  itself would write. Signed reports (those carrying
  `integrity.signature`) are rejected by default with exit 1, opt-in
  via `--allow-signed`. Exit codes: 0 success, 1 refused, 3 input
  error.

## [0.2.36]

### Changed

- `appVersion` bumped to `0.7.1`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.1`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.7.1 binary migrates the SLSA build provenance tooling from
  `slsa-framework/slsa-github-generator@v2.1.0` (in de-facto
  maintenance since 2025-02, internal actions stuck on Node.js 20
  while GitHub-hosted runners switch to Node 24 default on
  2 June 2026) to GitHub-native `actions/attest-build-provenance`.
  The new pipeline stores the attestation in the GitHub attestations
  API instead of the previous release asset
  `multiple.intoto.jsonl`, and the SLSA level claim moves from L2 to
  L3 because the new action produces a level-3 attestation by
  construction. The daemon advisory warning on
  `[reporting] disclose_output_path` is now emitted exactly once at
  startup, fixing the double-emit observed on 0.7.0 when CLI listen
  overrides re-ran `Config::validate()`.

### Breaking

- **Consumer-side SLSA verification changes**. A script that did
  `curl ... multiple.intoto.jsonl && slsa-verifier verify-artifact --provenance-path multiple.intoto.jsonl ...`
  no longer works on 0.7.1+ binaries (the release asset is gone).
  Migration: `gh attestation verify <binary> --owner robintra --repo perf-sentinel`
  (requires `gh` CLI 2.49+). The v0.7.0 release retains its legacy
  attestation asset, the breaking change applies only to v0.7.1+.

## [0.2.35]

### Changed

- `appVersion` bumped to `0.7.0`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.7.0`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.7.0 binary ships the public periodic disclosure pipeline: a new
  `disclose` subcommand produces a single JSON report from archived
  NDJSON windows with an in-toto v1 attestation sidecar, and a new
  `verify-hash` subcommand validates content hash, Sigstore
  signature and SLSA binary provenance with five distinct exit
  codes (TRUSTED, UNTRUSTED, PARTIAL, INPUT_ERROR, NETWORK_ERROR).
  Breaking change: `verify-hash` now refuses to invoke cosign
  without `--expected-identity` / `--expected-issuer` (or explicit
  `--no-identity-check`), closing the autosigning gap where any
  GitHub or Google account holder could forge a bundle claiming an
  identity. Cosign commands migrated from `attest-blob` to
  `sign-blob --new-bundle-format`, requires cosign 2.4+ in the
  signing pipeline. A new `[reporting] disclose_output_path`
  configuration field is reserved for 0.8.0 daemon-triggered
  disclosures and logs an advisory at startup when set today.

## [0.2.33]

### Changed

- `appVersion` bumped to `0.6.1`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.6.1`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.6.1 binary ships an internal-audit-driven hardening pass: CORS
  `["*"]` combined with `[daemon.ack] api_key` is now rejected at
  config load (was a startup `WARN`), the CI ack TOML loader
  refuses to follow symlinks, the SARIF result body strips BiDi
  and invisible-format characters, and the OTLP gRPC listener
  caps HTTP/2 stream multiplexing at 256 per connection. Plus
  hot-path tightening across the detection and scoring stages
  (single-pass chatty, unstable sort in serialized, pre-sized
  HTTP query-param vec, right-sized avoidable-finding dedup).
  Dependency bumps: `opentelemetry-proto` 0.31 to 0.32, `tonic`
  0.14.5 to 0.14.6, `tokio` 1.52.2 to 1.52.3. No chart-level
  template change.

## [0.2.32]

### Changed

- `appVersion` bumped to `0.6.0`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.6.0`. The 0.6.0
  binary removes the eight legacy top-level config keys deprecated
  in 0.5.26 and restructures the `Config` type around the
  `[thresholds]`, `[detection]`, `[green]` and `[daemon]`
  sections. Operators still on the flat form (a startup `WARN`
  has been firing since 0.5.26) must migrate before upgrade,
  config load now errors out instead of warning. No chart-level
  template change.

## [0.2.31]

### Changed

- `appVersion` bumped to `0.5.28`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.28`. The
  `artifacthub.io/images` annotation is updated in lockstep.
- **Breaking change in 0.5.28**: the finding signature format moves
  from a 16-hex SHA-256 prefix (~64 bits) to a 32-hex prefix (~128
  bits). Existing `.perf-sentinel-acknowledgments.toml` files with
  16-hex signatures stop matching after upgrade, the daemon JSONL
  ack store contains entries that no longer match either. Operators
  should flush the JSONL store at upgrade time and re-ack the
  surviving findings under the new format. See the upstream
  CHANGELOG for the full migration note. No chart-level template
  change beyond the image tag.

## [0.2.30]

### Changed

- `appVersion` bumped to `0.5.27`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.27`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.27 binary lands a hardening pass on the CLI output paths and
  the daemon ack flow, plus a TUI refactor that eliminates the UI
  freeze during ack/revoke (`a` / `u` keys in `query inspect`).
  Operator-visible new behaviors include a startup `WARN` when
  `[daemon.cors] allowed_origins = ["*"]` is combined with
  `[daemon.ack] api_key`, a render-time `WARN` for
  `--daemon-url http://...` on a non-loopback host, a
  `ps`-visibility `WARN` for `--auth-header` on the `tempo` and
  `jaeger-query` subcommands, and 1 KiB caps on the `ack create`
  stdin signature read and the interactive API-key prompt. CLI
  write paths (HTML report, calibration TOML, diff `--output`) now
  use `O_NOFOLLOW` on Unix. No chart-level template change. See
  `docs/HTML-REPORT.md`, `docs/CONFIGURATION.md` and `docs/CLI.md`.

## [0.2.29]

### Changed

- `appVersion` bumped to `0.5.26`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.26`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.26 binary adds soft-deprecation warnings for 8 legacy top-level
  config keys (`n_plus_one_threshold`, `window_duration_ms`,
  `listen_addr`, `listen_port`, `max_active_traces`, `trace_ttl_ms`,
  `max_events_per_trace`, `max_payload_size`). Operators see a
  `WARN`-level event with structured fields `legacy_key` and
  `replacement` at daemon startup if the mounted `.perf-sentinel.toml`
  still uses the flat form. Behavior is preserved bit-for-bit, the
  sectioned form takes precedence when both are set. No chart-level
  template change. Migration table in `docs/CONFIGURATION.md`.

## [0.2.28]

### Changed

- `appVersion` bumped to `0.5.25`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.25`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.25 binary adds two Prometheus counters on the daemon
  Scaphandre scraper (`perf_sentinel_scaphandre_scrape_total{status}`
  and `perf_sentinel_scaphandre_scrape_failed_total{reason}`)
  pre-warmed at startup so dashboards build with `rate()` queries
  without `absent()` guards. No chart-level template change. See
  `docs/METRICS.md` Scaphandre scrape counters section for the label
  values and sample PromQL queries.

## [0.2.27]

### Changed

- `appVersion` bumped to `0.5.24`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.24`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.24 binary adds interactive ack/revoke from the TUI
  (`perf-sentinel query inspect`, with `a` and `u` keybindings on
  the selected finding) and a new `--api-key-file` flag on
  `query inspect`. The daemon HTTP surface is unchanged, the TUI
  consumes the existing ack endpoints. No chart-level config
  change. This release closes the three-axis UX marathon over the
  daemon ack API (CLI 0.5.22, HTML 0.5.23, TUI 0.5.24). See
  `docs/INSPECT.md` and `docs/ACK-WORKFLOW.md` for the user-facing
  reference.

## [0.2.26]

### Changed

- `appVersion` bumped to `0.5.23`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.23`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.23 binary adds an opt-in HTML report live mode (the new
  `perf-sentinel report --daemon-url <URL>` flag) and an opt-in
  daemon CORS layer (`[daemon.cors] allowed_origins`). The daemon
  HTTP surface is otherwise unchanged. The static HTML report
  generated without `--daemon-url` is fully byte-equivalent to the
  0.5.22 output. No chart-level config change. See `docs/HTML-REPORT.md`
  and the `[daemon.cors]` section in `docs/CONFIGURATION.md` for the
  user-facing reference.

## [0.2.25]

### Changed

- `appVersion` bumped to `0.5.22`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.22`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.22 binary ships a new `perf-sentinel ack` CLI subcommand for
  the daemon ack API, with `create`, `revoke`, `list` actions and
  flexible auth / duration parsing. The daemon HTTP surface is
  unchanged, the new CLI consumes the existing endpoints. No
  chart-level config change. See `docs/CLI.md` and
  `docs/ACK-WORKFLOW.md` for the user-facing reference.

## [0.2.24]

### Changed

- `appVersion` bumped to `0.5.21`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.21`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.21 daemon adds two Prometheus counters on `/metrics` for
  observability of operator-driven activity on the ack endpoints
  (`perf_sentinel_ack_operations_total` and
  `perf_sentinel_ack_operations_failed_total`). No chart-level config
  change, scrapers pick up the new series automatically. See
  `docs/METRICS.md` for the label set and pre-warmed combinations.

## [0.2.23]

### Changed

- `appVersion` bumped to `0.5.20`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.20`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.19` if you pin. The 0.5.20 daemon adds the runtime ack API
  (`POST` / `DELETE` / `GET /api/findings/{sig}/ack` and
  `GET /api/acks`); operators wiring the new endpoints from outside
  the cluster should review the loopback-by-default posture and the
  optional `[daemon.ack] api_key` setting (see `docs/QUERY-API.md`).

## [0.2.22]

### Changed

- `appVersion` bumped to `0.5.19`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.19`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.18` if you pin.

## [0.2.21]

### Changed

- `appVersion` bumped to `0.5.18`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.18`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.17` if you pin.

## [0.2.20]

### Changed

- `appVersion` bumped to `0.5.17`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.17`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.16` if you pin.

## [0.2.19]

### Changed

- `appVersion` bumped to `0.5.16`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.16`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.15` if you pin.

## [0.2.18]

### Changed

- `appVersion` bumped to `0.5.15`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.15`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.14` if you pin.

## [0.2.17]

### Changed

- `appVersion` bumped to `0.5.14`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.14`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.13` if you pin.

## [0.2.16]

### Changed

- `appVersion` bumped to `0.5.13`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.13`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.12` if you pin.

## [0.2.15]

### Changed

- `appVersion` bumped to `0.5.12`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.12`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.11` if you pin.

## [0.2.14]

### Changed

- `appVersion` bumped to `0.5.11`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.11`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.10` if you pin.

## [0.2.13]

### Changed

- `appVersion` bumped to `0.5.10`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.10`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.9` if you pin.

## [0.2.12]

### Changed

- `appVersion` bumped to `0.5.9`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.9`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.8` if you pin.

## [0.2.11]

### Changed

- `appVersion` bumped to `0.5.8`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.8`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.7` if you pin.

## [0.2.10]

### Changed

- `appVersion` bumped to `0.5.7`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.7`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.6` if you pin.

## [0.2.9]

### Changed

- `appVersion` bumped to `0.5.6`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.6`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.5` if you pin.

## [0.2.8]

### Changed

- `appVersion` bumped to `0.5.5`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.5`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.4` if you pin.

## [0.2.7]

### Changed

- `appVersion` bumped to `0.5.4`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.4`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.3` if you pin.

## [0.2.6]

### Changed

- Artifact Hub-facing `README.md` refreshed: the "Install from a local
  checkout" section no longer claims an OCI-published chart is pending,
  since OCI publication has been live since 0.2.0. The section now
  frames local-checkout install as the path for iterating on values or
  templates, and points to `docs/HELM-DEPLOYMENT.md` and the Artifact
  Hub badge for the OCI install. No `appVersion` or image tag change,
  the chart keeps pointing at `ghcr.io/robintra/perf-sentinel:0.5.3`.

## [0.2.5]

### Added

- `artifacthub.io/official: "true"` annotation on `Chart.yaml` so the
  chart can claim the Artifact Hub "Official" badge. The annotation
  alone does not flip the badge, Artifact Hub staff validate ownership
  before it activates.

## [0.2.4]

### Changed

- Chart `description` and Artifact Hub-facing `README.md` intro now
  read "distributed traces" instead of "OpenTelemetry traces",
  reflecting that perf-sentinel also ingests Jaeger, Zipkin, Tempo and
  pg_stat_statements feeds, not only OTLP.
- `[daemon] environment` row in the "Chart at a glance" table rewords
  the `confidence` tag description: consumed by downstream tooling,
  with perf-lint called out as a planned companion IDE integration
  (not yet published, dead GitHub link removed).
- No `appVersion` or image tag change, the chart keeps pointing at
  `ghcr.io/robintra/perf-sentinel:0.5.3`.

## [0.2.3]

### Changed

- `appVersion` bumped to `0.5.3`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.3`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.2` if you pin.

## [0.2.2]

### Changed

- `appVersion` bumped to `0.5.2`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.2`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.1` if you pin.

## [0.2.1]

### Changed

- `artifacthub-repo.yml` now carries the Artifact Hub
  `repositoryID` (`70c507ff-c75e-4808-9c3b-87fb5696dce8`) and
  maintainer email. Artifact Hub re-scrapes the `:artifacthub.io`
  OCI tag on the next cycle after this tag is published and marks
  the repository as "Verified publisher".

## [0.2.0]

### Added

- Artifact Hub listing. The chart is now discoverable on
  artifacthub.io. `charts/perf-sentinel/artifacthub-repo.yml`
  documents the repository ownership and is pushed to the OCI
  registry under the special `artifacthub.io` tag on every release.
  `Chart.yaml` now carries `artifacthub.io/*` annotations
  (category, license, links, images).
- SPDX SBOM per release. Every chart release ships a Syft-generated
  SPDX SBOM as a GitHub Release asset, attested via
  `actions/attest` with the SBOM predicate and verifiable through
  `gh attestation verify --predicate-type https://spdx.dev/Document/v2.3`.

### Changed

- `docs/HELM-DEPLOYMENT.md` (and its French parity) gains an
  Artifact Hub section, an SBOM verification subsection, and an
  OCI-based `gh attestation verify oci://...` recipe alongside the
  existing tarball-based one.

## [0.1.2]

### Fixed

- SLSA build provenance is now reliably attached to each published
  chart release via `actions/attest-build-provenance`, replacing the
  previous `slsa-framework/slsa-github-generator` reusable workflow
  whose draft-release integration diverged onto ephemeral
  `untagged-*` drafts under the `helm-release.yml` release flow. The
  provenance is now queryable via `gh attestation verify` without
  requiring a separate `.intoto.jsonl` asset on the GitHub Release.
  Cosign signatures on the OCI artifact are unchanged.

### Changed

- `scripts/check-chart-version-bumped.sh` now also requires that
  `charts/perf-sentinel/CHANGELOG.md` contain a matching
  `## [NEW_VERSION]` section on HEAD that was not present on the PR
  base. A PR that bumps the chart version without a corresponding
  changelog entry now fails CI rather than merging silently.

## [0.1.1]

### Fixed

- `helm test` no longer races with CoreDNS warm-up on a freshly started
  cluster. The test Job's wget now retries up to 5 times over 15 seconds
  instead of giving up on the first `bad address` error when the Service
  DNS record has not yet propagated. Observed against minikube when
  chaining `minikube start + helm install + helm test` back to back.

## [0.1.0-rc.1]

First release candidate for the 0.1.0 chart. Chart content is identical
to the upcoming 0.1.0. The RC exists to dry-run the release pipeline:
OCI push to `ghcr.io/robintra/charts/perf-sentinel`, Cosign keyless
signing via the GitHub OIDC token, SLSA level 3 provenance on the
tarball, draft GitHub Release with both the `.tgz` and the
`.intoto.jsonl` as assets.

Promote to 0.1.0 once the RC's OCI artifact verifies clean via
`cosign verify` and `slsa-verifier verify-artifact`.

## [0.1.0]

Initial release of the perf-sentinel Helm chart.

### Added

- Helm chart deployable as Deployment (default), DaemonSet or
  StatefulSet via `workload.kind`.
- ConfigMap-backed `.perf-sentinel.toml` with rolling update on
  content change via `checksum/config` pod annotation.
- ClusterIP Service exposing OTLP gRPC (4317) and OTLP HTTP (4318).
- Optional ServiceMonitor for Prometheus Operator users
  (`serviceMonitor.enabled`).
- Optional NetworkPolicy (`networkPolicy.enabled`), fail-closed by
  default when enabled.
- Liveness and readiness probes wired to the lock-free `/health`
  endpoint.
- Extension hooks: `extraEnv`, `extraEnvFrom`, `extraVolumes`,
  `extraVolumeMounts`, `extraArgs`.
- Values schema (`values.schema.json`) validating the `workload.kind`
  enum and shape of the main keys.
