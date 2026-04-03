# CLI, configuration, and release profile

## CLI design

The CLI (`sentinel-cli`) is intentionally thin. It parses arguments with [clap](https://docs.rs/clap/) and delegates to `sentinel-core` functions. Seven subcommands are available: `analyze`, `explain`, `watch`, `demo`, `bench`, `pg-stat`, and `inspect`.

### Analyze: colored report by default, JSON with `--ci`

`perf-sentinel analyze` displays a colored terminal report when run interactively (without `--ci`). This is the output humans see when running the tool locally. With `--ci`, the output switches to structured JSON for machine consumption, and the process exits with code 1 if the quality gate fails.

This follows the convention of tools like `cargo test` (colored output by default, `--format json` for CI).

The `--format` flag provides explicit control over the output format: `text` (colored terminal, default), `json` (structured report), or `sarif` (SARIF v2.1.0 for code scanning). When `--ci` is used without `--format`, the output defaults to JSON for backward compatibility.

### Explain: per-trace tree view

`perf-sentinel explain --input FILE --trace-id ID` builds a tree from `parent_span_id` relationships and annotates findings inline. It runs per-trace detectors only (N+1, redundant, slow, fanout); cross-trace percentile findings are not included.

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
let p99_idx = ((per_event_ns.len() as f64 * 0.99).ceil() as usize).min(per_event_ns.len() - 1);
```

The ceiling-based index computation follows the [nearest-rank method](https://en.wikipedia.org/wiki/Percentile#The_nearest-rank_method) for percentiles. The `.saturating_sub(1)` converts from 1-based rank to 0-based index. The `.min(len - 1)` prevents out-of-bounds access when `ceil` rounds up to `len`.

### Throughput from nanoseconds

```rust
let elapsed_nanos: u64 = durations_ns.iter().sum();
let total_seconds = elapsed_nanos as f64 / 1_000_000_000.0;
let throughput = if total_seconds > 0.0 { total_events / total_seconds } else { 0.0 };
```

Throughput is computed from nanosecond precision (not millisecond) to avoid division-by-zero when iterations complete in less than 1ms. The `total_elapsed_ms` field in the output is derived from nanoseconds for display purposes.

### RSS Measurement

Platform-specific memory measurement:

| Platform | Method                                        | Unit                    |
|----------|-----------------------------------------------|-------------------------|
| Linux    | `/proc/self/status` -> `VmRSS` line           | KB (converted to bytes) |
| macOS    | `libc::getrusage(RUSAGE_SELF)` -> `ru_maxrss` | Bytes (on macOS)        |
| Windows  | Not implemented                               | Returns `None`          |

The macOS implementation uses `unsafe` for the `libc::getrusage` FFI call. This is justified: there is no safe Rust API for this syscall, and the function is well-documented in POSIX. The return value is checked (`if ret == 0`) before using the result.

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

`perf-sentinel pg-stat --input FILE` parses PostgreSQL `pg_stat_statements` exports (CSV or JSON, auto-detected) and produces hotspot rankings by total execution time, call count, and mean execution time. The `--traces` flag enables cross-referencing with trace-based findings: the tool runs `pipeline::analyze()` on the trace file and marks `pg_stat_statements` entries whose normalized template also appears in trace findings.

This subcommand is intentionally separate from `analyze` because `pg_stat_statements` data has no `trace_id` -- it cannot participate in the trace correlation pipeline. It is a complementary analysis mode.

### Inspect: interactive TUI

`perf-sentinel inspect --input FILE` launches a terminal UI built with [ratatui](https://ratatui.rs/) and [crossterm](https://docs.rs/crossterm/). These dependencies live in `sentinel-cli/Cargo.toml` only (not `sentinel-core`) because TUI is a presentation concern.

**Layout:** 3-panel split -- traces list (top-left, 30%), findings for selected trace (top-right, 70%), finding detail with span tree (bottom, 50%). The detail panel reuses `explain::build_tree()` and `explain::format_tree_text()` for the span tree display.

**State management:** the `App` struct holds pre-computed `findings_by_trace` (indexed at construction time) to avoid recomputing on every frame. Navigation state (selected_trace, selected_finding, active_panel, scroll_offset) is updated by key events.

**Data loading:** events are ingested once, then cloned: one copy for `correlate()` (needed for tree building) and one for `pipeline::analyze()` (consumed by the pipeline). This avoids re-reading the file.

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

| Field                        | Min   | Max                  | Rationale                          |
|------------------------------|-------|----------------------|------------------------------------|
| `max_payload_size`           | 1,024 | 104,857,600 (100 MB) | Prevent disabling input protection |
| `max_active_traces`          | 1     | 1,000,000            | Prevent unbounded memory           |
| `max_events_per_trace`       | 1     | 100,000              | Prevent per-trace OOM              |
| `n_plus_one_threshold`       | 1     | *(none)*             | At least 1 occurrence to detect    |
| `window_duration_ms`         | 1     | *(none)*             | Non-zero window                    |
| `slow_query_threshold_ms`    | 1     | *(none)*             | Non-zero threshold                 |
| `slow_query_min_occurrences` | 1     | *(none)*             | At least 1 occurrence              |
| `max_fanout`                 | 1     | 100,000              | Prevent disabling detection        |
| `trace_ttl_ms`               | 100   | *(none)*             | Minimum eviction interval          |
| `sampling_rate`              | 0.0   | 1.0                  | Valid probability                  |
| `io_waste_ratio_max`         | 0.0   | 1.0                  | Valid ratio                        |

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

Maximum optimization: aggressive inlining, loop vectorization, and dead code elimination. perf-sentinel's hot path is data-processing (string matching, HashMap operations, iterator chains) that benefits from inlining. The [Cargo documentation](https://doc.rust-lang.org/cargo/reference/profiles.html) notes that the difference between `opt-level = 2` and `3` is primarily more aggressive inlining, which is exactly what a pipeline tool needs.

The alternative `opt-level = "s"` (optimize for size) was considered but rejected: the binary size difference is marginal (~200KB), while the throughput difference can reach 10-30% on data-processing workloads.

## Distribution strategy

1. **GitHub Releases** (primary): cross-platform binaries for 4 targets (linux/amd64, linux/arm64, macOS/arm64, windows/amd64) with SHA256 checksums. macOS Intel users can run the arm64 binary via Rosetta 2
2. **`cargo install sentinel-cli`** via crates.io
3. **Docker** (`FROM scratch`, `USER 65534`): minimal image for Kubernetes deployments

GitHub Actions are pinned to commit SHAs for supply-chain security. The `cross` tool used for ARM cross-compilation is pinned to a specific version (`--version 0.2.5`) to prevent unexpected behavior from upstream releases. The release workflow generates SHA256 checksums for all binaries.
