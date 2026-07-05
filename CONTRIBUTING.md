# Contributing to perf-sentinel

Thank you for your interest in contributing to perf-sentinel! This document covers the development setup, coding conventions and how to submit changes.

## Prerequisites

- **Rust** stable toolchain pinned in `rust-toolchain.toml` (currently 1.96.1, edition 2024)
- **cargo** (comes with Rust)
- Optional: `cargo-llvm-cov` for code coverage

## Development setup

```bash
# Clone the repository
git clone https://github.com/robintra/perf-sentinel.git
cd perf-sentinel

# Build the workspace
cargo build --workspace

# Run all tests
cargo test --workspace

# Run with clippy (must pass with zero warnings)
cargo clippy --workspace --all-targets -- -D warnings

# Check formatting
cargo fmt --all -- --check
```

### Windows notes

`.cargo/config.toml` injects `/STACK:8388608` on windows-msvc because the debug `#[tokio::main]` future overflows the default 1 MiB stack. Leave it in place. Some tests and example binaries have known, pre-existing failures on Windows unrelated to any given change. `cargo check --workspace` is the fast correctness signal there.

### Git hooks

The pre-commit hook runs two checks: gitleaks on staged content for secrets, and `cargo clippy --workspace --features daemon -- -D warnings` when at least one staged file is a `*.rs`. Install once after cloning:

```bash
bash scripts/install-hooks.sh
```

The gitleaks check requires version 8.16+ for the `git --staged` subcommand. Both checks skip silently if their tooling is not on PATH (gitleaks not installed, cargo not available), so a fresh checkout never fails `git commit` with a confusing missing-binary error.

Bypass matrix:

| Situation                                   | Command                                         |
|---------------------------------------------|-------------------------------------------------|
| Normal commit                               | `git commit ...` (runs both checks)             |
| WIP commit, clippy noisy on transient state | `SKIP_CLIPPY=1 git commit ...` (keeps gitleaks) |
| Emergency, all checks off                   | `git commit --no-verify` (use sparingly)        |

The clippy check on a Rust-touching commit costs ~5s on a warm cache for modern CPUs, up to ~20s on memory-constrained or slow runners. Commits that only touch docs, CI, or config files skip the clippy step entirely, so the only contributors paying the cost are the ones editing Rust.

If you have set `core.hooksPath` globally to a custom directory (some `dotfiles` setups do), `install-hooks.sh` aborts with instructions. Either unset it for this repo (`git config --local --unset core.hooksPath`) or chain the invocation from `scripts/hooks/pre-commit` into your existing global hook.

CI also runs gitleaks and clippy on every push (`.github/workflows/ci.yml`), the local hook only catches issues earlier. SonarCloud cloud scan handles cognitive-complexity at threshold 15 on every PR and its quality gate blocks the pipeline (`sonar.qualitygate.wait=true`), the local clippy gate is at threshold 60 by design (see `clippy.toml` for the rationale).

## Code coverage

```bash
# Install cargo-llvm-cov (once)
cargo install cargo-llvm-cov

# Generate HTML coverage report
cargo llvm-cov --workspace --html --open
```

## Project structure

perf-sentinel is a Cargo workspace with two crates:

- **sentinel-core** (`crates/sentinel-core/`): library containing all pipeline logic
- **sentinel-cli** (`crates/sentinel-cli/`): binary providing the CLI

The pipeline architecture is: `event -> normalize -> correlate -> detect -> score -> report`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

## Where to start

Whatever your Rust level, start by running the product, not by reading code:

```bash
cargo run -p perf-sentinel -- demo
cargo run -p perf-sentinel -- analyze --input tests/fixtures/n_plus_one_sql.json
```

Then read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) followed by [docs/design/01-PIPELINE-AND-TYPES.md](docs/design/01-PIPELINE-AND-TYPES.md). That chapter explains the core decision (a pipeline of pure functions, no hexagonal architecture) that shapes every contribution. The [design doc index](docs/design/00-INDEX.md) maps each source file to the chapter explaining its rationale.

### New to Rust

The batch pipeline is the approachable half of the codebase: plain ownership, no async, no elaborate trait bounds. A good reading path is `pipeline.rs` (short, wires the stages together), then one detector such as `detect/slow.rs` or `detect/redundant.rs`. Each detector is a small pure function from `Trace` to `Vec<Finding>` with its tests next to the code, so one file gives you a complete pipeline stage. [docs/design/04-DETECTION.md](docs/design/04-DETECTION.md) explains each algorithm.

Leave `daemon/` (async, backpressure), `tui/` (mutable TUI state) and `score/carbon*` (carbon methodology) for later.

Good first contributions: a new fixture plus a test case for an existing detector, or a polyglot N+1 fixture exercising the language-aware `SuggestedFix` table. Both are isolated, well-tested extension points (see the fixtures section below).

### Experienced with Rust

The parts that reward expertise: the hand-written SQL tokenizer (`normalize/sql.rs`, design doc 02), the streaming correlator (`correlate/window.rs`, LRU + TTL ring buffer with a memory budget, doc 03), and the daemon (`daemon/`, tokio, OTLP gRPC + HTTP, sampling, security hardening, doc 06).

