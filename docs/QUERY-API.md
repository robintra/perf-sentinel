# Daemon query API

The perf-sentinel daemon exposes an HTTP query API that lets external
systems pull findings, trace explanations, cross-trace correlations, and
daemon liveness. Use it to feed Prometheus alerts, Grafana dashboards,
on-call runbooks or custom CI gate scripts without parsing NDJSON logs.

The API shipped in v0.4.0. This page documents it as a
first-class product surface with a stability contract.

## Contents

- [Endpoint overview](#endpoint-overview): one-line description per endpoint.
- [Restricting writes in production](#restricting-writes-in-production-reverse-proxy): reserve acks and report export to a group with a reverse proxy.
- [Endpoints](#endpoints): full per-endpoint reference with request, response and worked examples.
- [Error responses](#error-responses): status codes and body shapes.
- [Use cases](#use-cases): Prometheus alerting, custom Grafana panels, SRE runbooks.
- [Stability contract](#stability-contract): the v0.4.1+ stability guarantees.
- [See also](#see-also): cross-references to related docs.

## Endpoint overview

| Method | Path                            | Purpose                                                                           |
|--------|---------------------------------|-----------------------------------------------------------------------------------|
| GET    | `/api/status`                   | Daemon liveness, version, uptime, in-flight counts                                |
| GET    | `/api/energy`                   | Live health of the energy/intensity backends (since 0.8.8)                        |
| GET    | `/api/findings`                 | Recent findings from the ring buffer, with service, type and severity filters     |
| GET    | `/api/findings/{trace_id}`      | All findings for one trace                                                        |
| GET    | `/api/explain/{trace_id}`       | Span tree for a trace still in daemon memory, findings annotated inline           |
| GET    | `/api/correlations`             | Active cross-trace temporal correlations                                          |
| GET    | `/api/export/report`            | Snapshot the live state as a Report JSON, pipe-compatible with `report --input -` |
| POST   | `/api/findings/{signature}/ack` | Acknowledge a finding at runtime (since 0.5.20)                                   |
| DELETE | `/api/findings/{signature}/ack` | Revoke a runtime ack                                                              |
| GET    | `/api/acks`                     | List active runtime acks                                                          |

All endpoints return `application/json`. No built-in authentication. The
daemon listens on `127.0.0.1` by default (see `[daemon] listen_address`
in `docs/CONFIGURATION.md`), so the API is reachable only from the host
running the daemon unless you explicitly widen the bind address. To let
developers read findings while reserving writes (acks) and the official
report export to architects or DevOps, see
[Restricting writes in production](#restricting-writes-in-production-reverse-proxy).

### Deployment notes

- The query API shares the same HTTP port as OTLP HTTP ingestion
  (`[daemon] listen_port_http`, default `4318`), the `/metrics`
  Prometheus scrape endpoint and the `GET /health` liveness probe.
  One port, four surfaces.
- The query API can be disabled at startup by setting
  `[daemon] api_enabled = false`. Useful when the daemon runs in a
  hostile multi-tenant host and you only want OTLP ingestion. In that
  mode, `/metrics` and `/health` stay exposed, they are infrastructure
  surfaces, not part of the query API.
- For Kubernetes or load-balancer probes, prefer `GET /health` over
  `GET /api/status`: `/health` is always on, holds no locks and stays
  responsive under any ingestion load.
- The findings **ring buffer** (a fixed-size circular store that evicts oldest entries when full) is bounded by
  `[daemon] max_retained_findings` (default `10000`). Older findings are
  evicted FIFO.

## Restricting writes in production (reverse proxy)

A common production requirement is to let any developer **read** findings
while reserving the **write** paths (acknowledge and revoke) and the
**official report export** to architects or DevOps. This stops a finding
from being acked without sign-off from the people accountable for the
production posture.

The daemon does not carry an identity provider or a role model. The
optional `[daemon.ack] api_key` (see
[POST /api/findings/{signature}/ack](#post-apifindingssignatureack)) is a
single shared secret: it gates writes coarsely, but it cannot tell one
user from another and cannot express "this group may, that group may
not". For per-identity authorization, put a reverse proxy in front of the
daemon. The proxy authenticates every caller against your SSO, then
authorizes by HTTP method and path. The daemon stays a pure analysis
engine, which matches its design (no implicit network surface, no
embedded IAM).

The rule the proxy enforces:

| Path                                                                                 | GET                    | POST / DELETE         |
|--------------------------------------------------------------------------------------|------------------------|-----------------------|
| `/api/findings`, `/api/explain/...`, `/api/correlations`, `/api/status`, `/api/energy`, `/api/acks` | any authenticated user | not applicable        |
| `/api/findings/{signature}/ack`                                                      | not applicable         | privileged group only |
| `/api/export/report`                                                                 | privileged group only  | not applicable        |

`/api/export/report` sits in the privileged column because it
materializes the full report snapshot that feeds the official
HTML dashboard. Producing an official report is itself a privileged
action, see [`docs/REPORTING.md`](./REPORTING.md#restricting-who-can-publish-an-official-disclosure)
for the CI-side counterpart (who may run `disclose --intent official`).

### oauth2-proxy + nginx

[oauth2-proxy](https://oauth2-proxy.github.io/oauth2-proxy/) handles the
OIDC authentication and surfaces the authenticated identity as response
headers. Its `/oauth2/auth` endpoint also enforces group membership
per request through the `allowed_groups` query parameter, so the
authorization decision is made by oauth2-proxy, not by fragile nginx
`if` logic. nginx routes privileged paths to a group-checked auth
subrequest and everything else to a plain one.

`oauth2-proxy.cfg` (auth-only mode, nginx does the proxying):

```ini
provider          = "oidc"
oidc_issuer_url   = "https://sso.example.com/realms/prod"
client_id         = "perf-sentinel"
client_secret     = "${OAUTH2_PROXY_CLIENT_SECRET}"   # from your secret manager, never committed
cookie_secret     = "${OAUTH2_PROXY_COOKIE_SECRET}"   # 32-byte base64
email_domains     = ["example.com"]
upstreams         = ["static://202"]   # auth-only: return 202 on success, nginx proxies the daemon
reverse_proxy     = true
set_xauthrequest  = true               # emit X-Auth-Request-User / -Email / -Groups
oidc_groups_claim = "groups"           # so the group claim reaches nginx
scope             = "openid email groups"
```

`nginx.conf` (relevant server block):

```nginx
upstream perf_sentinel { server 127.0.0.1:4318; }   # daemon, loopback-only
upstream oauth2_proxy  { server 127.0.0.1:4180; }

server {
    listen 443 ssl;
    server_name perf-sentinel.internal;
    # ssl_certificate / ssl_certificate_key ...

    # oauth2-proxy sign-in and callback routes.
    location /oauth2/ {
        proxy_pass        http://oauth2_proxy;
        proxy_set_header  Host                     $host;
        proxy_set_header  X-Real-IP                $remote_addr;
        proxy_set_header  X-Forwarded-Proto        $scheme;
        proxy_set_header  X-Auth-Request-Redirect  $request_uri;
    }

    # Plain authentication: any valid SSO session.
    location = /oauth2/auth {
        internal;
        proxy_pass               http://oauth2_proxy;
        proxy_pass_request_body  off;
        proxy_set_header         Content-Length "";
        proxy_set_header         X-Original-URI $request_uri;
    }

    # Group-checked authentication: oauth2-proxy returns 403 when the
    # caller is not in the group, which auth_request propagates as 403.
    location = /oauth2/auth-admin {
        internal;
        proxy_pass               http://oauth2_proxy/oauth2/auth?allowed_groups=perf-sentinel-admins;
        proxy_pass_request_body  off;
        proxy_set_header         Content-Length "";
        proxy_set_header         X-Original-URI $request_uri;
    }

    # Privileged routes: ack create/revoke and the official report export.
    # A regex location wins over the /api/ prefix, so these never fall
    # through to the open rule below.
    location ~ ^/api/(findings/[^/]+/ack|export/report)$ {
        auth_request /oauth2/auth-admin;
        error_page 401 = /oauth2/sign_in;
        auth_request_set $auth_user $upstream_http_x_auth_request_user;
        proxy_set_header X-User-Id $auth_user;   # overwrites any client-supplied value
        proxy_pass       http://perf_sentinel;
        proxy_set_header Host $host;
    }

    # Everything else under /api/: read access for any authenticated user.
    location /api/ {
        auth_request /oauth2/auth;
        error_page 401 = /oauth2/sign_in;
        auth_request_set $auth_user $upstream_http_x_auth_request_user;
        proxy_set_header X-User-Id $auth_user;
        proxy_pass       http://perf_sentinel;
        proxy_set_header Host $host;
    }
}
```

### Why this is safe

- **Bind the daemon to loopback** (`[daemon] listen_address = "127.0.0.1"`)
  or an internal interface the proxy alone can reach. The proxy is the
  only front door.
- **Keep `[daemon.ack] api_key` set** as a second factor. If someone
  reaches the daemon port directly, bypassing the proxy, they still
  cannot write without the key.
- **The daemon trusts `X-User-Id`** for the audit `by` field. The nginx
  block sets it from the authenticated subrequest (`$auth_user`) and so
  overwrites any value a client supplies, which closes the spoofing gap.
  The authenticated identity then lands in the JSONL ack store, giving
  you an audit trail of who acked what.
- `perf-sentinel-admins` is illustrative. Use whatever group your IdP
  exposes in the `groups` claim.

## Endpoints

### GET /api/status

Returns a compact liveness object. Use this as a readiness probe or as the
cheapest way to verify the daemon is up.

**Query parameters:** none.

**Response shape:**

| Field             | Type   | Description                                          |
|-------------------|--------|------------------------------------------------------|
| `version`         | string | Daemon binary version (Cargo package version)        |
| `uptime_seconds`  | number | Seconds since the daemon process started             |
| `active_traces`   | number | Traces currently held in the correlation window      |
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

### GET /api/energy

Live health of the five energy/intensity backends (since 0.8.8): the
four scraped measured-energy sources (Scaphandre, Kepler, Redfish,
cloud SPECpower) and the Electricity Maps real-time intensity API.
Backs the Scrapers tab of `perf-sentinel query monitor`. The effective
mix itself (which source won the precedence chain per service, grid
intensity per region) lives on `/api/export/report` under
`green_summary`; this endpoint only answers "is each backend
configured, fresh, and succeeding".

**Query parameters:** none.

**Response shape:** an object with a `backends` array of five entries in
a fixed order (`scaphandre`, `kepler`, `redfish`, `cloud_energy`,
`electricity_maps`), each:

| Field                     | Type    | Description                                                                                                      |
|---------------------------|---------|------------------------------------------------------------------------------------------------------------------|
| `backend`                 | string  | Stable backend name                                                                                              |
| `configured`              | boolean | Whether the backend is configured, from the `[green]` config frozen at daemon startup                            |
| `last_scrape_age_seconds` | number  | Seconds since the last successful scrape, as of the backend's most recent scrape tick (same semantics as the `/metrics` gauge). Omitted when not configured or when the backend has no freshness gauge |
| `scrapes_ok`              | number  | Successful scrapes since daemon start. Omitted when not configured or not scraped (`cloud_energy`, `electricity_maps`) |
| `scrapes_failed`          | number  | Failed scrapes since daemon start. Same omission rules as `scrapes_ok`                                           |

The optional fields are omitted rather than zeroed for unconfigured
backends: the underlying Prometheus gauges are pre-registered at 0, and
a literal `0` would read as a fresh scrape. `electricity_maps` carries
no freshness gauge by design; its liveness shows as
`intensity_source = "real_time"` entries on the report's region
breakdown.

Two age-reading caveats. A configured backend still reads
`last_scrape_age_seconds = 0.0` during its first scrape interval after
daemon start, before anything has actually been scraped: read it
together with `scrapes_ok = 0` to tell "not scraped yet" from "fresh".
And for `cloud_energy` the age tracks the reachability of the
configured Prometheus endpoint, not per-service coverage: a tick counts
as successful as soon as one service yields a reading.

**Example:**

```bash
curl -sS http://127.0.0.1:4318/api/energy
```

```json
{
  "backends": [
    {
      "backend": "scaphandre",
      "configured": true,
      "last_scrape_age_seconds": 3.0,
      "scrapes_ok": 120,
      "scrapes_failed": 2
    },
    { "backend": "kepler", "configured": false },
    { "backend": "redfish", "configured": false },
    { "backend": "cloud_energy", "configured": false },
    { "backend": "electricity_maps", "configured": true }
  ]
}
```

### GET /api/findings

Returns a JSON array of recent findings, newest first. Each element wraps
the finding itself plus a daemon-side ingestion timestamp.

**Query parameters:**

| Name       | Type    | Default | Description                                                                                            |
|------------|---------|---------|--------------------------------------------------------------------------------------------------------|
| `service`  | string  | none    | Exact match on the `finding.service` field                                                             |
| `type`     | string  | none    | Exact match on `finding.type` in snake_case (e.g. `n_plus_one_sql`, `redundant_sql`)                   |
| `severity` | string  | none    | Exact match on `finding.severity` in snake_case (`critical`, `warning`, `info`)                        |
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

| Field             | Type              | Description                                                                                                                                                                  |
|-------------------|-------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `type`            | string (enum)     | `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`, `slow_sql`, `slow_http`, `excessive_fanout`, `chatty_service`, `pool_saturation`, `serialized_calls` |
| `severity`        | string (enum)     | `critical`, `warning`, `info`                                                                                                                                                |
| `trace_id`        | string            | Trace ID where the pattern was detected                                                                                                                                      |
| `service`         | string            | Service that emitted the anti-pattern                                                                                                                                        |
| `source_endpoint` | string            | Normalized inbound endpoint hosting the pattern                                                                                                                              |
| `pattern`         | object            | `{ template, occurrences, window_ms, distinct_params }`                                                                                                                      |
| `suggestion`      | string            | Human-readable remediation hint                                                                                                                                              |
| `first_timestamp` | string (ISO 8601) | Earliest span in the detected group                                                                                                                                          |
| `last_timestamp`  | string (ISO 8601) | Latest span in the detected group                                                                                                                                            |
| `confidence`      | string (enum)     | `ci_batch`, `daemon_staging`, `daemon_production`                                                                                                                            |
| `green_impact`    | object (optional) | `{ estimated_extra_io_ops, io_intensity_score, io_intensity_band }` when green scoring is enabled                                                                            |
| `code_location`   | object (optional) | `{ function?, filepath?, lineno?, namespace? }` when OTel `code.*` attributes are present                                                                                    |
| `suggested_fix`   | object (optional) | `{ pattern, framework, recommendation, reference_url? }` when the framework can be inferred (Java/JPA in v1)                                                                 |

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

| Field            | Type           | Description                                                                        |
|------------------|----------------|------------------------------------------------------------------------------------|
| `span_id`        | string         | Span identifier                                                                    |
| `parent_span_id` | string \| null | Parent span identifier, `null` for root spans                                      |
| `service`        | string         | Service that emitted the span                                                      |
| `operation`      | string         | Operation name (e.g. `SELECT`, `GET`, `POST`)                                      |
| `template`       | string         | Normalized SQL query or HTTP route                                                 |
| `timestamp`      | string         | ISO 8601 start timestamp                                                           |
| `duration_us`    | number         | Duration in microseconds                                                           |
| `findings`       | array          | Findings attached to this span, each `{ type, severity, suggestion, occurrences }` |
| `children`       | array          | Child span nodes, recursive                                                        |

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

### GET /api/export/report

Snapshot the daemon's current in-memory state as a `Report` JSON, identical in shape to `perf-sentinel analyze --format json`. This closes the loop between the live daemon and the post-mortem `perf-sentinel report` HTML dashboard: the HTML report can ingest a daemon snapshot over HTTP via standard shell composition.

The `analysis` section reflects daemon-lifetime counters (cumulative since daemon start). The `green_summary` field is refreshed by the event loop after each batch (regions, top offenders, avoidable I/O ratio, CO2 numbers, scoring config), so the snapshot carries a live CO2 picture. The chip banner and the GreenOps tab in the HTML dashboard surface naturally on Electricity-Maps-configured daemons. The quality gate is not recomputed on the snapshot path. See `docs/design/05-GREENOPS-AND-CARBON.md` for the full audit-trail story.

**Cold-start behavior.** When the daemon has not yet processed any event, the endpoint returns `200 OK` with an empty Report envelope: `findings: []`, `green_summary: GreenSummary::disabled(0)`, and `warnings: ["daemon has not yet processed any events"]`. Pre-0.5.16 this path returned `503 Service Unavailable`, which tripped Kubernetes probes and confused CI scripts that treated 5xx as a daemon health issue. The empty envelope lets clients distinguish "cold start" from "events seen, zero findings" (the latter returns `200` with no warning string and `analysis.events_processed > 0`) without a status code mismatch. The double-counter guard (`events_processed_total > 0` AND `traces_analyzed_total > 0`) is preserved internally so the snapshot stays self-consistent during the `trace_ttl_ms / 2` window between the first event ingest and the first eviction tick.

**Prometheus metric.** Each request bumps `perf_sentinel_export_report_requests_total` so operators can dashboard or alert on Report snapshot frequency.

Example:

```bash
# Materialize a live daemon snapshot as an HTML dashboard
curl -s http://daemon.internal:4318/api/export/report \
    | perf-sentinel report --input - --output report.html
```

The `report` subcommand auto-detects the JSON shape: a top-level array is treated as trace events (pipelined through normalize + detect + score), a top-level object is treated as a pre-computed Report (taken as-is). The Correlations tab in the HTML dashboard lights up automatically when the daemon-produced Report carries non-empty `correlations`.

### POST /api/findings/{signature}/ack

Acknowledge a finding at runtime. The signature is the canonical
`<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix>` produced
by the same hashing logic as the CI TOML workflow (see
`docs/ACKNOWLEDGMENTS.md`). Available since 0.5.20.

The daemon maintains a JSONL append-only store at
`~/.local/share/perf-sentinel/acks.jsonl` by default (configurable via
`[daemon.ack] storage_path`). The store is replayed and compacted at
every daemon restart, so an ack/unack churn loop cannot accumulate
forever.

**Headers:**

- `Content-Type: application/json` (required, even with an empty body).
- `X-User-Id: <identifier>` (optional, populates the audit `by` field
  with priority over the JSON body, falling back to `"anonymous"`).
- `X-API-Key: <secret>` (required only when `[daemon.ack] api_key` is
  set in the daemon config, constant-time compared).

**Body (all fields optional):**

```json
{
  "by": "alice@example.com",
  "reason": "deferred to next quarter, see TICKET-1234",
  "expires_at": "2026-08-01T00:00:00Z"
}
```

**Responses:**

| Status | Condition                                                        |
|--------|------------------------------------------------------------------|
| 201    | Ack created                                                      |
| 400    | Signature does not match the canonical format                    |
| 401    | `[daemon.ack] api_key` is set, header is missing or wrong        |
| 409    | The signature is already acked (use `DELETE` first to revoke)    |
| 503    | `[daemon.ack] enabled = false`, the runtime ack store is offline |

**Example:**

```bash
SIG="n_plus_one_sql:order-svc:_api_v1_orders:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
curl -fsS -X POST "http://127.0.0.1:4318/api/findings/${SIG}/ack" \
  -H "Content-Type: application/json" \
  -H "X-User-Id: alice@example.com" \
  -d '{"reason":"deferred to next quarter","expires_at":"2026-08-01T00:00:00Z"}'
# 201 Created
```

After a successful ack, `GET /api/findings` filters the entry out by
default. Pass `?include_acked=true` to see it back with an
`acknowledged_by` annotation.

### DELETE /api/findings/{signature}/ack

Revoke a previously created daemon ack. Same auth headers as `POST`.
The matching finding reappears on `GET /api/findings` immediately.

**Responses:**

| Status | Condition                                              |
|--------|--------------------------------------------------------|
| 204    | Ack revoked                                            |
| 400    | Signature does not match the canonical format          |
| 401    | API key required and missing or wrong                  |
| 404    | The signature is not currently acked at the daemon     |
| 503    | Runtime ack store offline                              |

Note: this endpoint only revokes daemon-side acks. CI TOML acks are
read-only at runtime and require a PR against the
`.perf-sentinel-acknowledgments.toml` file to remove.

### GET /api/acks

Returns the array of active runtime acks (post-replay, post-expiry
filter). Read-only, no auth required (reads on a loopback API are
considered safe even when the daemon enforces an API key on writes).

**Response:** array of objects, one per active ack:

```json
[
  {
    "action": "ack",
    "signature": "n_plus_one_sql:order-svc:_api_v1_orders:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "by": "alice@example.com",
    "reason": "deferred to next quarter",
    "at": "2026-05-04T13:30:00Z",
    "expires_at": "2026-08-01T00:00:00Z"
  }
]
```

This endpoint surfaces only the daemon-side JSONL acks. CI TOML acks
loaded at startup are not included, query the TOML file directly for
that view, or call `GET /api/findings?include_acked=true` and inspect
the `acknowledged_by.source` field to see both sources unified.

### TOML and JSONL interop

The daemon reads `.perf-sentinel-acknowledgments.toml` (path
configurable via `[daemon.ack] toml_path`) at startup and unions its
entries with the JSONL store at query time. **TOML wins on conflict**:
when a signature is acked in both, the response carries the TOML
metadata (`source: "toml"`). This keeps the CI baseline immutable from
the daemon side, an SRE cannot accidentally override what the team
agreed to in PR review.

| Source | Persistence            | Audit              | Mutable at runtime |
|--------|------------------------|--------------------|--------------------|
| TOML   | Repo file              | `git log`          | No (PR-only)       |
| Daemon | `acks.jsonl` on disk   | JSONL append + compaction | Yes (POST/DELETE) |

### Behavior change in 0.5.20: `/api/findings` default filter

`GET /api/findings` (and the `?service=` / `?type=` / `?severity=`
filters) now omits acked findings by default. Pass
`?include_acked=true` to restore the pre-0.5.20 behavior. The opt-in
default mirrors the CLI 0.5.17 `--acknowledgments` semantics: an
operator looking at "what is currently broken" should not be drowned
in entries the team has already triaged.

The `/api/findings/{trace_id}` and `/api/export/report` endpoints
intentionally keep their previous shape, the per-trace and full-report
views are diagnostic and may need to surface acked findings even in
the default path.

## Error responses

| Condition                                        | Status | Body                                                   |
|--------------------------------------------------|--------|--------------------------------------------------------|
| Unknown `trace_id` on `/api/findings/{trace_id}` | 200    | `[]`                                                   |
| Unknown `trace_id` on `/api/explain/{trace_id}`  | 200    | `{"error": "trace not found in daemon memory"}`        |
| Correlations disabled or correlator idle         | 200    | `[]`                                                   |
| `/api/export/report` on cold-start daemon        | 200    | empty Report envelope with `warnings: ["daemon has not yet processed any events"]` (pre-0.5.16: 503) |
| Malformed query parameter (e.g. `limit=abc`)     | 400    | axum-generated plain-text error                        |
| Unknown path (e.g. `/api/does-not-exist`)        | 404    | empty body                                      |
| Method other than GET                            | 405    | axum-generated plain-text error                 |

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
daemon for time-series trends and use the query API for the **list of
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
  shapes will not be renamed, removed or retyped in a minor release.
- Enum values (`finding.type`, `finding.severity`, `finding.confidence`,
  `io_intensity_band` and so on): existing variants remain. New
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
