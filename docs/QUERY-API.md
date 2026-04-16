# Daemon query API

The perf-sentinel daemon exposes an HTTP query API that lets external
systems pull findings, trace explanations, cross-trace correlations, and
daemon liveness. Use it to feed Prometheus alerts, Grafana dashboards,
on-call runbooks, or custom CI gate scripts without parsing NDJSON logs.

The API shipped in v0.4.0 (Phase 6). This page documents it as a
first-class product surface with a stability contract.

## Endpoint overview

| Method | Path                          | Purpose                                                                       |
|--------|-------------------------------|-------------------------------------------------------------------------------|
| GET    | `/api/status`                 | Daemon liveness, version, uptime, in-flight counts                            |
| GET    | `/api/findings`               | Recent findings from the ring buffer, with service, type, and severity filters |
| GET    | `/api/findings/{trace_id}`    | All findings for one trace                                                    |
| GET    | `/api/explain/{trace_id}`     | Span tree for a trace still in daemon memory, findings annotated inline       |
| GET    | `/api/correlations`           | Active cross-trace temporal correlations                                      |

All endpoints return `application/json`. No authentication. The daemon
listens on `127.0.0.1` by default (see `[daemon] listen_address` in
`docs/CONFIGURATION.md`), so the API is reachable only from the host
running the daemon unless you explicitly widen the bind address.

### Deployment notes

- The query API shares the same HTTP port as OTLP HTTP ingestion
  (`[daemon] listen_port_http`, default `4318`) and the `/metrics`
  Prometheus scrape endpoint. One port, three surfaces.
- The query API can be disabled at startup by setting
  `[daemon] api_enabled = false`. Useful when the daemon runs in a
  hostile multi-tenant host and you only want OTLP ingestion.
- The findings ring buffer size is bounded by
  `[daemon] max_retained_findings` (default `10000`). Older findings are
  evicted FIFO.

## Endpoints

### GET /api/status

Returns a compact liveness object. Use this as a readiness probe or as the
cheapest way to verify the daemon is up.

**Query parameters:** none.

**Response shape:**

| Field             | Type   | Description                                         |
|-------------------|--------|-----------------------------------------------------|
| `version`         | string | Daemon binary version (Cargo package version)       |
| `uptime_seconds`  | number | Seconds since the daemon process started            |
| `active_traces`   | number | Traces currently held in the correlation window    |
| `stored_findings` | number | Findings currently retained in the query ring buffer |

**Example:**

```bash
curl -sS http://127.0.0.1:4318/api/status
```

```json
{
  "version": "0.4.0",
  "uptime_seconds": 48,
  "active_traces": 0,
  "stored_findings": 5
}
```

### GET /api/findings

Returns a JSON array of recent findings, newest first. Each element wraps
the finding itself plus a daemon-side ingestion timestamp.

**Query parameters:**

| Name       | Type    | Default | Description                                                                                      |
|------------|---------|---------|--------------------------------------------------------------------------------------------------|
| `service`  | string  | none    | Exact match on the `finding.service` field                                                       |
| `type`     | string  | none    | Exact match on `finding.type` in snake_case (e.g. `n_plus_one_sql`, `redundant_sql`)             |
| `severity` | string  | none    | Exact match on `finding.severity` in snake_case (`critical`, `warning`, `info`)                   |
| `limit`    | integer | `100`   | Maximum number of entries to return, capped server-side at `1000` (higher values are silently clamped) |

Unknown parameters are ignored. Malformed values (e.g. `limit=abc`) return
HTTP 400 with an axum-generated error body.

**Response shape:** array of `StoredFinding`. Each `StoredFinding` has:

