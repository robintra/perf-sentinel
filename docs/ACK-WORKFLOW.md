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
messages. See [`CLI.md`](./CLI.md#ack) for the full reference.

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

## Observability

The daemon exposes Prometheus counters on `/metrics` for every ack
operation it processes (`perf_sentinel_ack_operations_total{action}`
and `perf_sentinel_ack_operations_failed_total{action,reason}`). See
[`METRICS.md`](./METRICS.md) for the full schema and example
PromQL queries.
