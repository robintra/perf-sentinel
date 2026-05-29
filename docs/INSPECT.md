# Interactive TUI

`perf-sentinel` ships an interactive terminal UI for exploring
findings, span trees and cross-trace correlations. It exposes three
views as a single drill-down: **Analyze** (the summary dashboard),
**Inspect** (the multi-panel browser) and **Explain** (one trace's
full-screen span tree). Whichever entry point you use, you move between
the three views without leaving the TUI.

Entry points:

- `perf-sentinel analyze --tui [--input <events.json>]`: opens on the
  Analyze view.
- `perf-sentinel inspect --input <events.json>`: opens on the Inspect
  view, reads a raw events file or a pre-computed Report JSON.
- `perf-sentinel explain --tui --trace-id <id> --input <events.json>`:
  opens on the Explain view, focused on that trace.
- `perf-sentinel query --daemon <URL> inspect`: live mode, opens on
  Inspect, reads findings and traces from a running daemon over HTTP.

In live mode (0.5.24+), the TUI also lets the operator acknowledge
and revoke findings interactively from the terminal.

![all-in-one TUI: Analyze drills into Inspect then Explain, Esc walks back up](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/demo.gif)

## Views and drill-down

The three views form one drill-down. `Enter` descends, `Esc` ascends:

```
Analyze  --Enter-->  Inspect  --Enter-->  Explain
         <---Esc---           <---Esc---
```

- **Analyze**: the scrollable summary (GreenOps waste, top offenders,
  quality gate), the same content as `analyze` stdout. `Enter` descends
  to Inspect.
- **Inspect**: the multi-panel browser described below. `Enter` drills
  through the panels and, from Detail, opens Explain. `Esc` walks back
  toward Analyze.
- **Explain**: the selected trace's annotated span tree, full screen
  and scrollable. `Esc` returns to the Inspect Detail panel.

A tab bar at the top highlights the active view. Span trees need raw
spans (`inspect --input <events>.json` or `query inspect`). A
pre-computed Report carries none, so Explain shows a hint instead.

![Analyze view: the GreenOps summary dashboard under the view tab bar](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/analyze.png)

![Inspect view: the four-panel browser, traces, findings, correlations and detail](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/inspect.png)

