# perf-sentinel exposed metrics

This document lists all metrics exposed by the perf-sentinel daemon on
`/metrics` (Prometheus text format). The endpoint serves both
`text/plain; version=0.0.4` (legacy Prometheus) and
`application/openmetrics-text; version=1.0.0` (OpenMetrics) via content
negotiation, and emits exemplars when finding-level traces are
available.

## Process metrics (since 0.5.19, Linux only)

Standard process collector metrics from the `prometheus` crate's
`process_collector` module. Registered only on Linux (the underlying
`procfs` reads return errors on macOS/Windows). Operators on non-Linux
hosts get `perf_sentinel_*` metrics only.

| Metric                          | Type    | Description                       |
|---------------------------------|---------|-----------------------------------|
| `process_resident_memory_bytes` | gauge   | RSS of the daemon process.        |
| `process_virtual_memory_bytes`  | gauge   | Virtual memory size.              |
| `process_open_fds`              | gauge   | Open file descriptors.            |
| `process_max_fds`               | gauge   | Maximum allowed file descriptors. |
| `process_start_time_seconds`    | gauge   | Unix timestamp of process start.  |
| `process_cpu_seconds_total`     | counter | Cumulative CPU time.              |

**Per-scrape cost.** The collector reads `/proc/self/{stat,status,limits}`
and walks `/proc/self/fd/` on every scrape. On a daemon with thousands
of long-lived OTLP connections plus outbound scrapers, the FD walk can
dominate at 1-5 ms per scrape. The Prometheus `Registry::gather()` lock
is held for the duration, so a slow collector blocks concurrent scrapes
when several scrapers (Prometheus + vmagent + sidecar) target the same
endpoint. Acceptable at the typical 15-60 second scrape interval, worth
noting for tighter intervals.

**Exposure scope.** When the operator binds the metrics endpoint to
`0.0.0.0` (Kubernetes Pod default for cluster-internal scraping), the
process metrics expose operationally sensitive signals: uptime via
`process_start_time_seconds` (patch / restart inference), file
descriptor pressure via `process_open_fds` and `process_max_fds`
(saturation oracle), and memory footprint via
`process_resident_memory_bytes`. Default `--listen-address` is
`127.0.0.1`, which scopes scraping to the same host or the Pod
itself. For cluster-wide scraping topologies, gate `/metrics` behind
a Kubernetes `NetworkPolicy` and prefer Prometheus-side mTLS so a
sibling Pod cannot read the daemon's process state freely.

## OTLP ingestion metrics

| Metric                              | Type    | Labels   | Description                                                                       |
|-------------------------------------|---------|----------|-----------------------------------------------------------------------------------|
| `perf_sentinel_otlp_rejected_total` | counter | `reason` | Total OTLP requests rejected by the daemon since start, by reason (since 0.5.19). |

`reason` label values:

- `unsupported_media_type` (HTTP only): `Content-Type` is not
  `application/x-protobuf`. perf-sentinel does not implement the
  JSON-encoded OTLP variant.
- `parse_error` (HTTP only): protobuf decode failed.
- `channel_full` (HTTP and gRPC): the event channel is saturated or
  closed and the daemon could not enqueue the batch. The HTTP path
  returns 503, the gRPC path returns `INTERNAL`.

All 3 reasons are pre-warmed to 0 at startup so dashboards can plot
zero-values before the first rejection.

`payload_too_large` is **not** counted by this metric. Tower-http
(`RequestBodyLimitLayer`) on the HTTP path and tonic
(`max_decoding_message_size`) on the gRPC path enforce the cap upstream
and return 413 / `RESOURCE_EXHAUSTED` before the application handler
runs. Operators concerned with payload size should monitor the upstream
proxy or gateway logs, or wire a tower-http rejection counter in their
own stack.

## Analysis and findings metrics

