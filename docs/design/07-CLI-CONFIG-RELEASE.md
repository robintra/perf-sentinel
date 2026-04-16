# CLI, configuration and release profile

## CLI design

The CLI (`sentinel-cli`) is intentionally thin. It parses arguments with [clap](https://docs.rs/clap/) and delegates to `sentinel-core` functions. Eight subcommands are available: `analyze`, `explain`, `watch`, `demo`, `bench`, `pg-stat`, `inspect` and `query`.

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

### PgStat: pg_stat_statements hotspot analysis

`perf-sentinel pg-stat --input FILE` parses PostgreSQL `pg_stat_statements` exports (CSV or JSON, auto-detected) and produces hotspot rankings by total execution time, call count and mean execution time. The `--traces` flag enables cross-referencing with trace-based findings: the tool runs `pipeline::analyze()` on the trace file and marks `pg_stat_statements` entries whose normalized template also appears in trace findings.

This subcommand is intentionally separate from `analyze` because `pg_stat_statements` data has no `trace_id`, it cannot participate in the trace correlation pipeline. It is a complementary analysis mode.

### Inspect: interactive TUI

`perf-sentinel inspect --input FILE` launches a terminal UI built with [ratatui](https://ratatui.rs/) and [crossterm](https://docs.rs/crossterm/). These dependencies live in `sentinel-cli/Cargo.toml` only (not `sentinel-core`) because TUI is a presentation concern.

**Layout:** 3-panel split, traces list (top-left, 30%), findings for selected trace (top-right, 70%), finding detail with span tree (bottom, 50%). The detail panel reuses `explain::build_tree()` and `explain::format_tree_text()` for the span tree display.

**State management:** the `App` struct holds pre-computed `findings_by_trace` (indexed at construction time) to avoid recomputing on every frame. Navigation state (selected_trace, selected_finding, active_panel, scroll_offset) is updated by key events.

**Data loading:** events are ingested once, then cloned. One copy for `correlate()` (needed for tree building) and one for `pipeline::analyze()` (consumed by the pipeline). This avoids re-reading the file.

### Feature flags

The workspace uses Cargo feature flags to keep daemon-only dependencies optional:

| Feature  | Crate           | What it gates                                                                                                                            |
|----------|-----------------|------------------------------------------------------------------------------------------------------------------------------------------|
| `daemon` | `sentinel-core` | `hyper`, `hyper-util`, `http-body-util`, `bytes`, `arc-swap`. Enables `daemon.rs`, Scaphandre scraper/state, cloud energy scraper/state. |
| `daemon` | `sentinel-cli`  | Forwards to `sentinel-core/daemon`. Enables the `watch` subcommand.                                                                      |
| `tui`    | `sentinel-cli`  | `ratatui`, `crossterm`. Enables the `inspect` subcommand.                                                                                |

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

| Sub-action | API endpoint | Output | Description |
|---|---|---|---|
| `findings` | `/api/findings` | colored text (default) or JSON | List recent findings with `--service`, `--finding-type`, `--severity`, `--limit` filters |
| `explain` | `/api/explain/{trace_id}` | colored tree (default) or JSON | Show the explain tree for a trace from daemon memory |
| `inspect` | `/api/findings` | ratatui TUI | Interactive 3-panel TUI fed from live daemon data |
| `correlations` | `/api/correlations` | colored table (default) or JSON | Show active cross-trace correlations |
| `status` | `/api/status` | colored summary (default) or JSON | Show daemon health (version, uptime, active traces, stored findings count) |

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

### Dual format: sectioned + flat

The config supports two formats for backward compatibility:

**Sectioned (recommended):**
```toml
[detection]
n_plus_one_min_occurrences = 5
```

**Legacy flat:**
```toml
n_plus_one_threshold = 5
```

**Priority:** section value > flat value > default. This is implemented with `Option<T>` fields in the raw section structs:

```rust
struct DetectionSection {
    n_plus_one_min_occurrences: Option<u32>,
    // ...
}
```

`serde(default)` produces `None` for absent fields. The `From<RawConfig> for Config` conversion uses `.or()` chains:

```rust
n_plus_one_threshold: raw.detection.n_plus_one_min_occurrences
    .or(raw.n_plus_one_threshold)
    .unwrap_or(defaults.n_plus_one_threshold),
```

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

## Distribution strategy

1. **GitHub Releases** (primary): cross-platform binaries for 4 targets (linux/amd64, linux/arm64, macOS/arm64, windows/amd64) with SHA256 checksums. macOS Intel users can run the arm64 binary via Rosetta 2
2. **`cargo install sentinel-cli`** via crates.io
3. **Docker** (`FROM scratch`, `USER 65534`): minimal image for Kubernetes deployments

GitHub Actions are pinned to commit SHAs for supply-chain security. The `cross` tool used for ARM cross-compilation is pinned to a specific version (`--version 0.2.5`) to prevent unexpected behavior from upstream releases. The release workflow generates SHA256 checksums for all binaries.