![Explain view: a trace's full-screen annotated span tree](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/explain.png)

## Layout

The screen splits into a 2-row layout:

```
┌─ Traces ──┬─ Findings ────────────────┬─ Correlations ────┐
│ trace-1   │ [1] N+1 SQL CRITICAL      │ svc-a -> svc-b    │
│ trace-2   │ [2] Redundant SQL WARNING │ ...               │
│ ...       │ [3] Slow HTTP INFO        │                   │
├───────────┴───────────────────────────┴───────────────────┤
│ Detail (full-width, span tree + finding metadata)         │
└───────────────────────────────────────────────────────────┘
```

The active panel border highlights cyan, the rest stays gray.

## Keybindings

Navigation works with arrow keys throughout. In the Inspect view the vim
keys `h` / `j` / `k` / `l` also apply, and `j` / `k` scroll the Analyze
and Explain views.

| Key                   | Action                                            |
|-----------------------|---------------------------------------------------|
| `q`                   | Quit                                              |
| `↑` / `k`             | Move selection up, or scroll (Analyze, Explain)   |
| `↓` / `j`             | Move selection down, or scroll (Analyze, Explain) |
| `→` / `Tab` / `l`     | Cycle to next panel (Inspect)                     |
| `←` / `BackTab` / `h` | Cycle to previous panel (Inspect)                 |
| `Enter`               | Drill down one step (see below)                   |
| `Esc`                 | Walk back up one step                             |
| `a`                   | Acknowledge the selected finding (live mode)      |
| `u`                   | Revoke the existing ack (live mode)               |

`Enter` drills down: from Analyze to Inspect, then through the Inspect
panels (Traces, Findings, Detail), then from Detail to Explain. From the
Correlations panel it jumps to the Detail of the correlation's sample
trace. `Esc` reverses each step, ascending from the top Inspect panels
back to Analyze.

`a` and `u` are no-op in batch mode (`inspect --input`): acknowledgment
requires a running daemon to persist.

## Acknowledgment workflow (live mode)

When launched via `query inspect`, the TUI fetches findings with
`?include_acked=true` so already-acknowledged findings appear in the
list with an italic gray `[acked by <user>]` indicator at the end of
the line.

### `a`: create an acknowledgment

Pressing `a` on a selected finding opens a modal centered on the
screen with three input fields:

| Field   | Constraint                                 | Default                |
|---------|--------------------------------------------|------------------------|
| Reason  | 1 to 256 chars, single line                | empty (required)       |
| Expires | empty, `24h`, `7d`, ISO8601, etc           | empty (no expiration)  |
| By      | 1 to 128 chars                             | `$USER` env var        |

Plus two buttons (`Submit` / `Cancel`).

Modal navigation:

| Key              | Action                                     |
|------------------|--------------------------------------------|
| `Tab`            | Move focus to the next field / button      |
| `BackTab`        | Move focus backwards                       |
| `Enter` (text)   | Advance to the next field                  |
| `Enter` (Submit) | Submit the form                            |
| `Enter` (Cancel) | Close the modal without submitting         |
| `Esc`            | Cancel the modal                           |
| `Backspace`      | Delete the last char of the focused buffer |

On submit, the TUI posts to `/api/findings/<sig>/ack` and closes the
modal on 201. On error (4xx/5xx), the modal stays open with the
error message at the bottom (red text).

### `u`: revoke an acknowledgment

Pressing `u` on an acknowledged finding opens a confirmation modal.
`Submit` / `Enter` issues a `DELETE /api/findings/<sig>/ack`.
`Cancel` / `Esc` closes without revoking.

### Expires format

Mirrors the CLI ack helper (since 0.5.22):

- Empty: no expiration, ack persists until manually revoked
- `24h`, `7d`, `30m`: relative duration parsed by humantime
- `2026-05-11T00:00:00Z`: ISO8601 absolute datetime

Invalid input shows `expires: <error>` in the modal footer without
sending the request.

## Authentication

The TUI mirrors the CLI ack helper's auth resolution:

1. `PERF_SENTINEL_DAEMON_API_KEY` env var (priority 1)
2. `--api-key-file <path>` flag on `query inspect` (priority 2)

```bash
# env var
export PERF_SENTINEL_DAEMON_API_KEY=$(cat ~/.config/perf-sentinel/key)
perf-sentinel query --daemon http://localhost:4318 inspect

# file
perf-sentinel query --daemon http://localhost:4318 inspect \
  --api-key-file ~/.config/perf-sentinel/key
```

Both are equivalent. The file path supports `O_NOFOLLOW` symlink
refusal on Unix and trims trailing newlines.

**No interactive password prompt in the TUI.** Raw mode and the
alternate screen are incompatible with `rpassword` TTY input. If the
daemon answers 401 without an env or file key, the modal shows
"API key required: set PERF_SENTINEL_DAEMON_API_KEY or pass
--api-key-file when launching `query inspect`." Quit, set the key,
relaunch.

When the daemon has no `[daemon.ack] api_key` configured (default for
loopback deployments), no key is needed and the modal just submits.

## Caveats

### Synchronous HTTP freezes the UI

`run_loop` is synchronous and the daemon ack write is performed via
`tokio::runtime::Handle::current().block_on(...)` from inside the
loop. The UI freezes for the duration of the request, typically
100-300 ms on localhost, longer over the network. Acceptable for a
scope-minimal release. An async event loop refactor is a candidate
followup if user feedback signals friction.

### Findings list snapshot

The findings list is fetched once at boot. `a`/`u` refresh only the
ack state via a second `GET /api/findings?include_acked=true`, the
list of findings itself does not change in-session. To pick up newly
ingested traces, quit and relaunch.

### TOML acks visible, not modifiable

Findings acked in `.perf-sentinel-acknowledgments.toml` (CI ack)
appear with the `[acked by <user>]` indicator and the source field
set to `toml`. The TUI cannot promote a daemon ack to TOML or edit
the TOML file. For permanent acks, edit the file via PR review per
[`ACK-WORKFLOW.md`](./ACK-WORKFLOW.md).

## See also

- [`ACK-WORKFLOW.md`](./ACK-WORKFLOW.md) for the relationship between
  TOML CI acks and daemon JSONL acks, and the full decision table.
- [`CLI.md`](./CLI.md) for the `perf-sentinel ack` subcommand
  reference (CLI-side equivalent of `a`/`u`).
- [`HTML-REPORT.md`](./HTML-REPORT.md) for the browser-side ack flow
  via `--daemon-url`.
- [`CONFIGURATION.md`](./CONFIGURATION.md) for the `[daemon.ack]`
  server-side config reference.
