# Changelog

All notable changes to the perf-sentinel Helm chart are documented in
this file. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Chart versions are independent from the perf-sentinel application
versions, the chart's `appVersion` field tracks which daemon version is
the default target.

## [0.2.28]

### Changed

- `appVersion` bumped to `0.5.25`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.25`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.25 binary adds two Prometheus counters on the daemon
  Scaphandre scraper (`perf_sentinel_scaphandre_scrape_total{status}`
  and `perf_sentinel_scaphandre_scrape_failed_total{reason}`)
  pre-warmed at startup so dashboards build with `rate()` queries
  without `absent()` guards. No chart-level template change. See
  `docs/METRICS.md` Scaphandre scrape counters section for the label
  values and sample PromQL queries.

## [0.2.27]

### Changed

- `appVersion` bumped to `0.5.24`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.24`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.24 binary adds interactive ack/revoke from the TUI
  (`perf-sentinel query inspect`, with `a` and `u` keybindings on
  the selected finding) and a new `--api-key-file` flag on
  `query inspect`. The daemon HTTP surface is unchanged, the TUI
  consumes the existing ack endpoints. No chart-level config
  change. This release closes the three-axis UX marathon over the
  daemon ack API (CLI 0.5.22, HTML 0.5.23, TUI 0.5.24). See
  `docs/INSPECT.md` and `docs/ACK-WORKFLOW.md` for the user-facing
  reference.

## [0.2.26]

### Changed

- `appVersion` bumped to `0.5.23`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.23`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.23 binary adds an opt-in HTML report live mode (the new
  `perf-sentinel report --daemon-url <URL>` flag) and an opt-in
  daemon CORS layer (`[daemon.cors] allowed_origins`). The daemon
  HTTP surface is otherwise unchanged. The static HTML report
  generated without `--daemon-url` is fully byte-equivalent to the
  0.5.22 output. No chart-level config change. See `docs/HTML-REPORT.md`
  and the `[daemon.cors]` section in `docs/CONFIGURATION.md` for the
  user-facing reference.

## [0.2.25]

### Changed

- `appVersion` bumped to `0.5.22`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.22`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.22 binary ships a new `perf-sentinel ack` CLI subcommand for
  the daemon ack API, with `create`, `revoke`, `list` actions and
  flexible auth / duration parsing. The daemon HTTP surface is
  unchanged, the new CLI consumes the existing endpoints. No
  chart-level config change. See `docs/CLI.md` and
  `docs/ACK-WORKFLOW.md` for the user-facing reference.

## [0.2.24]

### Changed

- `appVersion` bumped to `0.5.21`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.21`. The
  `artifacthub.io/images` annotation is updated in lockstep. The
  0.5.21 daemon adds two Prometheus counters on `/metrics` for
  observability of operator-driven activity on the ack endpoints
  (`perf_sentinel_ack_operations_total` and
  `perf_sentinel_ack_operations_failed_total`). No chart-level config
  change, scrapers pick up the new series automatically. See
  `docs/METRICS.md` for the label set and pre-warmed combinations.

## [0.2.23]

### Changed

- `appVersion` bumped to `0.5.20`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.20`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.19` if you pin. The 0.5.20 daemon adds the runtime ack API
  (`POST` / `DELETE` / `GET /api/findings/{sig}/ack` and
  `GET /api/acks`); operators wiring the new endpoints from outside
  the cluster should review the loopback-by-default posture and the
  optional `[daemon.ack] api_key` setting (see `docs/QUERY-API.md`).

## [0.2.22]

### Changed

- `appVersion` bumped to `0.5.19`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.19`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.18` if you pin.

## [0.2.21]

### Changed

- `appVersion` bumped to `0.5.18`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.18`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.17` if you pin.

## [0.2.20]

### Changed

- `appVersion` bumped to `0.5.17`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.17`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.16` if you pin.

## [0.2.19]

### Changed

- `appVersion` bumped to `0.5.16`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.16`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.15` if you pin.

## [0.2.18]

### Changed

- `appVersion` bumped to `0.5.15`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.15`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.14` if you pin.

## [0.2.17]

### Changed

- `appVersion` bumped to `0.5.14`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.14`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.13` if you pin.

## [0.2.16]

### Changed

- `appVersion` bumped to `0.5.13`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.13`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.12` if you pin.

## [0.2.15]

### Changed

- `appVersion` bumped to `0.5.12`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.12`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.11` if you pin.

## [0.2.14]

### Changed

- `appVersion` bumped to `0.5.11`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.11`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.10` if you pin.

## [0.2.13]

### Changed

- `appVersion` bumped to `0.5.10`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.10`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.9` if you pin.

## [0.2.12]

### Changed

- `appVersion` bumped to `0.5.9`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.9`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.8` if you pin.

## [0.2.11]

### Changed

- `appVersion` bumped to `0.5.8`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.8`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.7` if you pin.

## [0.2.10]

### Changed

- `appVersion` bumped to `0.5.7`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.7`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.6` if you pin.

## [0.2.9]

### Changed

- `appVersion` bumped to `0.5.6`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.6`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.5` if you pin.

## [0.2.8]

### Changed

- `appVersion` bumped to `0.5.5`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.5`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.4` if you pin.

## [0.2.7]

### Changed

- `appVersion` bumped to `0.5.4`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.4`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.3` if you pin.

## [0.2.6]

### Changed

- Artifact Hub-facing `README.md` refreshed: the "Install from a local
  checkout" section no longer claims an OCI-published chart is pending,
  since OCI publication has been live since 0.2.0. The section now
  frames local-checkout install as the path for iterating on values or
  templates, and points to `docs/HELM-DEPLOYMENT.md` and the Artifact
  Hub badge for the OCI install. No `appVersion` or image tag change,
  the chart keeps pointing at `ghcr.io/robintra/perf-sentinel:0.5.3`.

## [0.2.5]

### Added

- `artifacthub.io/official: "true"` annotation on `Chart.yaml` so the
  chart can claim the Artifact Hub "Official" badge. The annotation
  alone does not flip the badge, Artifact Hub staff validate ownership
  before it activates.

## [0.2.4]

### Changed

- Chart `description` and Artifact Hub-facing `README.md` intro now
  read "distributed traces" instead of "OpenTelemetry traces",
  reflecting that perf-sentinel also ingests Jaeger, Zipkin, Tempo and
  pg_stat_statements feeds, not only OTLP.
- `[daemon] environment` row in the "Chart at a glance" table rewords
  the `confidence` tag description: consumed by downstream tooling,
  with perf-lint called out as a planned companion IDE integration
  (not yet published, dead GitHub link removed).
- No `appVersion` or image tag change, the chart keeps pointing at
  `ghcr.io/robintra/perf-sentinel:0.5.3`.

## [0.2.3]

### Changed

- `appVersion` bumped to `0.5.3`, the default daemon image tag now
  points at `ghcr.io/robintra/perf-sentinel:0.5.3`. The
  `artifacthub.io/images` annotation is updated in lockstep so the
  Artifact Hub listing advertises the matching image. Pickup the new
  binary with `helm upgrade` or override `image.tag` to stay on
  `0.5.2` if you pin.

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