| Metric                                       | Type      | Labels             | Description                                                                                                                                                                                                                                                        |
|----------------------------------------------|-----------|--------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_findings_total`               | counter   | `type`, `severity` | Findings detected since daemon start. `type` mirrors the `Finding.finding_type` enum, `severity` is `critical` / `warning` / `info`. Carries OpenMetrics exemplars when a `trace_id` is available.                                                                 |
| `perf_sentinel_traces_analyzed_total`        | counter   | (none)             | Cumulative trace count processed by the event loop.                                                                                                                                                                                                                |
| `perf_sentinel_events_processed_total`       | counter   | (none)             | Cumulative event count processed by the event loop.                                                                                                                                                                                                                |
| `perf_sentinel_active_traces`                | gauge     | (none)             | Currently active traces in the sliding window.                                                                                                                                                                                                                     |
| `perf_sentinel_slow_duration_seconds`        | histogram | `type`             | Duration histogram for spans exceeding the slow threshold, by event `type` (`sql` or `http_out`). Buckets: 0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 5, 10, 30 seconds. Used by Grafana `histogram_quantile()` for accurate percentiles across sharded daemon instances. |
| `perf_sentinel_export_report_requests_total` | counter   | (none)             | Total `GET /api/export/report` requests. Includes cold-start responses (200 with empty envelope).                                                                                                                                                                  |

## Ack metrics (since 0.5.21)

Operator-driven activity on the daemon ack API
(`POST` / `DELETE /api/findings/{signature}/ack`). Read-only TOML
acks loaded from `.perf-sentinel-acknowledgments.toml` at daemon
startup are not counted, no operations occur after the initial load.

| Metric                                       | Type    | Labels             | Description                                                     |
|----------------------------------------------|---------|--------------------|-----------------------------------------------------------------|
| `perf_sentinel_ack_operations_total`         | counter | `action`           | Successful ack and unack operations.                            |
| `perf_sentinel_ack_operations_failed_total`  | counter | `action`, `reason` | Failed ack and unack operations, broken down by failure reason. |

`action` label values: `ack`, `unack`.

`reason` label values:

- `already_acked` (HTTP 409, `action=ack` only): signature already in
  the daemon JSONL, or covered by a TOML CI baseline that is still
  active. Both cases collapse on the same series.
- `not_acked` (HTTP 404, `action=unack` only): signature has no
  active daemon ack record.
- `unauthorized` (HTTP 401): `[daemon.ack] api_key` is set and the
  request is missing or has an invalid `X-API-Key` header. The
  series is pre-warmed at zero, so a non-zero value confirms
  `api_key` is configured (the counter only ever increments when
  auth is enforced).
- `no_store` (HTTP 503): daemon ack store is disabled
  (`[daemon.ack] enabled = false`, or default storage path could not
  be resolved at startup).
- `invalid_signature` (HTTP 400): the `{signature}` path segment
  fails canonical format validation.
- `limit_reached` (HTTP 507, `action=ack` only): `MAX_ACTIVE_ACKS`
  (10 000) reached, refusing to accept a new entry.
- `file_too_large` (HTTP 507, `action=ack` only): append would push
  the JSONL above 64 MiB. Per-daemon saturation, indicates compaction
  is needed at next restart or the cap should be raised. The `unack`
  path surfaces this under `internal_error` (HTTP 500) since the
  ack endpoints do not currently differentiate the cap on the
  unack write.
- `entry_too_large` (HTTP 507, `action=ack` only): a single record
  exceeds 4 KiB after serialization, typically because the
  caller-supplied `by` or `reason` field is oversized. Per-request
  misuse, indicates client-side validation should be tightened.
  Same `unack`-path caveat as `file_too_large`.
- `internal_error` (HTTP 500): IO failure, serialization error,
  symlink refused, insecure permissions, or no default storage
  location at write time.

**Pre-warming**. Both counters emit zero for documented reachable
combinations before the first request, so dashboards build with
`rate()` queries without `absent()` guards. The pre-warmed set is 2
success series (`action=ack` and `action=unack`) plus 13 failure
series (8 reasons on `action=ack`, 5 reasons on `action=unack`).
Impossible combinations (such as `action=ack,reason=not_acked` or
`action=unack,reason=already_acked`) are intentionally not
pre-warmed to avoid misleading series.

**Sample queries**.

- `rate(perf_sentinel_ack_operations_total[5m])`: ack and unack
  operations per second, useful for trend lines.
- `sum by (reason) (rate(perf_sentinel_ack_operations_failed_total{action="ack"}[5m]))`:
  ack failures by reason. Spikes on `unauthorized` indicate auth
  misconfiguration, spikes on `entry_too_large` indicate a
  misbehaving client (oversized `by` / `reason` payloads), spikes on
  `limit_reached` or `file_too_large` indicate store saturation.

## Scaphandre scrape counters (since 0.5.25)

Per-tick outcome of the daemon-side Scaphandre scraper (the task that
fetches `scaph_process_power_consumption_microwatts` from the
configured `[green.scaphandre] endpoint` every `scrape_interval_secs`).
Only registered when the daemon is built with the `daemon` feature.

| Metric                                          | Type    | Labels    | Description                                                                                  |
|-------------------------------------------------|---------|-----------|----------------------------------------------------------------------------------------------|
| `perf_sentinel_scaphandre_scrape_total`         | counter | `status`  | Total Scaphandre scrape attempts since daemon start, partitioned by outcome.                 |
| `perf_sentinel_scaphandre_scrape_failed_total`  | counter | `reason`  | Total failed Scaphandre scrapes since daemon start, partitioned by failure reason.           |
| `perf_sentinel_scaphandre_last_scrape_age_seconds` | gauge | (none)    | Seconds since the last successful scrape (resets to 0 on success). Hung-scraper canary.      |

`status` label values: `success`, `failed`. Pre-warmed at zero so
dashboards plot rate-zero before the first scrape.

`reason` label values:

- `unreachable`: low-level transport failure (connection refused, DNS
  failure, TLS handshake error, host down). The endpoint is not
  reachable from the daemon pod.
- `timeout`: the 3-second hard deadline on the per-scrape HTTP call
  elapsed before a response landed.
- `http_error`: the endpoint replied with a non-2xx status code.
- `body_read_error`: transport error while streaming the response body
  after a successful status read.
- `request_error`: hyper failed to build the HTTP request from the
  (post-validation) URI. Rare, indicates a configuration edge case
  the URI parser missed.
- `invalid_utf8`: the response body was not valid UTF-8. Scaphandre
  always emits ASCII-safe Prometheus text, so this almost always
  means the endpoint is not Scaphandre.

**Pre-warming**. Both counters emit zero for every documented label
value before the first scrape, so `rate()` queries do not need
`absent()` guards. The pre-warmed set is 2 status series plus 6
reason series. Configuration parsing failures (invalid endpoint URI)
abort the scraper task at startup before the counter is touched, they
are visible only in daemon logs at `error` level.

**Sample queries**.

- `rate(perf_sentinel_scaphandre_scrape_total{status="success"}[5m])`
  divided by `rate(perf_sentinel_scaphandre_scrape_total[5m])`:
  scrape success ratio over 5 minutes. Useful for an SLO panel or
  alert (`< 0.95` over 15 minutes flags a degraded scraper).
- `topk(1, increase(perf_sentinel_scaphandre_scrape_failed_total[1h]))`:
  dominant failure reason over the past hour. Persistent
  `unreachable` typically points at a missing Scaphandre exporter on
  the host, persistent `http_error` at an exporter behind a
  reverse proxy returning the wrong status, persistent `invalid_utf8`
  at an endpoint that is not Scaphandre at all.

## GreenOps metrics

| Metric                                               | Type    | Labels    | Description                                                                                                                        |
|------------------------------------------------------|---------|-----------|------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_io_waste_ratio`                       | gauge   | (none)    | Cumulative I/O waste ratio (avoidable / total) since daemon start. Use `rate()` on the underlying counters for windowed values.    |
| `perf_sentinel_total_io_ops`                         | counter | (none)    | Cumulative total I/O ops processed.                                                                                                |
| `perf_sentinel_avoidable_io_ops`                     | counter | (none)    | Cumulative avoidable I/O ops detected.                                                                                             |
| `perf_sentinel_service_io_ops_total`                 | counter | `service` | Per-service cumulative I/O ops (read by the Scaphandre scraper for per-service energy attribution).                                |
| `perf_sentinel_scaphandre_last_scrape_age_seconds`   | gauge   | (none)    | Seconds since the last successful Scaphandre scrape. Stays at 0 when Scaphandre is not configured. Useful for hung-scraper alerts. |
| `perf_sentinel_cloud_energy_last_scrape_age_seconds` | gauge   | (none)    | Same pattern for the cloud SPECpower scraper.                                                                                      |

