# CLI, configuration and release profile

## CLI design

The CLI (`sentinel-cli`) is intentionally thin. It parses arguments with [clap](https://docs.rs/clap/) and delegates to `sentinel-core` functions. Ten subcommands are available: `analyze`, `explain`, `watch`, `demo`, `bench`, `pg-stat`, `inspect`, `query`, `diff` and `report`.

### Analyze: colored report by default, JSON with `--ci`

`perf-sentinel analyze` displays a colored terminal report when run interactively (without `--ci`). This is the output humans see when running the tool locally. With `--ci`, the output switches to structured JSON for machine consumption and the process exits with code 1 if the quality gate fails.

This follows the convention of tools like `cargo test` (colored output by default, `--format json` for CI).

The `--format` flag provides explicit control over the output format: `text` (colored terminal, default), `json` (structured report) or `sarif` (SARIF v2.1.0 for code scanning). When `--ci` is used without `--format`, the output defaults to JSON for backward compatibility.

### Explain: per-trace tree view

`perf-sentinel explain --input FILE --trace-id ID` builds a tree from `parent_span_id` relationships and annotates findings inline. It runs per-trace detectors only (N+1, redundant, slow, fanout), cross-trace percentile findings are not included.

Output formats: `--format text` (colored tree with Unicode box-drawing characters, default) or `--format json` (nested JSON structure). Both include a `MAX_TREE_DEPTH` guard of 256 levels to prevent stack overflow on deeply nested traces.

### Bench: pre-cloned batches

```rust
let batches: Vec<Vec<SpanEvent>> = (0..iterations)
    .map(|_| events.clone())
    .collect();
```

Input batches are cloned **before** timing begins. This ensures the benchmark measures only `pipeline::analyze` performance, not `Vec<SpanEvent>::clone` overhead. Since `analyze` consumes its input (`Vec<SpanEvent>` is moved), each iteration needs its own copy.

### Percentile computation

```rust
per_event_ns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
let p50_idx = ((per_event_ns.len() as f64 * 0.50).ceil() as usize).saturating_sub(1);
let p99_idx = ((per_event_ns.len() as f64 * 0.99).ceil() as usize)
    .saturating_sub(1)
    .min(per_event_ns.len() - 1);
```

The ceiling-based index computation follows the [nearest-rank method](https://en.wikipedia.org/wiki/Percentile#The_nearest-rank_method) for percentiles. The `.saturating_sub(1)` converts from 1-based rank to 0-based index. The `.min(len - 1)` prevents out-of-bounds access when `ceil` rounds up to `len`.

### Throughput from nanoseconds

```rust
let elapsed_nanos: u64 = durations_ns.iter().sum();
let total_seconds = elapsed_nanos as f64 / 1_000_000_000.0;
let throughput = if total_seconds > 0.0 { total_events / total_seconds } else { 0.0 };
```

Throughput is computed from nanosecond precision (not millisecond) to avoid division-by-zero when iterations complete in less than 1ms. The `total_elapsed_ms` field in the output is derived from nanoseconds for display purposes.

### RSS measurement

Platform-specific memory measurement:

| Platform | Method                                        | Unit                    |
|----------|-----------------------------------------------|-------------------------|
| Linux    | `/proc/self/status` -> `VmRSS` line           | KB (converted to bytes) |
| macOS    | `libc::getrusage(RUSAGE_SELF)` -> `ru_maxrss` | Bytes (on macOS)        |
| Windows  | Not implemented                               | Returns `None`          |

The macOS implementation uses `unsafe` for the `libc::getrusage` FFI call. This is justified, there is no safe Rust API for this syscall and the function is well-documented in POSIX. The return value is checked (`if ret == 0`) before using the result.

### Colored output with TTY detection

```rust
let is_tty = force_color || std::io::stdout().is_terminal();
let (bold, cyan, red, yellow, green, dim, reset) = if is_tty {
    ("\x1b[1m", "\x1b[36m", "\x1b[31m", "\x1b[33m", "\x1b[32m", "\x1b[2m", "\x1b[0m")
} else {
    ("", "", "", "", "", "", "")
};
```

ANSI escape codes are suppressed when stdout is not a terminal (e.g., piped to a file or `jq`). The `force_color` parameter allows tests to exercise the color path without a real TTY. This follows the convention of tools like `ls --color=auto` and [rustc's output](https://doc.rust-lang.org/rustc/command-line-arguments.html).

**Sink override for `--output`.** The `stdout().is_terminal()` probe above is blind to the actual writer: a CLI invoked from an interactive terminal where `--output path.txt` redirects the sink to a `File` would still pick the colored palette and leak escape bytes into the file. `emit_diff` guards against this by forcing `no_colors()` whenever `output.is_some()`, regardless of the stdout TTY state. The palette is then threaded into `write_diff_text` as an explicit argument so the writer choice and the color decision stay in sync.

### PgStat: pg_stat_statements hotspot analysis

`perf-sentinel pg-stat --input FILE` parses PostgreSQL `pg_stat_statements` exports (CSV or JSON, auto-detected) and produces hotspot rankings by total execution time, call count and mean execution time. The `--traces` flag enables cross-referencing with trace-based findings: the tool runs `pipeline::analyze()` on the trace file and marks `pg_stat_statements` entries whose normalized template also appears in trace findings.

This subcommand is intentionally separate from `analyze` because `pg_stat_statements` data has no `trace_id`, it cannot participate in the trace correlation pipeline. It is a complementary analysis mode.

### Inspect: interactive TUI

`perf-sentinel inspect --input FILE` launches a terminal UI built with [ratatui](https://ratatui.rs/) and [crossterm](https://docs.rs/crossterm/). These dependencies live in `sentinel-cli/Cargo.toml` only (not `sentinel-core`) because TUI is a presentation concern.

**Layout:** 3-panel split, traces list (top-left, 30%), findings for selected trace (top-right, 70%), finding detail with span tree (bottom, 50%). The detail panel reuses `explain::build_tree()` and `explain::format_tree_text()` for the span tree display.

**State management:** the `App` struct holds pre-computed `findings_by_trace` (indexed at construction time) to avoid recomputing on every frame. Navigation state (selected_trace, selected_finding, active_panel, scroll_offset) is updated by key events.

**Data loading:** events are ingested once, then cloned. One copy for `correlate()` (needed for tree building) and one for `pipeline::analyze()` (consumed by the pipeline). This avoids re-reading the file.

### `report` subcommand

`perf-sentinel report --input FILE --output report.html` produces a single-file HTML dashboard aimed at developers exploring a CI artifact in a browser. The pipeline is identical to `analyze` end-to-end, only the final sink differs. Implemented in `crates/sentinel-core/src/report/html.rs` with the full UI template embedded via `include_str!` from `crates/sentinel-core/src/report/html_template.html`.

**Architecture: single-file, vanilla JS, no build step, no external dependencies.** The output is one HTML file with all CSS and JS inlined. No `<link rel="stylesheet">`, no `<script src="...">`, no web fonts, no images. The file opens offline from a `file://` URL with zero network requests, which makes it:

- Trivially auditable: one file, no minified bundles, no transpilation.
- Durable: no build toolchain to break on a CI runner upgrade, no NPM version drift on a recipe that's supposed to be reproducible for years.
- Safe to ship as a CI artifact: no lockfile to invalidate, no supply-chain vectors through a bundled minifier.
- Fast to review in PRs: the template is a single `.html` file that diffs cleanly.

The front-end uses DOM APIs directly (`document.createElement`, `Element.textContent`, `Element.setAttribute`). No framework. No Web Components (the prior plan considered them, but plain modules fit the 8.1 scope better in practice and keep the file ~15 KB smaller).

**Security model: `textContent` only, grep-level CI check.** All user-controlled data (SQL templates, URLs, service names, trace IDs, code locations, `SuggestedFix` text) is injected inside a `<script id="report-data" type="application/json">` block and read once at boot via `textContent` + `JSON.parse`. The JS then renders via `textContent` and `createElement` exclusively. Forbidden: `innerHTML`, `insertAdjacentHTML`, `document.write`, `eval`, `new Function`. A unit test (`no_forbidden_apis_in_template` in `report/html.rs`) greps the template on every build and fails CI if any of those strings appear. Second-layer defense: the Rust injector escapes the substring `</` to `<\/` in the serialized JSON so a hostile user-controlled value cannot close the script block early. `\/` is a permitted JSON escape, so `JSON.parse` recovers the original value unchanged.

Only `reference_url` from `SuggestedFix` becomes a hyperlink, and only when the value starts with `https://` (validated client-side in `safeHttpsHref`). Non-HTTPS URLs render as plain text without a link.

**Scope limit: post-mortem only.** The dashboard is a static rendering of a completed trace set. No polling, no WebSocket, no Server-Sent Events, no refresh loop. The equivalent "live" view from a running daemon stays with `perf-sentinel query inspect` (TUI fed by the daemon's `/api/*` endpoints). Making the HTML dashboard live-capable would require a real-time backend binding, an update diffing strategy and state persistence across reloads - a different architecture that is out of scope here and would be re-evaluated only on user feedback.

**Composition pattern for Tempo.** Tempo-backed exploration composes via the shell rather than via an integrated `--tempo` flag on `report`: `perf-sentinel tempo --endpoint ... --search ... --output traces.json && perf-sentinel report --input traces.json --output report.html`. This avoids duplicating ~8 Tempo flags (endpoint, search tags, time window, auth, timeout, max-results, etc.) on `report` and keeps the two subcommands each responsible for one concern (ingestion vs. rendering). The same pattern applies to any other ingestion source: compose, don't plumb.

**Trace embedding and size cap.** Only traces referenced by a finding are embedded (the Explain tab is entry-point-driven from Findings, so free-navigation traces would bloat the file without earning their bytes). When `--max-traces-embedded` is unset, the sink targets a ~5 MB HTML output size, greedily dropping the lowest-IIS traces first (reuses the `top_offenders` ordering). A `trimmed_traces: { kept, total }` field in the embedded payload drives a banner in the Findings tab when trimming kicks in. Setting `--max-traces-embedded` explicitly honors the cap exactly, overriding the 5 MB heuristic.

**Exit-code semantics differ from `analyze --ci`.** `report` exits 0 even when the quality gate fails. The gate status is surfaced as a red/green badge in the HTML top bar. Users who need a CI exit signal keep using `analyze --ci`. Two subcommands, two concerns.

**Optional cross-references: pg_stat, diff, correlations.** Three optional tabs are added by dedicated flags:

- `--pg-stat <FILE>` ingests a `pg_stat_statements` CSV or JSON export via the same `parse_pg_stat` + `rank_pg_stat` path that the `pg-stat` subcommand uses. A pg_stat tab then shows the by-total-time ranking (Template, Calls, Total ms, Mean ms). The other two rankings (by calls, by mean) stay accessible via the text `pg-stat` subcommand and are not duplicated in the HTML.
- `--pg-stat-prometheus <URL>` scrapes a `postgres_exporter` endpoint one-shot via `fetch_from_prometheus`, same effect as `--pg-stat` without the intermediate file. Mutually exclusive with `--pg-stat` at the clap level (`conflicts_with`). This is a flag on `report` rather than a separate subcommand because a one-shot HTTP GET is not a streaming source that deserves its own command surface. Consistent with the rest of the CLI: if it doesn't stream, it composes.
- `--before <FILE>` deserializes a baseline report JSON (the output of `analyze --format json`), feeds it into `diff::diff_runs` against the current run, and embeds the `DiffReport`. A Diff tab then renders four sections: new findings (clickable, open Explain), resolved findings (not clickable, their traces are in the baseline which is not embedded), severity changes and endpoint metric deltas (both non-clickable tabular data).

**Correlations tab.** Only daemon-produced reports carry `correlations`. The batch pipeline does not emit them, so the tab stays hidden on batch output. The JS guards on `report.correlations?.length > 0`, so the tab lights up automatically when a future daemon-produced JSON is fed into `perf-sentinel report --input <daemon.json>`. No new field was added to the `Report` struct.

**Cross-navigation.** Two cross-navs plug the tabs together:

- Explain to pg_stat: when the active finding's trace contains a SQL span whose normalized template matches a row in pg_stat, that span gets a `ps-span-pgstat-link` class and a click handler. Clicking switches to the pg_stat tab with the matching row highlighted and a "Filtered from Explain" banner shown above the table. The banner has a "clear" link that hides it and removes the highlight. The span is not clickable when pg_stat is absent from the payload.
- Diff to Explain: rows in the `new_findings` section are clickable and delegate to the existing `openExplain` function. Rows in `resolved_findings`, `severity_changes` and `endpoint_metric_deltas` are not clickable. For a new finding whose `trace_id` has been trimmed by the size cap, the Explain panel shows "Trace not embedded (cap reached). Rerun with `--max-traces-embedded <higher>` to include it." instead of an empty tree.

**Search via `/`.** Each of Findings, pg_stat, Diff, Correlations carries a hidden `<input type="search">` at the top of its panel. The global keyboard handler catches `/` when no input is focused and the active tab is searchable, reveals the input and focuses it. `esc` with the input focused clears the filter and hides the input. Filter logic walks the active panel's rows and toggles `display: none` based on case-insensitive substring match over `textContent`. State is cleared on tab switch (no cross-tab carryover). Explain and GreenOps are no-search by design (no meaningful row list). The 500-row cap on Findings still applies.

**Baseline JSON round-trip.** `--before` requires the `Report` struct to derive `Deserialize`, which the 8.1 tree did not. The cascade adds `Deserialize` to `Report`, `Analysis`, `GreenSummary`, `QualityGate`, `QualityRule`, `TopOffender`, `CarbonReport`, `CarbonEstimate`, `RegionBreakdown` and `IntensitySource`. Optional fields with `skip_serializing_if` gain a matching `#[serde(default)]` so round-trip parses cleanly even when the source JSON omits them. Two `&'static str` fields on `CarbonEstimate` (`model`, `methodology`) and one on `RegionBreakdown` (`status`) became `String` for serde round-trip. The cost is a handful of `.to_string()` calls on construction-site constants, invisible next to the surrounding numeric work.

**Polish pass: client-side-only ergonomics.** A later iteration added CSV export, deep-link hash, session-scoped persistence and a `?` cheatsheet modal, all strictly client-side additions to `html_template.html`. No Rust sink changes, no new endpoints, no new dependencies.

- **CSV export**: every listable tab (Findings, pg_stat, Diff, Correlations) carries an Export CSV button above the list/table. The click handler runs the same filter predicate that renders the DOM, assembles RFC 4180-escaped rows with pure string concatenation (no `innerHTML` risk), and triggers a download via `Blob` + `URL.createObjectURL` + a temporary `<a download>`. The object URL is revoked on a 0ms `setTimeout` to avoid a memory leak while letting the browser complete the download. Explain (not a list) and GreenOps (single summary, regions table short enough to read in-place) do not get export buttons, on purpose.
- **Deep-link hash**: the URL fragment encodes `tab[&search=...][&ranking=...][&severity=...][&service=...]` on every tab switch, chip click and search input change. Writes go through `history.replaceState` so back/forward history is not polluted. Old-browser fallback assigns `location.hash` directly (one history push, acceptable). Reads on `DOMContentLoaded` validate the tab is registered; an unknown target or malformed hash silently falls through to defaults.
- **sessionStorage persistence**: two keys, `perf-sentinel:theme` (dark/light, read before first paint to avoid theme-flash) and `perf-sentinel:pgstat-ranking` (last-active ranking slug). Every access is wrapped in `try/catch` because Safari private mode and some enterprise policies throw on `sessionStorage.setItem`. Deliberately not `localStorage`: `file://` shares the `null` origin across all local HTML files, so localStorage would collide across unrelated reports; sessionStorage is tab-scoped and collision-free. Hash wins over sessionStorage when both carry a value.
- **Cheatsheet modal**: a `?`-triggered native `<dialog>` element (opened via `showModal()`, which implicitly applies the WAI-ARIA dialog role and traps focus for us) lists every shortcut. The `?` key is ignored when a text input is focused so typing `?` in the filter still works. Vim-style `g`-prefixed shortcuts (`g f` / `g e` / `g p` / `g d` / `g c` / `g r`) switch tabs with a 1000ms timeout on the pending `g`, hidden tabs are a silent no-op. `Esc` gains two extra priority tiers on top of the existing ladder: close the cheatsheet (highest) and clear active filter chips (lowest). Findings pagination replaces the hard 500-row cap with a `Show N more findings` button that reveals another 500 rows at a time.

### `STATIC_CSP` compile-time invariant

The static-mode `Content-Security-Policy` is the same string the template shipped before the live mode was added. It forbids every network egress and inline-execution vector except the inline `<script>` and `<style>` blocks the report itself depends on.

The placeholder substitution pipeline in `inject` rewrites three tokens (`{{REPORT_JSON}}`, `{{PAGE_TITLE}}`, `{{CONTENT_SECURITY_POLICY}}`) in a fixed order. Any byte sequence `{{` that lands in `STATIC_CSP` would shadow that pipeline and corrupt the substitution silently.

A `const _: () = { ... while ... assert!(...) }` block at compile time checks that `STATIC_CSP.as_bytes()` never contains `{{`. The runtime `debug_assert!` in `inject` catches the daemon-URL half (validated by `validate_url`), the const block catches the static half so a future edit that introduces a templating bracket breaks the build instead of silently corrupting the output. `const _: () = ...` is the canonical pattern for an anonymous compile-time check that does not warn under `dead_code`.

### Feature flags

The workspace uses Cargo feature flags to keep daemon-only dependencies optional:

| Feature  | Crate           | What it gates                                                                                                                                          |
|----------|-----------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| `daemon` | `sentinel-core` | `hyper`, `hyper-util`, `http-body-util`, `bytes`, `arc-swap`. Enables the `daemon/` module tree, Scaphandre scraper/state, cloud energy scraper/state. |
| `daemon` | `sentinel-cli`  | Forwards to `sentinel-core/daemon`. Enables the `watch` subcommand.                                                                                    |
| `tui`    | `sentinel-cli`  | `ratatui`, `crossterm`. Enables the `inspect` subcommand.                                                                                              |

Both `daemon` and `tui` are in the `default` feature set for the CLI. Users of `sentinel-core` as a library dependency can depend on it without `daemon` to avoid pulling in the hyper stack:

```toml
perf-sentinel-core = { version = "0.3", default-features = false }
```

This compiles the full batch pipeline (normalize, correlate, detect, score, report) without any HTTP client code. Config types (`ScaphandreConfig`, `CloudEnergyConfig`) are always available so the TOML parser works regardless of features; only the runtime scrapers and state types are gated.

## Source code location on findings

### `CodeLocation` struct

When OTel spans carry source code attributes (`code.function`, `code.filepath`, `code.lineno`, `code.namespace`), these are extracted during OTLP conversion and stored on `SpanEvent` as four optional fields. The detection pipeline propagates them to `Finding.code_location: Option<CodeLocation>`:

```rust
pub struct CodeLocation {
    pub function: Option<String>,
    pub filepath: Option<String>,
    pub lineno: Option<u32>,
    pub namespace: Option<String>,
}
```

All four fields are optional and independently present. Most auto-instrumented OTel agents emit `code.function` and `code.namespace` but not `code.filepath` or `code.lineno`. The system degrades gracefully: findings without source attributes appear without a source line, with no noise in the output.

### CLI display

When `code_location` is present, the CLI renders a "Source:" line below the finding's endpoint:

```
    Source:   com.example.OrderService.processItems (OrderService.java:42)
```

The format is `namespace.function (filepath:lineno)`, with each part omitted if absent. The rendering logic builds the string incrementally: namespace and function are joined with a dot, filepath and lineno are appended in parentheses only when the name portion is also present.

### SARIF `physicalLocation` enhancement

When a finding carries a `CodeLocation` with at least a `filepath`, the SARIF output includes a `locations` array with a `physicalLocation` entry:

```json
{
  "physicalLocation": {
    "artifactLocation": { "uri": "OrderService.java" },
    "region": { "startLine": 42 }
  }
}
```

The `region.startLine` field is included only when `lineno` is available. This enables inline annotations in GitHub Code Scanning and GitLab SAST when the SARIF report is uploaded as a code scanning result.

### `code.filepath` sanitization

The `code.filepath` OTel attribute is attacker-controlled (a hostile span can set it to any string). Before emitting it as the SARIF `artifactLocation.uri`, `sanitize_sarif_filepath` rejects values that could phish a viewer or bypass code scanning resolvers. The sanitizer drops the URI entirely (returns `None`) for any of:

- Absolute paths (POSIX `/...`, Windows `\...`).
- Any colon. Legitimate source paths in instrumented apps do not contain colons; rejecting unconditionally avoids subtle bypasses around `javascript:`, `data:`, `file:`, etc.
- Path traversal segments. Both literal `..` and percent-encoded variants (`%2e%2e`, `%2E%2E`, mixed case, `.%2e`, `%2e.`) are caught.
- Double-encoded percent sequences (`%25...`) that decode to a percent on first pass and to a real character on a second pass.
- Overlong UTF-8 prefixes (`%c0`, `%c1`) that decode to non-canonical encodings of ASCII characters in lax decoders.
- Control characters (newlines, NUL, etc.) that could break a SARIF consumer's tokenizer or inject into logs.
- Unicode BiDi overrides and invisible format characters (`U+061C`, `U+180E`, `U+202A..U+202E`, `U+2066..U+2069`, `U+200B..U+200F`, `U+FEFF`) that can confuse rendered filenames (Trojan Source class of attack, CVE-2021-42574).

Findings with a rejected filepath still appear in the SARIF report; only the `physicalLocations` array is omitted (the `logicalLocations` and other fields remain).

## `query` subcommand

`perf-sentinel query` queries a running daemon's HTTP API. It requires the `daemon` feature flag.

### Sub-actions

| Sub-action     | API endpoint              | Output                            | Description                                                                              |
|----------------|---------------------------|-----------------------------------|------------------------------------------------------------------------------------------|
| `findings`     | `/api/findings`           | colored text (default) or JSON    | List recent findings with `--service`, `--finding-type`, `--severity`, `--limit` filters |
| `explain`      | `/api/explain/{trace_id}` | colored tree (default) or JSON    | Show the explain tree for a trace from daemon memory                                     |
| `inspect`      | `/api/findings`           | ratatui TUI                       | Interactive 3-panel TUI fed from live daemon data                                        |
| `correlations` | `/api/correlations`       | colored table (default) or JSON   | Show active cross-trace correlations                                                     |
| `status`       | `/api/status`             | colored summary (default) or JSON | Show daemon health (version, uptime, active traces, stored findings count)               |

All sub-actions except `inspect` accept `--format text|json`. The default is `text` (colored terminal output), matching the `analyze` command's default. `--format json` outputs raw JSON for scripting and automation.

### Colored output

`findings` reuses the existing `print_findings()` function from the `analyze` command, so the colored output is identical: severity-colored labels, source code location, template, suggestion, green impact.

`explain` deserializes the daemon's JSON response into an `ExplainTree` and calls `format_tree_text()` for the colored span tree with inline findings, identical to `perf-sentinel explain`.

`inspect` fetches all findings from `/api/findings?limit=10000`, then for each distinct `trace_id` it fetches the explain tree from `/api/explain/{trace_id}` and deserializes it into an `ExplainTree`. The pre-rendered colored trees are passed to the TUI via `App::with_pre_rendered_trees`, so the detail panel shows real span trees (not empty stubs) for every trace still in the daemon's `TraceWindow`. Traces that aged out return the detail panel without a tree (silent skip, no confusing empty panel).

`correlations` renders a custom colored table with confidence percentage color-coded (red >= 80%, yellow >= 50%).

`status` renders a key-value display with version, formatted uptime (Xh Ym Zs), active traces and stored findings count.

### Implementation

The `cmd_query` function builds a closure around `http_client::fetch_get` that handles connection failures with a helpful error message ("Is `perf-sentinel watch` running?"). Each sub-action constructs the appropriate URL path, fetches the response and renders it according to the `--format` flag.

The default daemon URL is `http://localhost:4318`, matching the daemon's default HTTP listen port. Users can override it with `--daemon http://host:port`.

## Configuration parsing

### Sectioned format (only format accepted from 0.6.0)

The config requires a sectioned form: every tunable lives under
`[thresholds]`, `[detection]`, `[green]` or `[daemon]`.

```toml
[detection]
n_plus_one_min_occurrences = 5
```

`serde(default)` produces `None` for absent fields. The `From<RawConfig> for Config` conversion is a flat `.unwrap_or(default)` per field:

```rust
n_plus_one_threshold: raw.detection.n_plus_one_min_occurrences
    .unwrap_or(defaults.n_plus_one_threshold),
```

### 0.6.0 breaking change: 8 legacy top-level keys removed

`load_from_str` runs `reject_legacy_top_level_keys` before the typed
`RawConfig` parse. Eight 0.5.x top-level keys (`n_plus_one_threshold`,
`window_duration_ms`, `listen_addr`, `listen_port`, `max_active_traces`,
`trace_ttl_ms`, `max_events_per_trace`, `max_payload_size`) now produce
a `ConfigError::Validation` whose message names both the removed key
and its sectioned replacement, so `cargo run --bin perf-sentinel watch`
on a 0.5.x config fails fast and tells the operator exactly what to
edit. The full migration table is in `docs/CONFIGURATION.md`.

### Validation bounds

Every numeric field has explicit bounds in `validate()`:

| Field                        | Min   | Max                  | Rationale                                                                            |
|------------------------------|-------|----------------------|--------------------------------------------------------------------------------------|
| `max_payload_size`           | 1,024 | 104,857,600 (100 MB) | Prevent disabling input protection                                                   |
| `max_active_traces`          | 1     | 1,000,000            | Prevent unbounded memory                                                             |
| `max_events_per_trace`       | 1     | 100,000              | Prevent per-trace OOM                                                                |
| `max_retained_findings`      | 0     | 10,000,000           | Prevent OOM on the findings store. `0` is documented as "disable the store entirely" |
| `n_plus_one_threshold`       | 1     | *(none)*             | At least 1 occurrence to detect                                                      |
| `window_duration_ms`         | 1     | *(none)*             | Non-zero window                                                                      |
| `slow_query_threshold_ms`    | 1     | *(none)*             | Non-zero threshold                                                                   |
| `slow_query_min_occurrences` | 1     | *(none)*             | At least 1 occurrence                                                                |
| `max_fanout`                 | 1     | 100,000              | Prevent disabling detection                                                          |
| `trace_ttl_ms`               | 100   | 3,600,000 (1 h)      | Minimum eviction interval                                                            |
| `sampling_rate`              | 0.0   | 1.0                  | Valid probability                                                                    |
| `io_waste_ratio_max`         | 0.0   | 1.0                  | Valid ratio                                                                          |

The non-loopback `listen_addr` check emits a warning but does not reject:

```rust
tracing::warn!(
    "Daemon configured to listen on non-loopback address: {}. \
     Endpoints have no authentication: use a reverse proxy or \
     network policy for security.",
    self.listen_addr
);
```

This allows advanced users to bind to `0.0.0.0` when running behind a reverse proxy, while making the security implications explicit.

### Windows path normalization

`.perf-sentinel.toml` accepts path-keyed fields (`hourly_profiles_file`, `calibration_file`, `json_socket`, `tls_cert_path`, `tls_key_path`) as basic TOML strings, where `\` is normally an escape introducer. A literal Windows path like `C:\temp\sock` written in a basic string raises a TOML parse error because `\t` is interpreted as a tab escape.

To make Windows configs work without forcing operators to write doubled backslashes (`C:\\temp\\sock`), `load_from_str` runs a narrow pre-processor before the TOML parse:

1. **`normalize_toml_path_strings`** scans the raw input line by line. For lines whose key is in `TOML_PATH_STRING_KEYS` and whose value is a basic string (`"..."`), it rewrites the value with `escape_toml_path_backslashes`.
2. **`escape_toml_path_backslashes`** walks the inner string in runs of consecutive `\`:
   - run of 1: emit `\\` (bare `\` becomes a TOML escape pair).
   - run of 2 or more: emit as-is (already a valid escape pair or an embedded `\\\\` that the user wrote intentionally).
   - run of 2 at the *very start* of the value, not followed by another `\`: emit `\\\\` (4 backslashes) so a raw UNC `\\server\share` decodes back to `\\server\share`.
3. **`find_basic_string_end`** locates the closing `"` of the basic string with a linear consecutive-backslash counter (the number of `\` immediately before the `"` must be even). The previous lookbehind implementation was O(n²) on adversarial inputs full of `\`.
4. **Fallback**: if the normalized input fails to parse but the original would have worked, `load_from_str` retries with the original and emits a `tracing::debug!` line so the divergence stays diagnosable without warning on every legit Windows-path config.

Untouched by this normalization: TOML literal strings (`'C:\temp\sock'`, which already treat `\` literally) and any key not in `TOML_PATH_STRING_KEYS`. As a side effect, TOML escape sequences (`\t`, `\n`, `\u00XX`) inside the targeted keys are treated as literal byte pairs rather than escapes. This is by design for filesystem paths and is documented in the helper's rustdoc.

A small UTF-8 invariant: `normalize_toml_path_line` builds the rewritten line by slicing on `[..value_start]` (exclusive) and pushing the opening `"` explicitly, so `value_start` is never used as the end of an inclusive byte range. The byte at `value_start` is ASCII `"` in practice, but the explicit form locks the invariant for future readers.

### Comfort-zone warnings

Beyond the hard validation bounds, `validate_daemon_limits` and `validate_detection_params` emit a one-shot `tracing::warn!` at config load when a value falls outside a recommended "comfort zone" around the default. The warning is informational: the daemon still runs.

Comfort zones bracket each default by roughly 1 to 2 orders of magnitude. They were picked from the same defaults already in `Config::default()`:

| Field                   | Comfort zone             | Note                                    |
|-------------------------|--------------------------|-----------------------------------------|
| `max_payload_size`      | 256 KiB to 16 MiB        |                                         |
| `max_active_traces`     | 1,000 to 100,000         |                                         |
| `max_events_per_trace`  | 100 to 10,000            |                                         |
| `max_retained_findings` | 100 to 100,000           | Skipped silently when value is `0`      |
| `trace_ttl_ms`          | 1,000 to 600,000         |                                         |
| `max_fanout`            | 5 to 1,000               |                                         |

The `warn_outside_comfort_zone` helper takes the field name, the value, both bounds and two short notes (one for "below floor", one for "above ceiling") describing the practical consequence (eviction pressure, ingest latency, detection noise...). The helper logs structured fields (`field`, `value`, `recommended_min` or `recommended_max`) so the warning is queryable in Loki / Elasticsearch.

Invariant locked by `config_defaults_sit_inside_every_comfort_zone`: `Config::default()` must never trigger a startup warning. If a default is moved outside its comfort zone, this test fails and forces an explicit re-check of the band.

User-facing summary of the bands lives in `docs/CONFIGURATION.md` next to the field tables.

## Release profile

```toml
[profile.release]
codegen-units = 1
lto = "thin"
strip = true
panic = "abort"
opt-level = 3
```

### `codegen-units = 1`

Single codegen unit enables whole-crate optimization: the compiler can inline across all modules and optimize the entire crate as one translation unit. The trade-off is longer compile time (parallel codegen is disabled). For release builds this is acceptable.

Reference: [The Rust Performance Book: Build Configuration](https://nnethercote.github.io/perf-book/build-configuration.html)

### `lto = "thin"`

[ThinLTO](https://blog.llvm.org/2016/06/thinlto-scalable-and-incremental-lto.html) provides most of the binary size and performance benefits of full LTO with significantly faster link times. Full LTO adds ~30s to link time on this project with marginal additional benefit. ThinLTO allows cross-module inlining and dead code elimination while supporting incremental builds.

### `strip = true`

Removes debug symbols from the release binary. Reduces size from ~15MB to ~8MB. Acceptable for a distributed CLI tool where users do not need debug information.

### `panic = "abort"`

Eliminates the unwinding machinery (~200KB binary savings). Since perf-sentinel is a standalone tool (not a library consumed by Rust code that catches panics with `catch_unwind`), abort-on-panic is safe and reduces both binary size and runtime overhead.

### `opt-level = 3`

Maximum optimization: aggressive inlining, loop vectorization and dead code elimination. perf-sentinel's hot path is data-processing (string matching, HashMap operations, iterator chains) that benefits from inlining. The [Cargo documentation](https://doc.rust-lang.org/cargo/reference/profiles.html) notes that the difference between `opt-level = 2` and `3` is primarily more aggressive inlining, which is exactly what a pipeline tool needs.

The alternative `opt-level = "s"` (optimize for size) was considered but rejected: the binary size difference is marginal (~200KB), while the throughput difference can reach 10-30% on data-processing workloads.

### Allocator on musl builds

Linux release binaries target `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` so the artifact is fully static and runs on any distribution regardless of host glibc version. The musl libc ships its own allocator, which is simple and small but noticeably slower than glibc's under allocator contention. On the v0.4.6 release (musl + default allocator) a bench run over 500 iterations of the 78-event demo dataset measured 1.08M events/sec on aarch64 Linux, against 1.47M for an `aarch64-unknown-linux-gnu` build of the same code. That is well above the documented 100k events/sec target, but also the sole real cost of choosing musl over glibc.

Rather than resurrect a dual glibc/musl release matrix to recover the gap, the CLI crate declares `mimalloc` as a target-gated dependency:

```toml
[target.'cfg(target_env = "musl")'.dependencies]
mimalloc = "0.1.49"
```

and swaps the global allocator in `main.rs` behind the same cfg:

```rust
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
```

Consequences:

- **On musl targets** (Linux release artifacts): mimalloc replaces the system allocator automatically at link time. The v0.4.7 bench (same 500 x 78 workload, aarch64 Linux) measures **2.00M events/sec**, against 1.54M for the glibc build of the same code. mimalloc not only closes the musl gap but surpasses the glibc baseline by roughly 30%, driven by mimalloc's segment-and-page layout that outperforms both ptmalloc2 (glibc) and musl's naive allocator on the small-to-medium allocations that dominate perf-sentinel's hot path.
- **On macOS, Windows, and any future `*-linux-gnu` target**: the `cfg(target_env = "musl")` guard evaluates to false, `mimalloc` is not even compiled, the system allocator stays in place. No surface-area change for those platforms.
- **RSS cost**: about +21% (measured 42 MB vs 34 MB on the same bench). Expected tradeoff for a faster allocator that preallocates arenas; still an order of magnitude below the documented 200 MB daemon ceiling and well within the K8s requests/limits range recommended in the Helm values.

The feature-flag-less, target-gated form was chosen over an opt-in cargo feature because (1) there is no plausible musl-build reason to keep the slower default, and (2) the swap has zero user-visible surface, so exposing it as a toggle would add documentation burden without a corresponding benefit.

## Distribution strategy

1. **GitHub Releases** (primary): cross-platform binaries for 4 targets (linux/amd64, linux/arm64, macOS/arm64, windows/amd64) with SHA256 checksums. macOS Intel users can run the arm64 binary via Rosetta 2
2. **`cargo install perf-sentinel`** via crates.io
3. **Docker** (`FROM scratch`, `USER 65534`): minimal image for Kubernetes deployments

GitHub Actions are pinned to commit SHAs for supply-chain security. The `cross` tool used for ARM cross-compilation is pinned to a specific version (`--version 0.2.5`) to prevent unexpected behavior from upstream releases. The release workflow generates SHA256 checksums for all binaries.

## Diff subcommand

`perf-sentinel diff --before <traces-old.json> --after <traces-new.json> [--config foo.toml] [--format text|json|sarif] [--output file]`

Compares two trace sets and emits a delta report. Primary use case: PR CI integration that surfaces regressions and improvements introduced by a change. The handler runs `pipeline::analyze` on each trace file with the **same** `Config`, then calls `diff::diff_runs(&before_report, &after_report)`.

### Identity tuple

Findings are matched across runs by the tuple `(finding_type, service, source_endpoint, pattern.template)`. Templates are normalized at detection time so direct equality suffices, no re-normalization at diff time. When the same identity tuple appears multiple times in one run (e.g. an N+1 template firing on different traces), the diff engine collapses the duplicates by keeping the worst-severity one. This avoids treating a count difference for the same template as a severity change.

### Output sections

The `DiffReport` carries four lists:

- `new_findings`: identity tuples present in `after` but absent from `before`.
- `resolved_findings`: present in `before` but absent from `after`.
- `severity_changes`: same identity in both runs, different severity. Sorted regressions first.
- `endpoint_metric_deltas`: per-endpoint I/O op count deltas, sorted by `delta` descending (regressions first). Sourced from `green_summary.per_endpoint_io_ops`, which the pipeline always populates regardless of `[green] enabled`.

### Output formats

- **text** (default): summary header followed by four sections, color-coded (red for regressions, green for improvements). Designed for terminal review.
- **json**: full `DiffReport` serialized via `serde_json::to_writer_pretty`. The stable JSON shape mirrors the diff module's struct layout.
- **sarif**: only the `new_findings` are emitted as SARIF results, since "resolved" and "severity changed" have no native SARIF concept. Suitable for PR-annotation pipelines (GitHub Code Scanning, GitLab Code Quality) that only need to surface regressions.

### No `--ci` flag

The `analyze --ci` quality gate is intentionally not duplicated on `diff`: the diff itself is the signal. A non-empty `new_findings` list, a regression in `severity_changes` or a positive `endpoint_metric_deltas` entry are all signals the CI consumer can decide to fail on, depending on its policy.
