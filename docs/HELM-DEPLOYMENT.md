# Helm deployment guide

This guide walks through deploying perf-sentinel on Kubernetes via the
packaged Helm chart under [`charts/perf-sentinel/`](../charts/perf-sentinel/).
The chart ships the daemon (`perf-sentinel watch`) behind a `ClusterIP`
Service exposing OTLP gRPC (4317) and OTLP HTTP plus `/metrics` plus
`/api/*` (4318).

For a non-Helm alternative, see the raw manifests in
[`docs/INSTRUMENTATION.md`](./INSTRUMENTATION.md#kubernetes-deployment).

## TL;DR

```bash
helm install perf-sentinel oci://ghcr.io/robintra/charts/perf-sentinel \
  --version 0.2.0 \
  --namespace observability --create-namespace
kubectl --namespace observability get pods -l app.kubernetes.io/name=perf-sentinel
```

Every published release is Cosign-keyless-signed, shipped with a
SLSA v1.0 build provenance attestation, and shipped with an SPDX
SBOM. See [Software supply chain](#software-supply-chain) below to
check them before installing.

After the pod is ready, point your OpenTelemetry Collector at
`perf-sentinel.observability.svc.cluster.local:4317` (gRPC) or `:4318`
(HTTP). A full end-to-end example composing perf-sentinel with the
upstream OTel Collector chart lives under
[`examples/helm/`](../examples/helm/).

## Topology

The chart is sentinel-only by design. Users compose perf-sentinel with
the upstream
[open-telemetry/opentelemetry-collector](https://github.com/open-telemetry/opentelemetry-helm-charts)
chart instead of bundling a collector that would get out of sync with
upstream releases.

```mermaid
flowchart LR
    subgraph apps [Application namespaces]
        A[api-gateway]
        B[order-svc]
        C[payment-svc]
        D[chat-svc]
    end
    subgraph obs [observability namespace]
        OC[OTel Collector<br/>open-telemetry/opentelemetry-collector]
        PS[perf-sentinel<br/>this chart]
    end
    subgraph mon [monitoring namespace]
        T[Tempo]
    end
    A -->|OTLP or Zipkin| OC
    B -->|OTLP or Zipkin| OC
    C -->|OTLP or Zipkin| OC
    D -->|OTLP or Zipkin| OC
    OC -->|OTLP gRPC 4317| T
    OC -->|OTLP gRPC 4317| PS
```

## Install from OCI registry

The chart is published as an OCI artifact at
`oci://ghcr.io/robintra/charts/perf-sentinel`. Every version gets
Cosign keyless signing (GitHub OIDC, Rekor transparency log), a
SLSA v1.0 build provenance attestation stored on the repository's
attestation store, and an SPDX SBOM shipped both as a GitHub Release
asset and as a signed attestation.

### Pin a version

```bash
helm install perf-sentinel oci://ghcr.io/robintra/charts/perf-sentinel \
  --version 0.2.0 \
  --namespace observability --create-namespace \
  -f my-values.yaml
```

Chart version and app version are decoupled: `version` is the chart
release, `appVersion` is the daemon image tag that ships with it. Pin
the app version explicitly in production via `image.tag` to avoid
drift when a newer chart ships.

## Artifact Hub

The chart is indexed on [Artifact Hub](https://artifacthub.io), where
users can discover it, browse its values schema, and read the
changelog.

Registration flow (done by the maintainer once per chart):

1. Sign in to artifacthub.io with a GitHub account.
2. In the control panel, add a repository of kind "Helm charts (OCI)"
   pointing to `oci://ghcr.io/robintra/charts/perf-sentinel`.
3. Artifact Hub issues a `repositoryID` (UUID).
4. Edit `charts/perf-sentinel/artifacthub-repo.yml`, replace the
   `REPLACE_AFTER_ARTIFACTHUB_REGISTRATION` placeholder with the
   UUID, commit and push.
5. Tag a new chart release (patch bump) so the release workflow
   pushes the updated `artifacthub-repo.yml` to the OCI registry
   under the special `artifacthub.io` tag.
6. Artifact Hub polls the registry and picks up the new metadata
   within 30 minutes. The "Verified publisher" badge appears on the
   next processing cycle.

## Software supply chain

Every published release is Cosign-keyless-signed, ships with a SLSA
v1.0 build provenance attestation, and ships with an SPDX SBOM
attested under the SPDX predicate. Users should check at least the
Cosign signature before installing, and the full set in
regulated environments.

### Verify the Cosign signature

Cosign keyless verification ties each release back to a specific
GitHub Actions workflow run. The certificate identity must match the
published release workflow, the OIDC issuer must be GitHub Actions:

```bash
cosign verify \
  --certificate-identity-regexp '^https://github.com/robintra/perf-sentinel/\.github/workflows/helm-release\.yml@refs/tags/chart-v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/robintra/charts/perf-sentinel:0.2.0
```

A successful run prints the Rekor log entry and the certificate
details. A mismatched or absent signature exits non-zero.

### Verify the SLSA build provenance

Each published chart tarball carries a SLSA v1.0 build provenance
attestation produced by `actions/attest-build-provenance` and stored
on the repository's attestation store (not on the OCI registry). The
attestation is queryable via `gh`:

```bash
gh release download chart-v0.2.0 \
  --repo robintra/perf-sentinel \
  --pattern 'perf-sentinel-*.tgz'

gh attestation verify perf-sentinel-0.2.0.tgz \
  --repo robintra/perf-sentinel
```

If you already have the OCI artifact pulled and prefer not to fetch
the tarball, verify the build provenance directly against the OCI
reference:

```bash
docker login ghcr.io
gh attestation verify oci://ghcr.io/robintra/charts/perf-sentinel:0.2.0 \
  --repo robintra/perf-sentinel
```

Either recipe produces the same assurance. Pair whichever one you
pick with the Cosign signature check above to confirm both the
signer identity on the OCI artifact and the build provenance on the
tarball.

### Verify the SBOM

Each release ships an SPDX SBOM as a GitHub Release asset and as a
signed attestation on the repository's attestation store.

Fetch and verify:

```bash
gh release download chart-v0.2.0 --repo robintra/perf-sentinel \
  --pattern 'perf-sentinel-chart-*.spdx.json'

gh attestation verify perf-sentinel-chart-0.2.0.spdx.json \
  --repo robintra/perf-sentinel \
  --predicate-type https://spdx.dev/Document/v2.3
```

The SBOM captures the chart's declared dependencies at release time.

## Install from a local checkout

For contributors and users who want to inspect, patch, or bisect the
chart before installing, a local clone still works:

```bash
git clone https://github.com/robintra/perf-sentinel.git
cd perf-sentinel

# Inspect or override defaults before install.
helm show values ./charts/perf-sentinel > my-values.yaml

helm install perf-sentinel ./charts/perf-sentinel \
  --namespace observability --create-namespace \
  -f my-values.yaml
```

Keep the OCI path for production installs. The local path bypasses
Cosign and SLSA checks by design, so it should not be used against
shared clusters unless you built the chart yourself.

## Cutting a new chart release

The release workflow (`.github/workflows/helm-release.yml`) triggers
on tags matching `chart-v*`. The first gate
(`scripts/check-helm-tag-version.sh`) asserts strict equality between
the tag and `charts/perf-sentinel/Chart.yaml:version`, including any
semver prerelease suffix. The flow for a release:

1. On a branch, bump `charts/perf-sentinel/Chart.yaml:version` to the
   target (e.g. `0.3.0-rc.1` for a release candidate, `0.3.0` for the
   final). Semver prerelease suffixes are supported.
2. Add a matching section to `charts/perf-sentinel/CHANGELOG.md`.
3. Open a PR. The helm-ci workflow runs lint, template+kubeconform
   across the three workload kinds, and a version-bump guard that
   fails if the chart was edited without a matching `version:` bump.
4. Merge the PR, then tag:
   ```bash
   git tag chart-v0.3.0-rc.1
   git push origin chart-v0.3.0-rc.1
   ```
5. The helm-release workflow packages the chart, pushes it to OCI,
   Cosign-signs, generates SLSA provenance, and opens a **draft**
   GitHub Release. Prerelease tags (`-rc`, `-beta`, `-alpha`) get the
   `prerelease: true` flag automatically.
6. Review the draft release and the OCI artifact, then publish.

## Workload modes

The chart supports three `workload.kind` values. Pick one per install.

### `Deployment` (default)

Single daemon behind a `ClusterIP` Service. This is the recommended
topology. perf-sentinel is stateful per trace (the `TraceWindow` lives in
memory), so running one daemon and scaling vertically is the right first
move. The
[sharded topology](../examples/docker-compose-sharded.yml) is available
for multi-daemon deployments, it relies on consistent hashing by
`trace_id` in the OTel Collector's `loadbalancingexporter` so every span
of a given trace lands on the same daemon instance.

```yaml
workload:
  kind: Deployment
  replicas: 1
```

### `DaemonSet`

Rare. Useful only when you have a hard requirement for a daemon on every
node (e.g. taking over an existing node-local trace forwarder role). Note
that a DaemonSet splits traces across nodes, which breaks N+1 detection
unless an upstream collector ensures all spans of a trace reach the same
daemon. Most users do not need this mode.

```yaml
workload:
  kind: DaemonSet
```

### `StatefulSet`

Reserved for future on-disk persistence. The chart provisions the
volumeClaimTemplate end-to-end so the toggle works today, but no daemon
feature currently writes under `/var/lib/perf-sentinel`. Use this mode
only if you are prototyping a persistence extension.

```yaml
workload:
  kind: StatefulSet
  replicas: 1
  statefulset:
    persistence:
      enabled: true
      size: 5Gi
      storageClass: gp3
```

## Config surface

The chart mounts a single ConfigMap at
`/etc/perf-sentinel/.perf-sentinel.toml`. Edit the content via
`values.yaml`:

```yaml
config:
  toml: |
    [thresholds]
    n_plus_one_sql_critical_max = 0
    io_waste_ratio_max = 0.25

    [green]
    enabled = true
    default_region = "eu-west-3"

    [daemon]
    listen_address = "0.0.0.0"
    environment = "production"
```

Full field reference: [`docs/CONFIGURATION.md`](./CONFIGURATION.md).

### Secrets

The TOML file must never contain secrets (the daemon rejects credential
fields at config load). Inject sensitive values via environment variables
fed by a Secret:

```bash
kubectl -n observability create secret generic perf-sentinel-secrets \
  --from-literal=PERF_SENTINEL_EMAPS_TOKEN=sk-your-token
```

```yaml
extraEnvFrom:
  - secretRef:
      name: perf-sentinel-secrets
```

perf-sentinel itself does not read environment variables for
configuration directly, so the pattern is: the Secret goes into the pod
env, and the config file references it via `env:VAR_NAME` for the
handful of fields that support that form (Electricity Maps `api_key`,
etc.). See the "Environment variables" section of
`docs/CONFIGURATION.md`.

### Calibration files and TLS certs

Both go through `extraVolumes` plus `extraVolumeMounts`:

```yaml
extraVolumes:
  - name: tls
    secret:
      secretName: perf-sentinel-tls
      defaultMode: 0400
extraVolumeMounts:
  - name: tls
    mountPath: /etc/tls
    readOnly: true

config:
  toml: |
    [daemon]
    tls_cert_path = "/etc/tls/tls.crt"
    tls_key_path = "/etc/tls/tls.key"
```

## Observability

### Prometheus ServiceMonitor

When the Prometheus Operator is installed, flip `serviceMonitor.enabled`
to scrape `/metrics` on port 4318:

```yaml
serviceMonitor:
  enabled: true
  interval: 15s
  scrapeTimeout: 10s
  labels:
    # Match whatever selector your Prometheus resource uses.
    release: prometheus
```

### Exemplars

perf-sentinel emits Prometheus exemplars on
`perf_sentinel_findings_total`, `perf_sentinel_io_waste_ratio` and
`perf_sentinel_slow_duration_seconds`. Enable exemplar storage on your
Prometheus:

```yaml
prometheus:
  prometheusSpec:
    enableFeatures:
      - exemplar-storage
```

Then configure Grafana to click through from metric to trace:

```yaml
datasources:
  - name: Prometheus
    type: prometheus
    jsonData:
      exemplarTraceIdDestinations:
        - name: trace_id
          datasourceUid: tempo
```

### Without the Prometheus Operator

If you use a plain Prometheus without the operator, add a static scrape
entry instead:

```yaml
scrape_configs:
  - job_name: perf-sentinel
    kubernetes_sd_configs:
      - role: service
        namespaces:
          names: [observability]
    relabel_configs:
      - source_labels: [__meta_kubernetes_service_label_app_kubernetes_io_name]
        regex: perf-sentinel
        action: keep
      - source_labels: [__meta_kubernetes_endpoint_port_name]
        regex: otlp-http
        action: keep
```

## Upgrading

```bash
helm upgrade perf-sentinel ./charts/perf-sentinel \
  --namespace observability \
  -f my-values.yaml
```

The daemon does not hot-reload its config, so changes to `config.toml`
require a pod restart. The chart handles this automatically: a
`checksum/config` annotation on the pod template computes a hash of the
rendered ConfigMap, so any config edit bumps the annotation and triggers
a rolling restart. No manual `kubectl rollout restart` is needed.

When bumping the chart to a new `appVersion`, pin `image.tag` explicitly
and review `CHANGELOG.md` for breaking config changes. The chart does
not yet validate that the daemon version matches the chart version; this
is the operator's responsibility.

## Uninstalling

```bash
helm uninstall perf-sentinel --namespace observability
```

This removes the Deployment, Service, ConfigMap, ServiceAccount and
(when created) ServiceMonitor and NetworkPolicy. StatefulSet mode with
persistence retains the underlying PersistentVolumeClaims by default,
per Kubernetes semantics. Delete them explicitly if you are wiping
state:

```bash
kubectl --namespace observability delete pvc \
  -l app.kubernetes.io/instance=perf-sentinel
```

## End-to-end example

[`examples/helm/`](../examples/helm/) ships two values files composing
the perf-sentinel chart with the upstream OTel Collector chart for a
Zipkin + OTLP fan-out topology to Tempo and perf-sentinel. Walk through
the README there for the full install + verification recipe.
