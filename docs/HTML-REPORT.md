# HTML report

`perf-sentinel report` produces a self-contained HTML dashboard for
post-mortem exploration of a trace set. It runs in two modes:

- **Static** (default, since 0.5.0): the HTML file embeds every panel
  and every trace tree as JSON. No network egress, no daemon
  connection. Acceptable to upload as a CI artifact (GitLab Pages,
  GitHub Pages, Artifactory, S3 static hosting). The output is
  identical for everyone who opens it.
- **Live** (since 0.5.23, opt-in via `--daemon-url`): the HTML file
  reaches a running daemon at runtime for ack/revoke interactions. The
  dashboard adds per-finding `Ack`/`Revoke` buttons, a connection
  status indicator, an Acknowledgments panel, a `Show acknowledged`
  toggle, and a manual refresh button. The static panels (Findings,
  Explain, pg_stat, Diff, Correlations, GreenOps) keep the same
  static-mode behavior, the live mode is purely additive.

## Static mode

```bash
perf-sentinel report --input traces.json --output report.html
open report.html
```

That is the artifact every CI job can produce. Without `--daemon-url`,
the generated HTML is byte-equivalent to the 0.5.22 output for the
same input. CSP stays strict (`default-src 'none'`), there is no
`fetch()` call against any host.

## Live mode

```bash
perf-sentinel report --input traces.json --output report.html \
  --daemon-url http://localhost:4318
open report.html
```

The daemon must:

1. Be reachable from the browser opening the HTML. For a developer
   workstation that means `localhost:4318`. For a shared report opened
   over GitLab Pages or GitHub Pages, the daemon must expose its API
   at a host the browser can reach.
2. Have `[daemon.cors] allowed_origins` configured to include the
   document origin. See [`CONFIGURATION.md`](./CONFIGURATION.md) for
   the section reference. The browser drops the response otherwise.
3. Have `[daemon.ack] enabled = true` (default).

The first time the user clicks `Ack` or `Revoke` on a 401-protected
daemon, the report opens an authentication modal and asks for the
`X-API-Key`. The key is held in `sessionStorage`, scoped to the tab,
and cleared when the tab closes.

### CSP under live mode

Live mode rewrites the rendered Content-Security-Policy meta tag to
add `connect-src <daemon_url>`. Every other directive keeps its
static-mode value. The daemon URL is validated by the CLI before it
ever reaches the meta tag (no scheme other than http/https, no path
component, no userinfo, no query string), so no CSP-breaking byte can
land in the directive.

```text
default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline';
img-src data:; base-uri 'none'; form-action 'none';
connect-src http://localhost:4318
```

### Daemon URL validation

The CLI rejects:

- Empty input
- Non-`http`/`https` schemes
- Missing host (e.g. `http://`, `http://:8080`)
- Userinfo (e.g. `http://alice@host`, since the X-API-Key never
  belongs in a URL)
- Path components (e.g. `https://example.com/v1/`, since the report
  builds `/api/...` itself)
- Query strings and fragments

A trailing slash on the authority is silently trimmed for uniformity
with the existing `perf-sentinel ack --daemon` flag.

### Mixed-content nudge

Since the post-0.5.26 hardening pass, calling `perf-sentinel report
--daemon-url http://...` with a non-loopback host emits a `WARN`-level
event at render time. Hosting the resulting HTML on an HTTPS origin
later (GitLab Pages, GitHub Pages, an internal HTTPS reverse proxy)
makes the browser block every ack/revoke fetch as mixed content,
silently turning the Acks panel into a dead-end. The warning catches
that mismatch before the operator opens the report. Loopback URLs
(`localhost`, `127.0.0.1`, `[::1]`) are exempt because dev setups
intentionally run the daemon on cleartext HTTP.

### Authentication flow

1. Boot: GET `/api/status` to determine connectivity. The status
   endpoint is unauthenticated (read-only, no secrets), so the badge
   in the top bar reaches `Connected` without a key.
2. First `Ack`/`Revoke` click: POST or DELETE on `/api/findings/<sig>/ack`.
   On 401, the auth modal opens with a password input (no echo). The
   key is stored in `sessionStorage` under
   `perf-sentinel.daemon.api-key` and the failed request retries.
3. Subsequent calls: every authenticated request reads the key from
   `sessionStorage` and sets `X-API-Key`.
4. Tab close: `sessionStorage` clears, the next reload re-prompts on
   the first authenticated call.

### What lives where

| Element                           | Mode    | Details                                                                                                  |
|-----------------------------------|---------|----------------------------------------------------------------------------------------------------------|
| Top bar daemon status badge       | Live    | Three states: `Connected` (green), `Authentication required` (orange), `Disconnected` / `Unreachable` (red) |
| Top bar refresh button            | Live    | Re-fetches `/api/status`, `/api/acks`, and re-renders the live state                                     |
| Per-row `Ack` / `Revoke` buttons  | Live    | Hidden in static mode via CSS, revealed under `body.ps-live`                                             |
| `Show acknowledged` toggle        | Live    | Filters the static findings list against the live `/api/acks` set                                        |
| Acknowledgments panel             | Live    | New tab `Acks` listing the daemon-side acks (paginated at 1000, daemon cap)                              |
| Authentication modal              | Live    | Triggered by the first 401 on a write call, never on `/api/status`                                       |
| Acknowledgment modal              | Live    | Triggered by `Ack`. Form fields: reason (required), expires (Never / 24h / 7d / 30d), by (optional)      |

