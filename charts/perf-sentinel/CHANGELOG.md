# Changelog

All notable changes to the perf-sentinel Helm chart are documented in
this file. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Chart versions are independent from the perf-sentinel application
versions, the chart's `appVersion` field tracks which daemon version is
the default target.

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