## Warning kinds: transient vs sticky

`Report.warning_details` (since 0.5.19) has two stable kinds today,
each with a different lifecycle. The distinction matters for
monitoring strategies: a transient warning self-resolves, a sticky one
persists until the daemon restarts.

| Kind              | Lifecycle | Emitted when                                                                            | Cleared by                                              |
|-------------------|-----------|-----------------------------------------------------------------------------------------|---------------------------------------------------------|
| `cold_start`      | Transient | `events_processed_total == 0` or `traces_analyzed_total == 0` on the daemon            | First successful batch (both counters strictly positive) |
| `ingestion_drops` | Sticky    | `perf_sentinel_otlp_rejected_total{reason="channel_full"} > 0` since daemon start      | Daemon restart (counter reset)                          |

`cold_start` is a state warning: "the snapshot is not meaningful right
now". `ingestion_drops` is an audit warning: "at some point since
daemon start the channel saturated, here is the count for the
post-mortem". Acknowledging findings via the daemon ack API does not
clear either kind, they reflect daemon state rather than detection
output.

Lab tooling that asserts on `warning_details[].kind == "cold_start"`
should account for the transient nature: any background traffic, even
synthetic seed traces or health probes, can close the cold-start
window in well under 60 seconds.

## Cross-references

- `Report.warning_details` field (operator-facing snapshot warnings):
  see [RUNBOOK.md](RUNBOOK.md) section "Reading Report warnings".
- Acknowledgments workflow (cross-format finding suppression):
  see [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md).
- SARIF emitter for CI integration: see [SARIF.md](SARIF.md).
