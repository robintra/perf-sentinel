# perf-sentinel Helm chart

Deploy [perf-sentinel](https://github.com/robintra/perf-sentinel) in a
Kubernetes cluster as a `Deployment`, `DaemonSet` or `StatefulSet`, behind
a `ClusterIP` Service exposing OTLP gRPC (4317) and OTLP HTTP + `/metrics`
+ `/api/*` (4318).

perf-sentinel is a lightweight, polyglot performance anti-pattern detector
for distributed traces. It detects N+1 SQL, N+1 HTTP, redundant queries,
chatty services, connection pool saturation, serialized but parallelizable
calls and excessive fanout. It also scores I/O intensity per endpoint
(`GreenOps`) and emits SCI v1.0-aligned CO2 estimates.

## Chart at a glance

| Key                      | Default                           | Notes                                                             |
|--------------------------|-----------------------------------|-------------------------------------------------------------------|
| `image.repository`       | `ghcr.io/robintra/perf-sentinel`  | Published on GHCR.                                                |
| `image.tag`              | `""` (falls back to `appVersion`) | Pin explicitly in production.                                     |
| `workload.kind`          | `Deployment`                      | `DaemonSet` and `StatefulSet` are opt-in.                         |
| `workload.replicas`      | `1`                               | Per-trace state lives in memory, prefer vertical scaling first.   |
| `service.type`           | `ClusterIP`                       | Do not switch to `NodePort` or `LoadBalancer` without a gateway.  |
| `serviceMonitor.enabled` | `false`                           | Flip on when the Prometheus Operator is installed.                |
| `networkPolicy.enabled`  | `false`                           | Fail-closed when enabled without selectors.                       |
| `[daemon] environment`   | `"staging"` (via `config.toml`)   | Stamps every finding with a confidence tag consumed by downstream tooling (perf-lint, planned). |

## Install from a local checkout

Installing from a local checkout is useful when iterating on a values
file or on the chart templates. For the OCI install path (the default
for most users), see [`docs/HELM-DEPLOYMENT.md`](../../docs/HELM-DEPLOYMENT.md)
or the Artifact Hub badge at the top of this page.

```bash
git clone https://github.com/robintra/perf-sentinel.git
cd perf-sentinel
helm install perf-sentinel ./charts/perf-sentinel \
  --namespace observability --create-namespace
```

For the end-to-end topology (services -> OTel Collector -> perf-sentinel +
Tempo), see [`examples/helm/`](../../examples/helm/).

## Documentation

Full guide lives in
[`docs/HELM-DEPLOYMENT.md`](../../docs/HELM-DEPLOYMENT.md). Configuration
reference for the embedded `.perf-sentinel.toml` is in
[`docs/CONFIGURATION.md`](../../docs/CONFIGURATION.md).