### Limitations

- The daemon-side findings list is not refetched on toggle: the
  static report is the source of truth for the findings list, and the
  toggle only filters against the live acks set. To see findings the
  daemon has retained beyond the static snapshot, use
  `perf-sentinel query findings --include-acked` or the daemon HTTP API
  directly.
- No automatic refresh timer. The browser does not poll the daemon
  unattended; use the manual refresh button. Real-time monitoring
  belongs in Grafana, not in a per-MR HTML artifact.
- No per-row `Explain` cross-link in live mode beyond the static
  static behavior. Ack/Revoke does not take the user away from the
  Findings tab.
- No bulk operations. Ack one finding at a time.
- `sessionStorage` is purged at tab close, by design. Do not stash
  long-lived secrets in a CI artifact opened in a shared browser
  profile.

### Security caveat

The X-API-Key is stored unencrypted in `sessionStorage`. That is
acceptable for an operator on their personal workstation, where
`sessionStorage` is scoped to a single tab and cleared at tab close.
It is not acceptable on a shared host, since any other code running
in the same tab session can read `sessionStorage`. The report ships a
strict CSP that forbids cross-origin script loading and inline
event handlers, which mitigates the risk but does not eliminate it.

**`script-src 'unsafe-inline'` caveat**: the dashboard inlines its
JavaScript inside the HTML file (the report is a single self-contained
artifact, no external resources). The CSP keeps `script-src
'unsafe-inline'` for that reason. In live mode, `connect-src` is
limited to `'self'` plus the operator-passed daemon URL, so even if a
future template change introduced an XSS vector, the only outbound
destinations available are the document's own origin and the daemon
itself, not an arbitrary attacker host. A future hardening (out of
scope for 0.5.23) would ship the JS in a separate `<script>` block
hashed via `'sha256-...'` and drop `'unsafe-inline'`. Track in
[`LIMITATIONS.md`](./LIMITATIONS.md) when that work lands.

**CORS preflight DoS surface**: when `[daemon.cors] allowed_origins`
is set, the daemon answers `OPTIONS` preflight requests on `/api/*`
without authentication (the X-API-Key check runs after CORS). A rogue
origin in the whitelist (or any origin under wildcard mode) can
issue unbounded preflights that bypass the ack auth boundary. The
daemon does not yet ship a rate limiter on this surface. The
`max_age=120s` preflight cache mitigates the volume from legitimate
browsers but does not help against a malicious script. Mitigation
posture for 0.5.23: deploy the daemon behind a reverse proxy with
per-IP rate limiting (nginx `limit_req`, Caddy `rate_limit`,
Cloudflare WAF) when exposing it cross-origin. A native
`tower-governor` integration is tracked for a future release.

If your threat model includes a shared browser profile, generate the
HTML in static mode and use the CLI (`perf-sentinel ack`) for ack
operations.

## Smoke test (manual)

The acceptance procedure for `--daemon-url`:

```bash
# 1. Static baseline
perf-sentinel report --input traces.json --output /tmp/static.html
open /tmp/static.html
# Verify: no daemon badge, no Ack buttons, no Acknowledgments tab.

# 2. Daemon with CORS open
cat > /tmp/daemon.toml <<EOF
[daemon.cors]
allowed_origins = ["*"]

[daemon.ack]
enabled = true
EOF
perf-sentinel watch --config /tmp/daemon.toml &
DAEMON_PID=$!
sleep 1

# 3. Live report
perf-sentinel report --input traces.json --output /tmp/live.html \
  --daemon-url http://localhost:4318
open /tmp/live.html
# Verify: green Connected badge, Ack buttons present on every row,
# Acks tab visible, refresh button visible.

# 4. Click Ack on any finding, fill the modal, submit. The badge in
# the row swaps to Revoke.

# 5. Click Revoke, confirm. The badge swaps back to Ack.

# 6. Restart the daemon with [daemon.ack] api_key set. Generate a
# fresh secret per run, never paste a literal in production:
kill $DAEMON_PID
SMOKE_KEY=$(openssl rand -hex 16)
cat >> /tmp/daemon.toml <<EOF
api_key = "${SMOKE_KEY}"
EOF
perf-sentinel watch --config /tmp/daemon.toml &
DAEMON_PID=$!
sleep 1
# Reload /tmp/live.html, click Ack: an authentication modal opens,
# enter $SMOKE_KEY, submit. The ack request retries automatically.

# 7. Reload the tab again. The key persists in sessionStorage, no
# re-prompt until you close the tab.

kill $DAEMON_PID
```

## Choosing between static and live

| Use case                                                  | Mode    |
| --------------------------------------------------------- | ------- |
| CI artifact uploaded on every MR                          | Static  |
| MR review where the reviewer wants to ack or revoke       | Live    |
| Onboarding doc bundled in a tarball                       | Static  |
| Live ops dashboard on a personal workstation              | Live    |
| Shared browser profile (kiosk, demo machine)              | Static  |
| Air-gapped offline analysis                               | Static  |

## See also

- [`CONFIGURATION.md`](./CONFIGURATION.md) for the `[daemon.cors]`
  config section.
- [`ACK-WORKFLOW.md`](./ACK-WORKFLOW.md) for the relationship
  between TOML CI acks and daemon JSONL acks.
- [`CLI.md`](./CLI.md) for the `perf-sentinel ack` subcommand
  reference.
