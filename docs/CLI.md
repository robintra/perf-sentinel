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
`explain`, `inspect`, `pg-stat`, `tempo`, `jaeger-query`, `demo`,
`bench` and `calibrate`. The commands themselves are stable; their
prose documentation is being filled in incrementally.

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
