# CLI reference

This page documents the user-facing subcommands of the `perf-sentinel`
binary. For deeper architecture and design notes, see
[`ARCHITECTURE.md`](./ARCHITECTURE.md). For runtime hooks (CI gates,
exit codes, env vars), see [`CI.md`](./CI.md) and
[`RUNBOOK.md`](./RUNBOOK.md).

A full inventory of options is also available via `--help` on each
subcommand:

```bash
perf-sentinel --help
perf-sentinel <subcommand> --help
```

The sections below are not exhaustive for every subcommand; they
focus on the user surfaces that benefit from prose context (workflow,
defaults, exit codes). For exhaustive flag listings, prefer `--help`.

## capture

Receives OTLP traces and writes them to a file, so a CI job can produce
the input `analyze --ci` gates on without running an OpenTelemetry
Collector. It only receives and writes, it never analyzes: the verdict
stays with `analyze`, on a file you can keep as a build artifact, replay
with different thresholds, or compare against a baseline with `diff`.

The application needs no perf-sentinel-specific setting, only the
standard endpoint variables. Set the protocol too: SDKs disagree on
their default, and an endpoint pointed at the wrong one exports nothing
without saying so.

```bash
# OTLP HTTP, what most SDKs default to
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf

# OTLP gRPC, the opentelemetry-java default
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

### Two shapes

**Wrapping the test step**, which is the sturdier one:

```bash
perf-sentinel capture --output traces.json -- mvn verify
perf-sentinel analyze --ci --input traces.json
```

The ports are bound before the command starts, so no export is lost to a
start-up race, and the capture ends when the command does rather than on
a guessed delay. The command inherits stdout and stderr untouched, and
its exit code is propagated: a failing test run stays a failing job.

**Alongside an existing test step**, when your pipeline owns the test
command and it cannot be prefixed:

```bash
perf-sentinel capture --output traces.json &
CAPTURE=$!
./scripts/run-integration-tests.sh
kill -TERM $CAPTURE && wait $CAPTURE
```

> **Prefix the existing step, never add a second one.** `capture -- mvn
> verify` runs the tests once. A new pipeline stage next to the existing
> one would run the whole integration suite twice, for nothing.

### Output

NDJSON, one OTLP request per line, the shape the Collector `file`
exporter produces. `analyze`, `report` and `diff` auto-detect it, no
flag needed. Requests are written as received, unconverted, so the file
describes what the application actually sent.

Progress and the final count go to **stderr**, never stdout: in wrapper
mode that stream belongs to the wrapped command. The summary is how you
tell "no anti-patterns found" from "nothing was ever exported", and an
empty trace file is rejected by `analyze` rather than reported as a
clean gate.

### Options worth knowing

| Flag | Default | Why you would change it |
|---|---|---|
| `--listen-address` | `127.0.0.1` | `0.0.0.0` when the application runs in another container of the same job |
| `--listen-port-grpc` / `--listen-port-http` | `4317` / `4318` | a port is already taken on the agent |
| `--max-file-size` | `512` (MiB) | a large suite. Past the cap the file stays valid but incomplete, and the run exits `2` rather than pretending |
| `--grace-ms` | `2000` | how long to keep listening after the command exits, for the exporter's last flush |

### Exit codes

- `0`: capture completed.
- the wrapped command's own code when it failed, since that is the more
  important signal. A command killed by a signal reports `128 + signal`,
  as a shell would, never `0`.
- `1`: the capture itself failed (port taken, unwritable file). Both are
  detected before the wrapped command starts, so a capture that cannot
  listen never leaves a test suite running.
- `2`: the trace file is short of the run, either because the size cap
  was hit or because requests could not be queued fast enough. Any
  verdict from it would understate the run.

## ack

Acknowledge findings via the daemon ack API introduced in 0.5.20.
Three subactions: `create`, `revoke`, `list`.

The CLI consumes the daemon's HTTP endpoints
(`POST/DELETE /api/findings/{sig}/ack` and `GET /api/acks`). It does
not edit the TOML CI baseline
(`.perf-sentinel-acknowledgments.toml`); that file is meant to be
edited by hand and shipped via PR review. See
[`ACK-WORKFLOW.md`](./ACK-WORKFLOW.md) for guidance on choosing
between the two ack mechanisms.

### Synopsis

```bash
perf-sentinel ack [OPTIONS] <SUBCOMMAND>
```

Top-level options (apply to all subactions):

- `--daemon <URL>`: daemon HTTP endpoint. Defaults to
  `$PERF_SENTINEL_DAEMON_URL` then `http://localhost:4318`.

### `ack create`

Create a new acknowledgment.

```bash
perf-sentinel ack create \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef0123456789abcdef" \
  --reason "deferred to next sprint" \
  --expires 7d
```

Options:

- `--signature <SIG>` (or `-s`): finding signature to acknowledge. If
  omitted, the CLI reads it from stdin (only when stdin is not a TTY).
  The stdin read is capped at 1 KiB so a `cat /dev/urandom` pipe cannot
  exhaust memory before the daemon-side validator rejects the input.