Read the conventions below before your first PR: this project actively refuses speculative abstraction (three duplicated lines beat a premature helper, traits only at the pipeline borders), and the CI gates are strict (`clippy -D warnings`, `cognitive_complexity` denied, bounded Prometheus label sets, no outbound network calls). A PR that introduces an unneeded trait or an unbounded label value will be rejected regardless of code quality.

## Coding conventions

### Language

- All code, comments, doc comments, error messages and CLI output must be in **English**.
- French is used only in `README-FR.md` and the `docs/FR/` mirror.

### Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) format, entirely in **English**:

```
feat: add slow query detection for HTTP calls
fix: correct window computation for cross-midnight traces
docs: add CONFIGURATION.md reference
test: add e2e test for redundant SQL detection
refactor: extract normalization into separate module
```

Keep the message to the subject line: no body, no `Co-Authored-By` or other trailers.

### Architecture

- **Pipeline stages, not hexagonal architecture.** Each stage is a pure function that takes data and returns data.
- **Traits only at borders:** `IngestSource` for input, `ReportSink` for output. No trait abstractions between pipeline stages.
- **No unnecessary abstractions.** If three similar lines of code work, prefer them over a premature abstraction.

### Dependencies

- Do **not** use the `sqlparser` crate, use the existing homemade tokenizer.
- Do **not** vendor `.proto` files, use the `opentelemetry-proto` crate.
- Do **not** make outbound network calls, all data (including carbon intensity tables) must be embedded.

### Prometheus metrics

- Label values must always come from a **bounded, compile-time-known set** (enum variants, not user-controlled strings). This prevents label cardinality explosions that could crash the metrics endpoint.

## Test strategy

### Unit tests

Each module has its own unit tests in a `#[cfg(test)] mod tests` block. Large modules keep that block in a sibling `tests.rs` file (folder module + `#[cfg(test)] mod tests;` declaration, e.g. `config/`, `score/`, `ingest/otlp/`, `report/html/`); follow that pattern when a test module grows past a few hundred lines. Tests should cover:

- Happy path with representative input
- Edge cases (empty input, boundary values, malformed data)
- Regression cases for bugs found in the past

### End-to-end tests

Integration tests live in:

- `crates/sentinel-core/tests/e2e.rs`: tests the full pipeline with JSON fixtures
- `crates/sentinel-cli/tests/e2e/`: tests the CLI binary behavior (one test target: `helpers.rs` plus one module per subcommand topic)
- `crates/sentinel-cli/tests/`: three additional flat targets, `cli_ack.rs`, `hash_bake_e2e.rs` and `verify_hash_e2e.rs`

### Fixtures

Test fixtures live in `tests/fixtures/`. Group by purpose:

- **Per-detector**: `n_plus_one_sql.json`, `n_plus_one_http.json`, `slow_queries.json`, `fanout.json`, `mixed.json`, `clean_traces.json`.
- **Polyglot N+1 SQL** (one per ORM, used by the language-aware `SuggestedFix` tests): `n_plus_one_sql_java_jpa.json`, `n_plus_one_sql_java_quarkus.json`, `n_plus_one_sql_java_mutiny_reactive.json`, `n_plus_one_sql_csharp_ef_core.json`, `n_plus_one_sql_rust_diesel.json`, `n_plus_one_sql_php_laravel.json`.
- **End-to-end demo**: `demo.json` (10 findings across all detectors, drives `tapes/demo.tape`), `report_realistic.json` (5 findings, drives `crates/sentinel-cli/tests/browser/` Playwright stills), `report_minimal.json`, `report_three_estimation_states.json`.
- **External formats**: `jaeger_export.json`, `zipkin_export.json`, `otlp_export.json`, `otlp_export.ndjson` (all auto-detected by `analyze --input`).
- **Diff baseline**: `baseline_report.json` (consumed by `analyze --before`).
- **`pg_stat`**: `pg_stat_statements.csv`, `pg_stat_statements.json`.
- **`mysql_stat`**: `mysql_perf_schema.csv`, `mysql_perf_schema.json` (consumed by `perf-sentinel mysql-stat`).
- **Carbon calibration**: `demo-energy.csv` (consumed by `perf-sentinel calibrate`).

The CLI `demo` subcommand bundles its own dataset, embedded at `crates/sentinel-cli/src/demo_data.json`.

## Documentation assets

Some changes require regenerating committed image assets so the README, the docs and the dashboard stills stay in sync with the code. The pipelines are scripted, no manual screen-recording is needed.

### Terminal and TUI (VHS)

