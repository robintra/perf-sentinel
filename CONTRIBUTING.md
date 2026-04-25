# Contributing to perf-sentinel

Thank you for your interest in contributing to perf-sentinel! This document covers the development setup, coding conventions and how to submit changes.

## Prerequisites

- **Rust 1.95.0+** stable toolchain (edition 2024)
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

## Coding conventions

### Language

- All code, comments, doc comments, error messages and CLI output must be in **English**.
- French is used only in `README-FR.md`.

### Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) format, entirely in **English**:

```
feat: add slow query detection for HTTP calls
fix: correct window computation for cross-midnight traces
docs: add CONFIGURATION.md reference
test: add e2e test for redundant SQL detection
refactor: extract normalization into separate module
```

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

Each module has its own unit tests in a `#[cfg(test)] mod tests` block. Tests should cover:

- Happy path with representative input
- Edge cases (empty input, boundary values, malformed data)
- Regression cases for bugs found in the past

### End-to-end tests

Integration tests live in:

- `crates/sentinel-core/tests/e2e.rs`: tests the full pipeline with JSON fixtures
- `crates/sentinel-cli/tests/e2e.rs`: tests the CLI binary behavior

### Fixtures

Test fixtures are JSON files in `tests/fixtures/`:

- `n_plus_one_sql.json`: N+1 SQL query pattern
- `n_plus_one_http.json`: N+1 HTTP call pattern
- `clean_traces.json`: traces with no anti-patterns
- `mixed.json`: multiple pattern types in one file
- `slow_queries.json`: slow SQL and HTTP operations
- `fanout.json`: excessive fanout pattern
- `jaeger_export.json`: Jaeger JSON export format
- `zipkin_export.json`: Zipkin JSON v2 format

The demo dataset is embedded at `crates/sentinel-cli/src/demo_data.json`.

### Running tests

```bash
# All tests
cargo test --workspace

# Tests for a specific crate
cargo test -p sentinel-core

# A specific test
cargo test -p sentinel-core -- detect::slow::tests::test_slow_sql
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
# cargo set-version --workspace 0.5.3

# Verify the tag you're about to push matches every Cargo.toml
./scripts/check-tag-version.sh v0.5.3

# Tag and push
git tag v0.5.3
git push origin v0.5.3
```

The same check runs as the first job of the release workflow (`check-versions`). If the tag and any `Cargo.toml` in the workspace disagree, the workflow aborts before any artifact is built or published, saving you from deleting a broken release post-hoc.

Bump targets beyond `Cargo.toml`:
- `PERF_SENTINEL_VERSION` in the three CI templates under `docs/ci-templates/` and their referenced examples in `docs/CI.md` and `docs/FR/CI-FR.md`.
- `CHANGELOG.md`: move the `[Unreleased]` section content under a new `[x.y.z]` header.
- `CLAUDE.md`: update the "Version" status line after the tag is pushed.

## License

By contributing, you agree that your contributions will be licensed under the [AGPL-3.0-only](LICENSE) license.