- `--reason <TEXT>` (or `-r`): required, free-form description of why
  the finding is being acked.
- `--expires <ISO8601_OR_DURATION>`: ack expiration. Accepts ISO8601
  datetimes (`2026-05-11T00:00:00Z`) or relative durations (`7d`,
  `24h`, `30m`). Omit for a permanent ack.
- `--by <NAME>`: identity of the acker. Falls back to `$USER`, then
  `"anonymous"`.
- `--api-key-file <PATH>`: see "Authentication" below.

### `ack revoke`

Remove an existing acknowledgment.

```bash
perf-sentinel ack revoke \
  --signature "n_plus_one_sql:order-svc:_api_orders:0123456789abcdef0123456789abcdef"
```

### `ack list`

Enumerate active daemon acknowledgments.

```bash
perf-sentinel ack list
perf-sentinel ack list --output json
```

`ack list` only shows daemon-side acks. TOML CI acks remain visible
in `.perf-sentinel-acknowledgments.toml` itself. The daemon caps the
response at 1000 entries.

### Authentication

When the daemon enforces an API key (`[daemon.ack] api_key` in the
daemon config), the CLI resolves it in priority order:

1. `PERF_SENTINEL_DAEMON_API_KEY` environment variable.
2. `--api-key-file <PATH>`. The file's content is read and any
   trailing newline is stripped.
3. Interactive `rpassword` prompt (no echo) if the daemon returns 401
   and stdin is a TTY. The pasted value is capped at 1 KiB.

There is no `--api-key <SECRET>` flag, by design: passing secrets on
the command line leaks them via the process list and shell history.

On Unix, `--api-key-file` is opened with `O_NOFOLLOW` (symlinks are
refused) and the CLI prints a one-line warning on stderr if the file
is group/world readable (`mode & 0o077 != 0`). The warning is gated
behind a TTY check on stderr: in CI / Docker / systemd contexts where
stderr is not a TTY, the warning is suppressed to keep build logs
clean. Operators running in those environments should set the file
mode declaratively (k8s Secret with `defaultMode: 0o400`, a
`StatefulSet` mounted from a `Secret`, etc.) rather than relying on
the runtime warning.

### Daemon URL resolution

`--daemon <URL>` > `PERF_SENTINEL_DAEMON_URL` env > default
`http://localhost:4318`. The default matches `perf-sentinel watch`,
which listens on the OTLP/HTTP standard port.

### Exit codes

- `0`: success.
- `1`: generic error (network failure, parse error, missing
  signature on stdin).
- `2`: client error (HTTP 4xx). Includes 401 (unauthorized), 409
  (already acknowledged), 404 (not acknowledged on revoke), 400
  (invalid signature format).
- `3`: server error (HTTP 5xx). Includes 503 (ack store disabled),
  500 (write failure) and 507 (ack store full).

Errors are written to stderr with a one-line cause and an actionable
hint when applicable.

## Other subcommands

For now, see `perf-sentinel <subcommand> --help` for the exhaustive
option lists of `analyze`, `watch`, `query`, `report`, `diff`,
`explain`, `inspect`, `pg-stat`, `mysql-stat`, `tempo`, `jaeger-query`, `demo`,
`bench` and `calibrate`. The commands themselves are stable; their
prose documentation is being filled in incrementally.

The supply-chain trio has dedicated prose documentation elsewhere:
`disclose`, `verify-hash` and `hash-bake` are covered in
[`REPORTING.md`](./REPORTING.md), with the signing and provenance
background in [`SUPPLY-CHAIN.md`](./SUPPLY-CHAIN.md). Shell
completions and the man page are documented below on this page.

## Shell completions

`perf-sentinel completions <shell>` writes a completion script to
stdout. Supported shells: `bash`, `zsh`, `fish`, `powershell`,
`elvish`. Pipe the output to the shell-specific completion path:

```bash
# Zsh (oh-my-zsh, prezto, manual fpath)
perf-sentinel completions zsh > ~/.zfunc/_perf-sentinel

# Bash
perf-sentinel completions bash > /usr/local/etc/bash_completion.d/perf-sentinel

# Fish
perf-sentinel completions fish > ~/.config/fish/completions/perf-sentinel.fish
```

Reload your shell, or `source` the file, after install. Re-run the
generator after upgrading `perf-sentinel` so completions stay in sync
with new flags and subcommands.

## Man page

`perf-sentinel man` writes a roff man page to stdout. It renders the
top-level page, which lists the subcommands (like `git.1`). Redirect it
into your man path:

```bash
perf-sentinel man > /usr/local/share/man/man1/perf-sentinel.1
```

`man perf-sentinel` then works. To preview without installing:

```bash
perf-sentinel man > /tmp/perf-sentinel.1 && man /tmp/perf-sentinel.1
```

Re-run the generator after upgrading `perf-sentinel` so the page stays
in sync with new flags and subcommands.
