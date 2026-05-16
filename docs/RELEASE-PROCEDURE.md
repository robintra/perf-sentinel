# perf-sentinel release procedure

This document describes the end-to-end release procedure for `perf-sentinel`, applicable from 0.7.0 onwards. The procedure includes a mandatory simulation-lab validation gate that blocks tagging a version that has not been exercised end-to-end on a real k3d cluster.

The gate is intentionally pre-flight and operator-driven, not a CI job. It runs against an append-only ledger (`release-gate/lab-validations.txt`) that records every lab validation and its verdict. CI cannot reproduce a lab run, so automating the gate inside the release workflow would defeat its purpose.

## Prerequisites

- Local checkout of the `perf-sentinel-simulation-lab` repository at a recent commit on `main`.
- A working `k3d` + Docker environment for the lab (see the lab's `docs/QUICKSTART.md`).
- Push access to the `perf-sentinel` repository and a tag signing identity. The procedure uses `git tag -s`, which is GPG by default (requires `user.signingkey` configured). SSH signing works too via `git config gpg.format ssh` and a key registered as a signer.
- `gh` CLI authenticated when you need to query the GHCR REST API.

## Procedure

### 1. Open a release branch

```bash
git checkout main && git pull
git checkout -b release/X.Y.Z
```

The branch is preserved post-merge for traceability of which commits constitute the release. Do not squash on merge. Note the naming convention: the **branch** is `release/X.Y.Z` (no leading `v`), the **tag** that ships later is `vX.Y.Z` (leading `v`). `scripts/check-tag-version.sh` accepts both forms as input.

### 2. Code, tests, version bumps

Apply the feature, fix or refactor work for the release. Then bump every version reference in lockstep.

**Enforced by `scripts/check-tag-version.sh`** (CI runs this as the first job of `release.yml`, also runnable locally):

- `Cargo.toml` workspace `[workspace.package].version`
- Each `crates/*/Cargo.toml`: either `version.workspace = true` (resolves to the workspace version), or an explicit version that must match the tag. The intra-workspace pin `perf-sentinel-core = { version = "X.Y.Z", path = "..." }` in `crates/sentinel-cli/Cargo.toml` is checked here too.

**Operator-driven** (no CI gate, audit manually with `grep -RIn "<previous_version>" docs/ charts/ CHANGELOG.md`):

- `docs/ci-templates/*`: the `PERF_SENTINEL_VERSION` constant in `github-actions-baseline.yml`, `github-actions-report-cleanup.yml`, `github-actions.yml`, `gitlab-ci.yml`, and `jenkinsfile.groovy`.
- `docs/CI.md` and `docs/FR/CI-FR.md`: snippet examples that show `perf-sentinel@vX.Y.Z`.
- `docs/schemas/examples/*.json`: only the `binary_verification_url` field (which always points to the latest release). The other version fields (`perf_sentinel_version`, `binary_version`, `binary_versions`) are deliberately frozen at the example's historical baseline.
- `CHANGELOG.md`: move the `[Unreleased]` content under a new `[X.Y.Z]` heading dated today.

Run local gates:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --features daemon -- -D warnings
cargo clippy --workspace --no-default-features -- -D warnings
cargo test --workspace
scripts/check-tag-version.sh vX.Y.Z
```

The two clippy invocations cover the default feature set and the no-default-features build, since several modules are behind `#[cfg(feature = "...")]`. CI runs the same matrix.

### 2.5 GreenOps reference-data freshness check

This is an operator-driven audit, no script enforces it. Embedded reference data drives the carbon scoring pipeline and ships as Rust source (so it is exercised by `cargo test --workspace` in step 2). Test passes guarantee correctness, not freshness. Before tagging, confirm the vintages declared in:

- `crates/sentinel-core/src/score/cloud_energy/table.rs`: SPECpower / CCF coefficients, refreshed quarterly. Vintage exposed as `SPECPOWER_VINTAGE`.
- `crates/sentinel-core/src/score/carbon_profiles.rs`: ENTSO-E / EIA / AEMO / Electricity Maps hourly grid profiles, refreshed at least annually. Vintage exposed as `CARBON_PROFILES_VINTAGE`.
- `crates/sentinel-core/src/score/carbon.rs`: per-provider PUE constants (AWS, GCP, Azure, generic), refreshed when any provider publishes a new sustainability report. Vintage exposed as `PUE_VINTAGE`.

Surface all three vintages in one command:

```bash
grep -rn 'VINTAGE' crates/sentinel-core/src/score/
```

If the data window does not cover the release date with comfortable margin (typically: SPECpower table within the last 2 quarters, grid profiles within the current year), either refresh the data inside this release (also bumping the corresponding `_VINTAGE` constant) or document the deferral in `CHANGELOG.md` so the staleness is explicit to downstream users.

### 3. Bump the Helm chart in lockstep

The chart version and `appVersion` move together with the application version:

```bash
# charts/perf-sentinel/Chart.yaml
version: A.B.C        # bump on every chart change
appVersion: "X.Y.Z"   # tracks the perf-sentinel release
```

`scripts/check-chart-version-bumped.sh` runs in PR CI and rejects any chart change without a version bump and a `CHANGELOG.md` entry under `charts/perf-sentinel/`. `scripts/check-helm-tag-version.sh` validates the chart tag at release time.

### 4. Validate in the simulation lab

Push the release branch for preservation:

```bash
git push -u origin release/X.Y.Z
```

The Docker image is published to GHCR exclusively by `release.yml` on a `v*` tag push, not by `ci.yml`. Two options to get an image into the lab cluster:

- **Option A (recommended for clean validation):** build the image locally from the release branch checkout and import it into the k3d cluster:
  ```bash
  docker build -t perf-sentinel:vX.Y.Z-rc .
  k3d image import perf-sentinel:vX.Y.Z-rc -c <cluster-name>
  ```
  Then pin the lab manifests to the locally-loaded tag.
- **Option B:** push a pre-release tag (`vX.Y.Z-rc.1`) to trigger `release.yml` for a candidate image. The image becomes available on GHCR within ~10 minutes. Pin the lab manifests to the resulting digest (resolve via the GHCR REST API, see the lab's `docs/TROUBLESHOOTING.md`).

Run the lab end-to-end:

```bash
cd <path-to>/perf-sentinel-simulation-lab

make down
make up
make seed-services
make validate-findings        # expected: 10/10 scenarios pass
make verify-all-scenarios     # expected: 24/24 detector outcomes match
```

If either step fails, do not record a PASS. Fix the underlying issue in `perf-sentinel`, rebuild the image, and rerun the lab.

If both pass, record the validation in the ledger:

```bash
# From the lab repo, produces one tab-separated line on stdout.
scripts/record-validation.sh vX.Y.Z PASS

# Copy the line and append it to release-gate/lab-validations.txt in
# the perf-sentinel repo. The ledger is append-only. Never edit prior
# entries.
```

### 5. Pre-flight gate

Back in the `perf-sentinel` checkout:

```bash
release-gate/check-lab-validation.sh --version vX.Y.Z
```

The `--version` argument accepts either `vX.Y.Z` or `X.Y.Z` (the latter normalized to `vX.Y.Z` internally), matching the convention of `scripts/check-tag-version.sh`. The ledger's column 1 must always carry the leading `v` (for example `v0.7.2`). Expected output on success:

```
release-gate: PASS for vX.Y.Z dated YYYY-MM-DD (lab commit <sha>, <N>d old, threshold 30d). OK to release.
```

The gate has three failure modes, each with an actionable remedy:

| Failure                             | Message                                                          | Remedy                                                                                                     |
|-------------------------------------|------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------|
| Version absent or only FAIL entries | `no PASS entry for vX.Y.Z in ...`                                | Rerun the lab and append a PASS line.                                                                      |
| Latest PASS too old                 | `latest PASS for vX.Y.Z is N days old ... Threshold is 30 days.` | Rerun the lab on the current branch and append a fresh PASS entry.                                         |
| Ledger file missing                 | `ledger ... not found.`                                          | Make sure `release-gate/lab-validations.txt` is present next to the script, or set `LEDGER=/path/to/file`. |

The age threshold is configurable for backfill or audit scenarios: `--max-age-days 365` accepts entries up to a year old. The default (30 days) is the working value, do not override it for normal releases.

### 6. Merge and tag

After the gate passes:

```bash
git checkout main
git merge release/X.Y.Z --no-ff -m "Merge release/X.Y.Z"
git tag -s vX.Y.Z -m "vX.Y.Z"
git push origin main vX.Y.Z
```

The tag push triggers `.github/workflows/release.yml`. Its first job re-runs `scripts/check-tag-version.sh` as a sanity gate, then the build matrix produces binaries, the publish job pushes to crates.io strictly (no soft fallback on rate-limit), and the docker job scans the image with Trivy (hard exit on HIGH or CRITICAL) before pushing the multi-arch manifest to GHCR and Docker Hub.

Provenance for every release binary is attested via `actions/attest-build-provenance` (Sigstore OIDC, keyless), producing SLSA Build L3 attestations queryable through `gh attestation verify`. The migration from `slsa-framework/slsa-github-generator` to `actions/attest-build-provenance` landed in 0.7.1.

### 7. Release the Helm chart

Wait for the GHCR image to appear (typically 5-10 minutes after the workflow run), then:

```bash
git tag chart-vA.B.C
git push origin chart-vA.B.C
```

This triggers `.github/workflows/helm-release.yml`, which validates the chart tag against `Chart.yaml` via `scripts/check-helm-tag-version.sh`, packages the chart, and publishes it to the GitHub Pages chart repository.

### 8. Public communication

After the GitHub release page is generated and the chart is live:

- LinkedIn post linking the release notes
- Blog entry on the project site if the release introduces user-facing capabilities
- Reddit, Hacker News, community channels when the change is broadly relevant
- Institutional contacts (academic collaborators, customers under disclosure agreements) for releases that touch their use cases

## What the release workflow does

For reference, here is what `release.yml` runs on every `v*` tag push:

1. **check-versions**: `scripts/check-tag-version.sh "${GITHUB_REF_NAME}"` rejects any mismatch between the tag and the workspace version files (Cargo.toml only, see the script header for the exact scope).
2. **build** (matrix): cross-compiles `perf-sentinel` for `linux-amd64-gnu`, `linux-amd64-musl`, `linux-arm64-musl`, `macos-arm64`, `windows-amd64`. The musl variants use `mimalloc` as the global allocator (see `docs/design/07-CLI-CONFIG-RELEASE.md`).
3. **release**: gathers artifacts, computes SHA-256 checksums, attests build provenance via Sigstore (keyless OIDC), and creates the GitHub release with all assets and notes from `CHANGELOG.md`.
4. **publish-crate**: publishes `perf-sentinel-core` then `perf-sentinel` to crates.io, waits for the index to update, fails the workflow on timeout instead of warning.
5. **docker**: builds the multi-arch image, scans it with Trivy (`exit-code: 1` on HIGH or CRITICAL CVE), uploads the SARIF, then pushes to GHCR and Docker Hub.

The release gate is **never** invoked from this workflow by design. If you find a PR adding a gate step to `release.yml`, reject it. The gate validates against a real k3d cluster, which CI cannot reproduce, and an empty automated check would silently degrade the gate's guarantee.

## Troubleshooting

**Gate fails with "no PASS entry" but you just ran the lab.** The line from `record-validation.sh` is printed on stdout, not appended automatically. Open `release-gate/lab-validations.txt` and paste the line manually. Verify the separators are tab characters, not spaces, with `cat -t release-gate/lab-validations.txt` (on macOS) or `cat -A` (on GNU).

**Gate fails with "is N days old".** A stale validation typically means the release branch has accumulated commits since the lab run. Rerun the lab on the latest commit, append a fresh PASS line, and retry the gate.

**Gate prints `warning: ignoring malformed line N`.** A previous append got mangled (split tabs, wrong column count). Open the ledger, find line N, fix the separator or column count. The gate continues processing the rest of the file.

**`check-tag-version.sh` fails on `crates/sentinel-cli/Cargo.toml`.** The script checks both the workspace `version` and the intra-workspace pin on `perf-sentinel-core`. Both must be bumped together.

**`publish-crate` times out waiting for the crates.io index.** The job fails strictly to avoid releasing a partial state. Wait 5 minutes, then re-run the failed job from the GitHub Actions UI. If the crate is already on the index, the job will detect it and complete.

**Trivy scan flags a HIGH or CRITICAL CVE.** The `docker` job blocks. Check if a base-image rebuild resolves it (`docker pull` then rerun the workflow). If the CVE is in a Cargo dependency, bump the dependency in a follow-up PR and cut a patch release. Do not bypass the scan.

## Ledger format reference

The ledger at `release-gate/lab-validations.txt` is tab-separated, append-only, and ignores lines starting with `#` or empty lines. Each entry has four columns:

```
<version>\t<lab_commit_sha>\t<YYYY-MM-DD>\t<PASS|FAIL>
```

- `version`: matches the tag form, including the leading `v` (for example `v0.7.2`). The gate compares this column to its `--version` argument literally.
- `lab_commit_sha`: short SHA of the lab repo HEAD at validation time, used to reproduce the lab state if a question arises later.
- `date`: UTC calendar date when the validation completed. Must be strict `YYYY-MM-DD`. The gate rejects anything else (fuzzy strings like `now` are not accepted).
- `verdict`: `PASS` or `FAIL`. The gate only accepts `PASS`.

FAIL entries are not strictly required (the gate treats them the same as a missing entry), but recording them in the ledger preserves the institutional memory of why a candidate version was held back.