Terminal-side GIFs and PNGs under `docs/img/{analyze,inspect,explain,calibrate,disclose,monitor,pg-stat,ack,...}/` are generated by [VHS](https://github.com/charmbracelet/vhs) tapes living in `tapes/*.tape`. Each tape carries a header comment explaining what it captures, the source fixture, and the ffmpeg post-processing (if any). Install once (macOS shown, see the [VHS install docs](https://github.com/charmbracelet/vhs#installation) for other platforms):

```bash
brew install vhs ffmpeg
```

Run a tape from the repo root after building the release binary:

```bash
cargo build --release
vhs tapes/demo.tape                # analyze GIF + four still frames
vhs tapes/demo-ack-cli.tape        # ack subcommand (create/list/revoke)
python3 scripts/trim-bottom-png.py docs/img/<dir>/*.png  # crop empty bottoms
```

Regenerate the relevant tape(s) when you change:
- The colored CLI output of any subcommand (`render.rs`, `score/`, `quality_gate.rs`, `acknowledgments.rs`).
- The TUI layout, key bindings, or panel rendering (`tui/`).
- The `inspect`, `explain`, `calibrate`, `disclose`, `pg-stat` or `query monitor` subcommands' user-facing surface.

### HTML dashboard (Playwright)

The dashboard tour GIFs and per-tab PNGs under `docs/img/report/` are generated by a Playwright suite at `crates/sentinel-cli/tests/browser/`:

```bash
cd crates/sentinel-cli/tests/browser
npm install                # once
npm run demo               # produces 2 GIFs + 14 PNGs (light + dark pairs)
```

The fixture `dashboard-demo.html` is regenerated by `global-setup.ts`, which also patches `window.fetch` so the live-mode UI (Ack/Revoke buttons, Acknowledgments panel) renders without a real daemon. Mock data lives in `injectDemoAckMock`. Regenerate when you change:
- The HTML template (`crates/sentinel-core/src/report/html/html_template.html`).
- The dashboard JS bundle, CSS, or per-tab layout.
- The set of tabs or the live-mode endpoints (`/api/status`, `/api/acks`, `/api/findings/{sig}/ack`).

When changing the underlying fixture (`tests/fixtures/report_realistic.json`), the Playwright stills update on the next `npm run demo` run, but check `demo/tour.spec.ts` and `demo/stills.spec.ts` for selectors that depend on specific finding signatures (e.g. the mock acks in `global-setup.ts` are pinned to real signatures). The regression suite itself lives in `tests/dashboard.spec.ts`.

### Architecture diagrams

Pipeline diagrams under `docs/diagrams/svg/` are committed SVGs hand-edited or exported from a draw tool. There is no automated regeneration pipeline — update them manually when the pipeline architecture changes.

### Running tests

```bash
# All tests
cargo test --workspace

# Tests for a specific crate (package names: perf-sentinel-core, perf-sentinel)
cargo test -p perf-sentinel-core

# A specific test
cargo test -p perf-sentinel-core -- detect::slow::tests::test_slow_sql
```

## Submitting changes

1. Fork the repository and create a feature branch from `main`.
2. Make your changes, ensuring all tests pass and clippy is clean.
3. Write or update tests for any new functionality.
4. Submit a pull request with a clear description of the change and its motivation.

### Pull request checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] New functionality has tests
- [ ] Commit messages follow conventional commits format in English

## Release process

Releases fire on a `v*` git tag push. The `.github/workflows/release.yml` workflow builds binaries for four targets, generates SHA256 checksums, publishes both crates to crates.io, and pushes a multi-arch Docker image to GHCR and Docker Hub.

Before tagging, bump `workspace.package.version` in the root `Cargo.toml` and run the local pre-flight script:

```bash
# Bump the workspace version (manually or via cargo-edit)
# cargo set-version --workspace 0.5.4

# Verify the tag you're about to push matches every Cargo.toml
./scripts/check-tag-version.sh v0.5.4

# Tag and push
git tag v0.5.4
git push origin v0.5.4
```

The same check runs as the first job of the release workflow (`check-versions`). If the tag and any `Cargo.toml` in the workspace disagree, the workflow aborts before any artifact is built or published, saving you from deleting a broken release post-hoc.

Bump targets beyond `Cargo.toml`:
- `PERF_SENTINEL_VERSION` in the CI templates under `docs/ci-templates/` and their referenced examples in `docs/CI.md` and `docs/FR/CI-FR.md`.
- `CHANGELOG.md`: add a new `[x.y.z]` section at the top.
- `CLAUDE.md`: update the "Version" status line after the tag is pushed.
- `charts/perf-sentinel/Chart.yaml`: bump `version` and `appVersion` in lockstep. The chart itself is published by a separate `chart-v*` tag (`.github/workflows/helm-release.yml`, guarded by `scripts/check-helm-tag-version.sh`).
- Intra-workspace dependency pins: any `[dependencies]` block that pins a sibling workspace crate by literal version (e.g. `perf-sentinel-core = { version = "0.5.24", path = "../sentinel-core" }` in `crates/sentinel-cli/Cargo.toml`) must be bumped in lockstep. `scripts/check-tag-version.sh` validates these pins and aborts the release on mismatch. List them with:

  ```bash
  grep -rn 'perf-sentinel-core[[:space:]]*=[[:space:]]*{' crates/*/Cargo.toml
  ```

## License

By contributing, you agree that your contributions will be licensed under the [AGPL-3.0-only](LICENSE) license.
