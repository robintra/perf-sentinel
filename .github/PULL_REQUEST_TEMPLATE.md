## Summary

<!-- One or two sentences describing the change and its motivation. -->

## Changes

<!-- Bullet list of the concrete changes in this PR. -->

-
-

## Checklist

Cargo:

- [ ] `cargo build --workspace` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] Default-features build AND `--no-default-features` build both pass when touching `daemon/`, `ingest/otlp.rs`, `ingest/tempo.rs`, `ingest/jaeger_query.rs` or any `#[cfg(feature = "...")]` code

Documentation and assets:

- [ ] Public-facing docs updated (`README.md`, `README-FR.md`, relevant files under `docs/` and `docs/FR/`)
- [ ] Captures regenerated when CLI output, TUI, or HTML dashboard changed (`vhs tapes/<x>.tape`, `npm run demo` in `crates/sentinel-cli/tests/browser/`, see [CONTRIBUTING.md / Documentation assets](../CONTRIBUTING.md#documentation-assets))
- [ ] `CHANGELOG.md` entry added under `[Unreleased]`

Versioning (release PRs only):

- [ ] `workspace.package.version` bumped in root `Cargo.toml`
- [ ] Intra-workspace `perf-sentinel-core = { version = "x.y.z", path = ... }` pin synced in `crates/sentinel-cli/Cargo.toml`
- [ ] `PERF_SENTINEL_VERSION` synced in `docs/ci-templates/` and the snippets in `docs/CI.md` / `docs/FR/CI-FR.md`
- [ ] `charts/perf-sentinel/Chart.yaml` (Helm chart) bumped in lockstep
- [ ] `./scripts/check-tag-version.sh vX.Y.Z` passes locally

## Related issues

<!-- Closes #X, refs #Y, or "n/a". -->
