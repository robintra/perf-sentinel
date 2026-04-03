# Pipeline architecture and type system

## Why a linear pipeline

perf-sentinel processes I/O traces through a sequence of transformations: `event -> normalize -> correlate -> detect -> score -> report`. This is a **linear pipeline**, not a hexagonal (ports-and-adapters) architecture.

The rationale is straightforward: the data flows in one direction. Events enter, get transformed at each stage, and produce a report. There are no bidirectional dependencies, no domain events, no complex interaction patterns. A hexagonal architecture would introduce trait indirection between every stage: adding cognitive overhead, compile-time cost, and dynamic dispatch for zero benefit.

Traits are used only at the **borders** of the pipeline:
- **Input:** `IngestSource` trait (JSON, OTLP)
- **Output:** `ReportSink` trait (JSON file, stdout)

Between the borders, every stage is a **pure function**: it takes data in and returns transformed data out. No side effects, no state, no trait objects. This makes each stage independently testable without mocks: just construct input data and assert on the output.

This pattern is common in data-processing tools in the Rust ecosystem. Projects like [ripgrep](https://github.com/BurntSushi/ripgrep) and [bat](https://github.com/sharkdp/bat) follow similar "pipeline of transformations" architectures.

## The type chain

Each pipeline stage produces a distinct type:

```
SpanEvent  ->  NormalizedEvent  ->  Trace  ->  Finding  ->  Report
 (event.rs)   (normalize/mod.rs) (correlate/) (detect/)  (report/mod.rs)
```

**Why distinct types instead of mutating in place?** Each stage adds information (normalization adds `template` + `params`, correlation groups by `trace_id`, detection produces findings). Making this explicit in the type system means the compiler enforces that no stage can use data from a future stage. A `NormalizedEvent` is guaranteed to have a `template` field: a raw `SpanEvent` is not.

**Ownership transfer:** `normalize_all()` takes `Vec<SpanEvent>` by value (moved, not borrowed). This is deliberate:
- The caller doesn't need the raw events after normalization
- Avoids lifetime annotations that would propagate through every stage
- Enables the normalizer to move fields (`SpanEvent` is consumed into `NormalizedEvent.event`)
- Zero-cost: the `SpanEvent` is moved into `NormalizedEvent`, not cloned

## Deterministic output

Detection uses `HashMap` internally for grouping. Rust's `HashMap` uses a [randomized hasher](https://doc.rust-lang.org/std/collections/struct.HashMap.html) (SipHash by default), so iteration order varies between runs. Without sorting, the same input could produce findings in different orders across runs.

The shared `detect::sort_findings()` function sorts findings after scoring with a multi-level key:

```rust
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.finding_type.cmp(&b.finding_type)
            .then_with(|| a.severity.cmp(&b.severity))
            .then_with(|| a.trace_id.cmp(&b.trace_id))
            .then_with(|| a.source_endpoint.cmp(&b.source_endpoint))
            .then_with(|| a.pattern.template.cmp(&b.pattern.template))
    });
}
```

This function is defined in `detect/mod.rs` and reused by `pipeline::analyze()` and `cmd_inspect` to guarantee consistent ordering everywhere. It requires `FindingType` and `Severity` to implement `Ord`. The derived `Ord` uses variant declaration order, giving a stable sort: `NPlusOneSql < NPlusOneHttp < RedundantSql < ... < SlowHttp < ExcessiveFanout`.

Top offenders are similarly sorted (IIS descending, alphabetical tiebreaker) to ensure the same report for the same input.

## Workspace split

The project is split into two crates:
- **sentinel-core**: library crate containing all pipeline logic
- **sentinel-cli**: binary crate providing the CLI entry point

**Why split?** The core library can be embedded by other Rust projects (e.g., a custom test harness that calls `pipeline::analyze` directly). The CLI is intentionally thin: it parses arguments with [clap](https://docs.rs/clap/), loads config, and delegates to sentinel-core functions. All business logic lives in the library.

The dependency direction is one-way: `sentinel-cli` depends on `sentinel-core`, never the reverse.

## Quality gate as a separate stage

The quality gate (`quality_gate::evaluate`) is a distinct stage called after scoring, not baked into detection or reporting. This separation allows:
- Detection to find **all** issues regardless of thresholds
- Scoring to compute **all** metrics regardless of pass/fail
- The quality gate to make a binary pass/fail decision based on **configurable rules**

The three rules (max critical SQL N+1, max warning+ HTTP N+1, max waste ratio) are evaluated independently. The gate passes only if all rules pass. This is more flexible than a single severity threshold.

## Report structure

The `Report` struct combines four sections:

```rust
pub struct Report {
    pub analysis: Analysis,        // duration_ms, events_processed, traces_analyzed
    pub findings: Vec<Finding>,    // sorted, enriched with green_impact
    pub green_summary: GreenSummary, // IIS, waste ratio, top offenders, CO2
    pub quality_gate: QualityGate,  // passed + individual rule results
}
```

**Why a single struct?** JSON serialization with `serde_json::to_writer_pretty` produces the complete report in one call. Consumers (CI scripts, dashboards) parse one JSON object, not multiple files. The `#[serde(skip_serializing_if = "Option::is_none")]` annotation on optional fields (CO2 values) keeps the JSON clean when those features are not configured.

## Crate-level clippy configuration

`sentinel-core/src/lib.rs` enables `clippy::pedantic` globally:

```rust
#![warn(clippy::pedantic)]
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 for elapsed_ms
#![allow(clippy::cast_precision_loss)]      // usize -> f64 for ratios
#![allow(clippy::similar_names)]            // min_ts/min_ms, max_ts/max_ms are clear
```

The three exceptions are documented with their justification. Every other `#[allow]` in the codebase has an inline comment explaining why.

## Error handling

The project uses typed errors throughout:
- `ConfigError`: config parsing and validation failures
- `DaemonError`: address parsing and listener binding failures
- `JsonIngestError`: payload size and JSON parse failures
- `JsonReportError`: stdout write failures

All error types use [thiserror](https://docs.rs/thiserror/) for `Display` and `Error` trait derivation. There are no `Box<dyn Error>` or `.unwrap()` calls in library production code. The few `.expect()` calls (Prometheus metric registration, `NonZeroUsize` creation) are in infallible paths guarded by upstream validation, and are documented with `# Panics` doc comments.