- `finding`: the detected finding. See
  [`Finding` schema](#finding-schema) below.
- `stored_at_ms`: integer Unix timestamp in milliseconds, recorded when
  the daemon inserted this finding into the ring buffer.

**Example:**

```bash
curl -sS "http://127.0.0.1:4318/api/findings?severity=warning&limit=2"
```

```json
[
  {
    "finding": {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "SELECT * FROM order_item WHERE order_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0,
        "io_intensity_band": "high"
      },
      "confidence": "daemon_staging"
    },
    "stored_at_ms": 1776350162450
  },
  {
    "finding": {
      "type": "n_plus_one_http",
      "severity": "warning",
      "trace_id": "trace-n1-http",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "GET /api/users/{id}",
        "occurrences": 6,
        "window_ms": 200,
        "distinct_params": 6
      },
      "suggestion": "Use batch endpoint with ?ids=... to batch 6 calls into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.200Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0,
        "io_intensity_band": "high"
      },
      "confidence": "daemon_staging"
    },
    "stored_at_ms": 1776350162450
  }
]
```

#### Finding schema

The `finding` object exposed by `/api/findings` and
`/api/findings/{trace_id}` is identical to the JSON emitted by
`perf-sentinel analyze --format json`. Stable fields as of v0.4.1:

| Field              | Type                | Description                                                                                      |
|--------------------|---------------------|--------------------------------------------------------------------------------------------------|
| `type`             | string (enum)       | `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`, `slow_sql`, `slow_http`, `excessive_fanout`, `chatty_service`, `pool_saturation`, `serialized_calls` |
| `severity`         | string (enum)       | `critical`, `warning`, `info`                                                                    |
| `trace_id`         | string              | Trace ID where the pattern was detected                                                          |
| `service`          | string              | Service that emitted the anti-pattern                                                            |
| `source_endpoint`  | string              | Normalized inbound endpoint hosting the pattern                                                  |
| `pattern`          | object              | `{ template, occurrences, window_ms, distinct_params }`                                          |
| `suggestion`       | string              | Human-readable remediation hint                                                                  |
| `first_timestamp`  | string (ISO 8601)   | Earliest span in the detected group                                                              |
| `last_timestamp`   | string (ISO 8601)   | Latest span in the detected group                                                                |
| `confidence`       | string (enum)       | `ci_batch`, `daemon_staging`, `daemon_production`                                                |
| `green_impact`     | object (optional)   | `{ estimated_extra_io_ops, io_intensity_score, io_intensity_band }` when green scoring is enabled |
| `code_location`    | object (optional)   | `{ code_function?, code_filepath?, code_lineno?, code_namespace? }` when OTel `code.*` attributes are present |

### GET /api/findings/{trace_id}

Returns all findings whose `trace_id` matches the path segment, as a JSON
array. Same element shape as `/api/findings`. Hard cap of 1000 entries
applies (pathological traces with hundreds of N+1 clusters).

**Path parameter:** `trace_id` (string, exact match). The path segment is
URL-decoded by axum before comparison.

**Response shape:** same `Vec<StoredFinding>` as `/api/findings`. An
**empty array `[]`** is returned when the trace ID is unknown (the
endpoint does not return 404).

**Example:**

```bash
curl -sS "http://127.0.0.1:4318/api/findings/trace-n1-sql"
```

```json
[
  {
    "finding": {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "order-svc",
      "source_endpoint": "POST /api/orders/42/submit",
      "pattern": {
        "template": "SELECT * FROM order_item WHERE order_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0,
        "io_intensity_band": "high"
      },
      "confidence": "daemon_staging"
    },
    "stored_at_ms": 1776350162450
  }
]
```

### GET /api/explain/{trace_id}

Returns the span tree for a trace **still held in the daemon correlation
window** (default TTL: 30 seconds after the last span of the trace
arrived). Useful for debugging a live trace right after it is emitted.

**Important:** findings are retained in the ring buffer long after the
trace itself evicts from the window. That means
`/api/findings/{trace_id}` keeps working for hours after the trace is
gone, but `/api/explain/{trace_id}` only works within the TTL window.

**Path parameter:** `trace_id` (string, exact match).

**Response shape (trace in memory):** object with a `roots` array. Each
node describes a span with:

| Field              | Type     | Description                                                                   |
|--------------------|----------|-------------------------------------------------------------------------------|
| `span_id`          | string   | Span identifier                                                               |
| `parent_span_id`   | string \| null | Parent span identifier, `null` for root spans                         |
| `service`          | string   | Service that emitted the span                                                 |
| `operation`        | string   | Operation name (e.g. `SELECT`, `GET`, `POST`)                                 |
| `template`         | string   | Normalized SQL query or HTTP route                                            |
| `timestamp`        | string   | ISO 8601 start timestamp                                                      |
| `duration_us`      | number   | Duration in microseconds                                                      |
| `findings`         | array    | Findings attached to this span, each `{ type, severity, suggestion, occurrences }` |
| `children`         | array    | Child span nodes, recursive                                                    |

**Response shape (trace unknown or evicted):** an object with a single
`error` field.

**Examples:**

```bash
# Trace still in memory
curl -sS "http://127.0.0.1:4318/api/explain/trace-n1-sql"
```

```json
{
  "roots": [
    {
      "children": [],
      "duration_us": 800,
      "findings": [
        {
          "occurrences": 6,
          "severity": "warning",
          "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
          "type": "n_plus_one_sql"
        }
      ],
      "operation": "SELECT",
      "parent_span_id": null,
      "service": "order-svc",
      "span_id": "span-1",
      "template": "SELECT * FROM order_item WHERE order_id = ?",
      "timestamp": "2025-07-10T14:32:01.000Z"
    }
  ]
}
```

```bash
# Trace not in memory (evicted or never seen)
curl -sS "http://127.0.0.1:4318/api/explain/trace-does-not-exist"
```

```json
{
  "error": "trace not found in daemon memory"
}
```

### GET /api/correlations

Returns active cross-trace temporal correlations, sorted by confidence
descending. Empty array when `[daemon.correlation] enabled = false`
(default). Capped at 1000 entries.

**Query parameters:** none.

**Response shape:** array of `CrossTraceCorrelation`. Each entry has:

| Field                      | Type    | Description                                                                      |
|----------------------------|---------|----------------------------------------------------------------------------------|
| `source`                   | object  | Leading endpoint: `{ finding_type, service, template }`                          |
| `target`                   | object  | Trailing endpoint observed after `source` within `lag_threshold_ms`              |
| `co_occurrence_count`      | number  | Number of co-occurrences within the rolling window                               |
| `source_total_occurrences` | number  | Total occurrences of `source` in the rolling window                              |
| `confidence`               | number  | Ratio `co_occurrence_count / source_total_occurrences`                           |
| `median_lag_ms`            | number  | Median lag between `source` and `target`                                         |
| `first_seen`               | string  | ISO 8601 timestamp of the first co-occurrence                                    |
| `last_seen`                | string  | ISO 8601 timestamp of the most recent co-occurrence                              |

**Example:**

```bash
curl -sS "http://127.0.0.1:4318/api/correlations"
```

```json
[
  {
    "source": {
      "finding_type": "redundant_sql",
      "service": "cache-svc",
      "template": "SELECT * FROM settings WHERE key = ?"
    },
    "target": {
      "finding_type": "n_plus_one_sql",
      "service": "order-svc",
      "template": "SELECT * FROM order_item WHERE order_id = ?"
    },
    "co_occurrence_count": 2,
    "source_total_occurrences": 1,
    "confidence": 2.0,
    "median_lag_ms": 0.0,
    "first_seen": "2026-04-16T14:36:02.450Z",
    "last_seen": "2026-04-16T14:36:02.450Z"
  }
]
```

## Error responses

| Condition                                       | Status | Body                                                |
|-------------------------------------------------|--------|-----------------------------------------------------|
| Unknown `trace_id` on `/api/findings/{trace_id}` | 200    | `[]`                                                |
| Unknown `trace_id` on `/api/explain/{trace_id}`  | 200    | `{"error": "trace not found in daemon memory"}`     |
| Correlations disabled or correlator idle         | 200    | `[]`                                                |
| Malformed query parameter (e.g. `limit=abc`)    | 400    | axum-generated plain-text error                     |
| Unknown path (e.g. `/api/does-not-exist`)       | 404    | empty body                                          |
| Method other than GET                            | 405    | axum-generated plain-text error                     |

The API does not emit 5xx on normal operation. A process crash returns
whatever the TCP stack emits (connection reset).

## Use cases

### Prometheus alerting on critical findings

Run a Prometheus Blackbox exporter that scrapes
`/api/findings?severity=critical&limit=1` and alerts when the response
array is non-empty. Example AlertManager rule using a `vector_count`
computed by a recording rule:

```yaml
groups:
  - name: perf-sentinel
    rules:
      - alert: PerfSentinelCriticalFinding
        expr: perf_sentinel_findings_total{severity="critical"} > 0
        for: 2m
        labels:
          severity: page
        annotations:
          summary: "perf-sentinel detected a critical performance anti-pattern"
          description: |
            Critical finding count is {{ $value }}.
            Query `/api/findings?severity=critical` on the daemon for details.
```

The built-in Prometheus scrape endpoint at `/metrics` already exposes
`perf_sentinel_findings_total{type,severity}` as a counter, so you do not
need the query API for counting alerts. Use the query API to fetch the
**payload** (template, trace ID, suggestion) that the alert handler
includes in the notification.

### Custom Grafana dashboard via the JSON datasource

Install the Grafana JSON API datasource plugin, point it at the daemon,
and build per-service tables. Example panel query returning the 20 most
recent findings for `order-svc`:

```
URL:     http://perf-sentinel.internal:4318/api/findings
Method:  GET
Params:  service=order-svc
         limit=20
Fields:  $.finding.type,
         $.finding.severity,
         $.finding.pattern.template,
         $.finding.pattern.occurrences,
         $.finding.source_endpoint,
         $.stored_at_ms
```

Pair this with the Prometheus `/metrics` endpoint already exposed by the
daemon for time-series trends, and use the query API for the **list of
concrete findings** the user can click into.

### SRE runbook: page on a stuck scraper

If your daemon has any opt-in scraper configured (`[green.scaphandre]`,
`[green.cloud]`, `[green.electricity_maps]`, `[pg_stat]`), a staleness in
`active_traces` or `stored_findings` growth is a strong signal that
ingestion has stalled. A bash snippet to embed in an on-call runbook:

```bash
#!/usr/bin/env bash
set -euo pipefail

DAEMON="${DAEMON:-http://127.0.0.1:4318}"
response=$(curl -sSf --max-time 3 "${DAEMON}/api/status")
uptime=$(echo "$response" | jq -r '.uptime_seconds')
traces=$(echo "$response" | jq -r '.active_traces')
findings=$(echo "$response" | jq -r '.stored_findings')

if [ "$uptime" -gt 300 ] && [ "$traces" -eq 0 ] && [ "$findings" -eq 0 ]; then
  echo "perf-sentinel daemon has been idle for ${uptime}s with no traces or findings"
  echo "Check ingestion path: OTLP endpoint, collector config, Java agent env vars"
  exit 1
fi
```

Wire this to PagerDuty or OpsGenie via the on-call escalation tool of
your choice.

## Stability contract

The query API carries a stability promise starting at v0.4.1.

**What is stable:**

- All paths listed in [Endpoint overview](#endpoint-overview).
- All fields listed in the endpoint sections above. Field names and
  shapes will not be renamed, removed, or retyped in a minor release.
- Enum values (`finding.type`, `finding.severity`, `finding.confidence`,
  `io_intensity_band`, and so on): existing variants remain. New
  variants may be added in minor releases. Clients must tolerate
  unknown enum values and not crash on them.
- The behavior of the five error responses in
  [Error responses](#error-responses).

**What may change in a minor release:**

- New optional fields may be added to any JSON object.
- New enum variants may be added.
- New endpoints under `/api/...` may be introduced.
- Default values (e.g. `limit=100`) may be tuned if profiling shows a
  better default, but the hard cap (`1000`) will not shrink.

**What requires a major release:**

- Removing or renaming any field.
- Retyping a field (e.g. turning a number into a string).
- Shrinking the hard cap on `/api/findings?limit=`.
- Changing the authentication surface (the current contract is
  unauthenticated loopback-only by default).

**Client guidance:**

- Always tolerate unknown fields in JSON objects.
- Never parse enum variants exhaustively without a fallback branch.
- Pin the daemon version in your CI/CD manifests and review the
  `CHANGELOG.md` before bumping.

## See also

- [`docs/INTEGRATION.md`](./INTEGRATION.md) for the overall deployment
  topology.
- [`docs/CONFIGURATION.md`](./CONFIGURATION.md) for `[daemon]` and
  `[daemon.correlation]` settings.
- [`docs/design/06-INGESTION-AND-DAEMON.md`](./design/06-INGESTION-AND-DAEMON.md)
  for the daemon's internal design.
