# Public-disclosure schema examples

This directory contains two illustrative outputs of `perf-sentinel disclose`:

- `example-internal-G1.json` — gate G1 (internal disclosure, audience: own organization).
- `example-official-public-G2.json` — gate G2 (official public disclosure, audience: world).

Both files are checked into the repository and serve as living references for the schema documented in [`docs/SCHEMA.md`](../../SCHEMA.md). They are also surfaced from the public reporting docs (`docs/REPORTING.md`).

## Version fields convention

The examples carry version information in two unrelated registers, and they bump on different cadences:

- `binary_verification_url` (one occurrence per file) **always points to the latest released perf-sentinel version**. It tells a reader "here is where you can grab the current binary to verify these signatures yourself." It is the only field bumped by the release procedure (`docs/RELEASE-PROCEDURE.md` step 2).
- `perf_sentinel_version`, `binary_version`, `binary_versions` (multiple occurrences) **are frozen at the example's historical baseline** (currently `0.7.0`, with one narrative entry referencing `0.6.2` as a prior version). They describe a snapshot of what a disclosure report looked like at that point in time. They are not bumped at every release.

This asymmetry is deliberate. The point of the examples is to show what a real disclosure looks like (frozen narrative) while still letting a reader fetch the current binary (live URL). Bumping the narrative fields at every release would make the examples drift away from the actual schema versions a real user would have produced.

If you ever refactor these examples (for example, to demonstrate a new schema feature introduced in a later version), make a deliberate choice:

- Re-baseline the example to the new version: bump every version field together and rewrite any narrative that references prior versions.
- Or freeze a new historical snapshot: pick the version that best illustrates the feature and keep all narrative fields consistent with that vintage.

Do not partially bump some narrative fields and leave others stale.

## Tooling

These files are loaded by the workspace tests under `cargo test --workspace`. Notably:

- `crates/sentinel-core/tests/periodic_examples.rs` round-trips both files through the schema deserializer.
- `crates/sentinel-cli/tests/hash_bake_e2e.rs` and `verify_hash_e2e.rs` use the G2 example as input for the `hash-bake` and `verify-hash` subcommands.

If you edit these files (including the `binary_verification_url` bump at release time), confirm `cargo test --workspace` still passes before committing.
