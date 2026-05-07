# Acknowledgment workflow

perf-sentinel supports two complementary acknowledgment mechanisms:
TOML in-repo (CI ack, since 0.5.17) and JSONL via the daemon HTTP API
(daemon ack, since 0.5.20). They cover different operational
scenarios and can be used side-by-side. This page explains how each
works, when to pick which, and how the CLI helper introduced in
0.5.22 plugs into the daemon side.

## CI ack: TOML in repo

The `.perf-sentinel-acknowledgments.toml` file at the root of an
application repository, versioned in git, modified through PR review.
Use this for permanent decisions made by the team: false positives,
known accepted-risk findings, intentional design choices.

### Adding a TOML ack

Edit the file directly:

```toml
[[acknowledged]]
signature = "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef"
acknowledged_by = "team-architecture"
acknowledged_at = "2026-05-04T13:30:00Z"
reason = "Intentional fan-out for batch reporting endpoint"
```

Commit, open a pull request, get review, merge. The next CI run will
honor the ack via `analyze --acknowledgments` and the
[CI templates](./ci-templates) bundled with the project.

### Removing a TOML ack

Delete the entry, commit, PR, review, merge. Same lifecycle as
adding one.

## Daemon ack: JSONL via API

For temporary, runtime acks made by SREs or oncall: defer a finding
while a fix ships, suppress noise during a known incident, etc. The
daemon persists these in a JSONL file as append-only events, with
optional expiration timestamps.

### Adding a daemon ack via curl (low-level)

```bash
curl -X POST http://daemon:4318/api/findings/<sig>/ack \
  -H "Content-Type: application/json" \
  -d '{"by":"alice","reason":"deferred","expires_at":"2026-05-11T00:00:00Z"}'
```

When auth is enabled server-side (`[daemon.ack] api_key`), add
`-H "X-API-Key: <KEY>"`.

### Adding a daemon ack via CLI (since 0.5.22, recommended)

```bash
perf-sentinel ack create \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef" \
  --reason "deferred to next sprint" \
  --expires 7d
```

