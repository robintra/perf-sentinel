# Helm example: perf-sentinel + OpenTelemetry Collector

End-to-end Kubernetes topology where services emit OTLP or Zipkin traces to
an in-cluster OpenTelemetry Collector. The collector fans the traces out to
two backends: Grafana Tempo (for storage and query) and perf-sentinel (for
anti-pattern detection and GreenOps scoring).

```
 services  ---OTLP grpc---\
 (api-gateway, order-svc,   \
  payment-svc, chat-svc)     +--> otel-collector --+--> tempo          (storage)
                            /                      |
 services  ---Zipkin------+                        +--> perf-sentinel  (analysis)
```

The perf-sentinel chart is sentinel-only by design. The collector is
composed by the user via the upstream
[open-telemetry/opentelemetry-collector](https://github.com/open-telemetry/opentelemetry-helm-charts)
chart. This keeps the perf-sentinel chart small and avoids duplicating
collector configuration logic that the upstream project already solves.

## Prerequisites

- A Kubernetes cluster (tested shapes: kind, minikube, k3d, or any managed
  K8s).
- Helm v3.12 or newer.
- Two namespaces: `observability` (for perf-sentinel and the collector)
  and `monitoring` (placeholder for Tempo, assumed pre-installed in this
  example). Replace these with your own conventions.
- For `serviceMonitor.enabled=true`, the
  [kube-prometheus-stack](https://github.com/prometheus-operator/kube-prometheus)
  operator must be installed. When absent, set the flag to `false` or swap
  in your own scrape config.

## Install order

The perf-sentinel Service must exist before the collector starts pushing
to it, otherwise the exporter logs DNS errors until the Service appears.
Install perf-sentinel first, then the collector.

```bash
# 1. Add the upstream OTel Collector Helm repo.
helm repo add open-telemetry https://open-telemetry.github.io/opentelemetry-helm-charts
helm repo update

# 2. Install perf-sentinel in the observability namespace.
helm install perf-sentinel ../../charts/perf-sentinel \
  --namespace observability --create-namespace \
  -f values-perf-sentinel.yaml

# 3. Install the OpenTelemetry Collector in the same namespace.
helm install otel-collector open-telemetry/opentelemetry-collector \
  --namespace observability \
  -f values-otel-collector.yaml
```

Tempo is assumed to be pre-installed under
`tempo.monitoring.svc.cluster.local:4317`. Swap the exporter endpoint in
`values-otel-collector.yaml` if your deployment differs.

## Verify the pipeline

```bash
# 1. perf-sentinel /health must answer 200.
kubectl --namespace observability port-forward \
  svc/perf-sentinel 14318:4318
curl -sS http://127.0.0.1:14318/health && echo ok

# 2. Send a sample OTLP trace to the collector.
kubectl --namespace observability port-forward \
  svc/otel-collector 4318:4318
curl -sS -X POST http://127.0.0.1:4318/v1/traces \
  -H 'Content-Type: application/json' \
  -d @sample-trace.json

# 3. Watch perf-sentinel findings stream out.
kubectl --namespace observability logs -f \
  -l app.kubernetes.io/name=perf-sentinel
```

## Switching to production TLS

The example uses `tls.insecure: true` on every exporter because this is the
canonical in-cluster pattern for a quick start. For production:

1. Issue certs via cert-manager or your internal PKI. Mount them into both
   the collector pod and perf-sentinel (via `extraVolumes` on the chart)
   under `/etc/tls/`.
2. Enable TLS on the perf-sentinel daemon by setting `tls_cert_path` and
   `tls_key_path` in the `config.toml` block of
   `values-perf-sentinel.yaml`.
3. Flip `tls.insecure: false` on the `otlp/perf-sentinel` exporter in
   `values-otel-collector.yaml` and set `tls.ca_file` to your internal
   trust bundle.

See `docs/HELM-DEPLOYMENT.md` and `docs/CONFIGURATION.md` for the full
surface.

## Adjusting to your topology

- **Single-region, Zipkin-only apps**: keep the `zipkin` receiver and
  remove the `otlp` receiver from `values-otel-collector.yaml`.
- **Multi-region**: add a `resource` processor that stamps
  `cloud.region` on spans lacking it, so perf-sentinel can attribute
  CO2 to the right region. See `docs/CONFIGURATION.md` under
  `[green.service_regions]`.
- **Sampling at high volume**: enable the `tail_sampling` processor on
  the collector and route only error or slow traces to perf-sentinel.
  Sampling below 100% means perf-sentinel cannot see every N+1 pattern,
  trade volume against detection recall.
