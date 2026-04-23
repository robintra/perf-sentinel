# Changelog

All notable changes to the perf-sentinel Helm chart are documented in
this file. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Chart versions are independent from the perf-sentinel application
versions, the chart's `appVersion` field tracks which daemon version is
the default target.

## [0.2.2]

### Changed

- `appVersion` bumped to `0.5.2`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.2`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.1` if you pin.

## [0.2.1]

### Changed

- `artifacthub-repo.yml` now carries the Artifact Hub
  `repositoryID` (`70c507ff-c75e-4808-9c3b-87fb5696dce8`) and
  maintainer email. Artifact Hub re-scrapes the `:artifacthub.io`
  OCI tag on the next cycle after this tag is published and marks
  the repository as "Verified publisher".

## [0.2.0]

### Added

- Artifact Hub listing. The chart is now discoverable on
  artifacthub.io. `charts/perf-sentinel/artifacthub-repo.yml`
  documents the repository ownership and is pushed to the OCI
  registry under the special `artifacthub.io` tag on every release.
  `Chart.yaml` now carries `artifacthub.io/*` annotations
  (category, license, links, images).
- SPDX SBOM per release. Every chart release ships a Syft-generated
  SPDX SBOM as a GitHub Release asset, attested via
  `actions/attest` with the SBOM predicate and verifiable through
  `gh attestation verify --predicate-type https://spdx.dev/Document/v2.3`.

### Changed

- `docs/HELM-DEPLOYMENT.md` (and its French parity) gains an
  Artifact Hub section, an SBOM verification subsection, and an
  OCI-based `gh attestation verify oci://...` recipe alongside the
  existing tarball-based one.

## [0.1.2]

### Fixed

- SLSA build provenance is now reliably attached to each published
  chart release via `actions/attest-build-provenance`, replacing the
  previous `slsa-framework/slsa-github-generator` reusable workflow
  whose draft-release integration diverged onto ephemeral
  `untagged-*` drafts under the `helm-release.yml` release flow. The
  provenance is now queryable via `gh attestation verify` without
  requiring a separate `.intoto.jsonl` asset on the GitHub Release.
  Cosign signatures on the OCI artifact are unchanged.

### Changed

- `scripts/check-chart-version-bumped.sh` now also requires that
  `charts/perf-sentinel/CHANGELOG.md` contain a matching
  `## [NEW_VERSION]` section on HEAD that was not present on the PR
  base. A PR that bumps the chart version without a corresponding
  changelog entry now fails CI rather than merging silently.

## [0.1.1]

### Fixed

- `helm test` no longer races with CoreDNS warm-up on a freshly started
  cluster. The test Job's wget now retries up to 5 times over 15 seconds
  instead of giving up on the first `bad address` error when the Service
  DNS record has not yet propagated. Observed against minikube when
  chaining `minikube start + helm install + helm test` back to back.

## [0.1.0-rc.1]

First release candidate for the 0.1.0 chart. Chart content is identical
to the upcoming 0.1.0. The RC exists to dry-run the release pipeline:
OCI push to `ghcr.io/robintra/charts/perf-sentinel`, Cosign keyless
signing via the GitHub OIDC token, SLSA level 3 provenance on the
tarball, draft GitHub Release with both the `.tgz` and the
`.intoto.jsonl` as assets.

Promote to 0.1.0 once the RC's OCI artifact verifies clean via
`cosign verify` and `slsa-verifier verify-artifact`.

## [0.1.0]

Initial release of the perf-sentinel Helm chart.

### Added

- Helm chart deployable as Deployment (default), DaemonSet or
  StatefulSet via `workload.kind`.
- ConfigMap-backed `.perf-sentinel.toml` with rolling update on
  content change via `checksum/config` pod annotation.
- ClusterIP Service exposing OTLP gRPC (4317) and OTLP HTTP (4318).
- Optional ServiceMonitor for Prometheus Operator users
  (`serviceMonitor.enabled`).
- Optional NetworkPolicy (`networkPolicy.enabled`), fail-closed by
  default when enabled.
- Liveness and readiness probes wired to the lock-free `/health`
  endpoint.
- Extension hooks: `extraEnv`, `extraEnvFrom`, `extraVolumes`,
  `extraVolumeMounts`, `extraArgs`.
- Values schema (`values.schema.json`) validating the `workload.kind`
  enum and shape of the main keys.