The CLI handles auth resolution, duration parsing (relative or
ISO8601), daemon URL resolution and produces readable error
messages. See [`CLI.md`](./CLI.md#ack) for the full reference,
including the 1 KiB caps applied to stdin signatures and the
interactive API-key prompt.

### Revoking a daemon ack

```bash
perf-sentinel ack revoke \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef"
```

Or via curl:

```bash
curl -X DELETE http://daemon:4318/api/findings/<sig>/ack
```

## Listing active acks

```bash
perf-sentinel ack list                  # daemon acks, table format
perf-sentinel ack list --output json    # daemon acks, JSON
```

`perf-sentinel ack list` only enumerates daemon-side acks. TOML CI
acks live in the file itself, view them with:

```bash
cat .perf-sentinel-acknowledgments.toml
```

## Interop: TOML wins on conflict

Both sources are unioned at finding-filtering time. If the same
signature is acked in both TOML and daemon JSONL, the TOML version
wins. The rationale: the TOML baseline is shipped via PR review and
represents an immutable team-level decision; the daemon JSONL is a
mutable, runtime-only override.

A `POST /api/findings/{sig}/ack` for a signature already covered by
TOML returns HTTP 409 to avoid silent shadowing. The `ack create` CLI
maps this to exit 2 with a hint pointing at `ack revoke`.

### Adding a daemon ack from the HTML report (since 0.5.23, browser)

The HTML report can run in live mode and drive the same daemon
endpoints from the browser. Generate the report with `--daemon-url`,
open it, click the per-finding `Ack` button. See
[`HTML-REPORT.md`](./HTML-REPORT.md) for the setup, the CORS
prerequisites, and the X-API-Key handling.

```bash
perf-sentinel report --input traces.json --output report.html \
  --daemon-url http://localhost:4318
open report.html
```

### Adding a daemon ack from the TUI (since 0.5.24, terminal)

`perf-sentinel query inspect` opens an interactive TUI that exposes
the daemon findings list, span trees, and cross-trace correlations.
With 0.5.24, pressing `a` on the selected finding opens an
acknowledgment modal (reason / expires / by) that posts to the same
daemon endpoint, and `u` opens a revoke confirmation. The Findings
panel renders an `[acked by <user>]` italic gray indicator next to
already-acknowledged findings. See [`INSPECT.md`](./INSPECT.md) for
the keybinding map and the auth flow.

```bash
perf-sentinel query --daemon http://localhost:4318 inspect
# Press 'a' on a finding → modal → fill reason → Tab to Submit → Enter
```

`a` and `u` are no-op in batch mode (`inspect --input`) since
acknowledgment requires a running daemon to persist.

## Choosing between TOML and daemon

| Scenario                                  | Use                                  |
| ----------------------------------------- | ------------------------------------ |
| Permanent decision by the team            | TOML (versioned, auditable in git)   |
| Temporary defer during an incident        | Daemon (CLI or curl)                 |
| False positive shared by all environments | TOML                                 |
| Environment-specific suppression          | Daemon (one per environment)         |
| Onboarding cleanup of pre-existing        | TOML (bulk via editor)               |
| Single ack at 3am from PagerDuty          | Daemon CLI                           |
| Click Ack from MR review on the CI report | Daemon (HTML live mode, since 0.5.23)|
| Audit findings in a terminal session      | Daemon (TUI, since 0.5.24)           |

## Observability

The daemon exposes Prometheus counters on `/metrics` for every ack
operation it processes (`perf_sentinel_ack_operations_total{action}`
and `perf_sentinel_ack_operations_failed_total{action,reason}`). See
[`METRICS.md`](./METRICS.md) for the full schema and example
PromQL queries.

## Signature stability and service restarts

Acknowledgments match findings by a canonical signature:

```
<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>
```

The signature deliberately excludes `trace_id` and `span_id`, so a
single ack survives service restarts and routine traffic with varying
request identifiers. The contract is locked by unit tests in
`crates/sentinel-core/src/acknowledgments.rs`.

### Critical dependency on `http.route`

The `endpoint` component is derived from the OpenTelemetry `http.route`
attribute on the parent HTTP span, which carries the route template
(e.g. `/api/orders/{id}`) rather than the instantiated URL
(`/api/orders/42`).

When traced services emit `http.route`:

- Same finding on the same logical endpoint produces the same signature.
- Acknowledgments survive service restarts.
- Acknowledgments survive normal traffic with rotating request IDs.

When `http.route` is missing, perf-sentinel falls back to `http.url`,
then to `url.full` (OTel v1.21+ stable convention). Each unique URL
yields a different signature, ack churn becomes proportional to URL
cardinality, and deferred findings reappear at every new request id.
The fallback exists so the operator still sees a usable endpoint
string, not as a recommended posture.

Standard OpenTelemetry agents emit `http.route` automatically:

- Spring Boot 3+ with the OpenTelemetry Java agent.
- ASP.NET Core with the OpenTelemetry .NET SDK.
- Express.js, Fastify, Koa with `@opentelemetry/instrumentation-*`.
- Most modern HTTP framework auto-instrumentations.

To confirm an instrumented service emits route templates, inspect a
recent finding's `source_endpoint` against a running daemon:

```bash
curl -s http://localhost:4318/api/findings | jq -r '.[].source_endpoint' | sort -u
```

Templates with placeholders (`/api/orders/{id}`) indicate healthy
instrumentation. Instantiated URLs with hardcoded ids
(`/api/orders/42`) indicate `http.route` is missing and acks will
churn.

### Carbon scoring scope

The `green_impact` field on each finding is computed per detection
inside a single trace. The values reported by `perf-sentinel analyze`
or in the JSON report describe one occurrence and do not aggregate
across traces.

The daemon exposes Prometheus counters
(`perf_sentinel_findings_total`,
`perf_sentinel_avoidable_io_ops_total`) that accumulate
monotonically over the daemon's lifetime. Each batch contributes its
own per-batch dedup, keyed on `(trace_id, template, source_endpoint)`,
which prevents counting the same pattern twice within one batch.
Distinct traces, including those produced after a service restart,
contribute separately because they represent distinct request
executions. The counters reset only when the daemon process restarts,
matching the standard Prometheus counter semantics. Use `rate(...)`
over short windows for trend dashboards rather than reading the raw
absolute value.
