# perf-sentinel instrumentation guide

This guide covers the parts of the data pipeline that turn an application's runtime activity into the OTLP / JSON input perf-sentinel consumes. For an end-to-end overview, the four supported topologies and the four quick starts, see [INTEGRATION.md](./INTEGRATION.md). For the CI-side of the integration (CI mode, GitHub Actions / GitLab CI / Jenkins recipes, interactive HTML report deployment, PR regression detection), see [CI.md](./CI.md).

## Contents

- [Kubernetes deployment](#kubernetes-deployment): manifests for the daemon and the OTel Collector sidecar.
- [Cloud provider integrations](#cloud-provider-integrations): AWS X-Ray, GCP Cloud Trace, Azure Application Insights, self-hosted Jaeger / Tempo / Zipkin.
- [Production: via OpenTelemetry Collector](#production-via-opentelemetry-collector): central collector setup, sampling and detection accuracy.
- [Required span attributes](#required-span-attributes): the legacy and stable OTel semantic conventions perf-sentinel reads.
- [Dev/staging: per-language instrumentation](#devstaging-per-language-instrumentation): step-by-step setup for Java, Quarkus, .NET, Rust.

## Kubernetes deployment

A packaged Helm chart is available under [`charts/perf-sentinel/`](../charts/perf-sentinel/). See [HELM-DEPLOYMENT.md](./HELM-DEPLOYMENT.md) for the full install guide and [`examples/helm/`](../examples/helm/) for a worked example composing the chart with the upstream OpenTelemetry Collector chart. The raw manifests below remain for users who prefer to deploy without Helm.

perf-sentinel runs as a standard Kubernetes Deployment behind a Service. The OTel Collector runs as a DaemonSet (per-node) or Deployment (centralized), forwarding traces to perf-sentinel.

### Minimal manifests

```yaml
# perf-sentinel Deployment
apiVersion: apps/v1
kind: Deployment
metadata:
  name: perf-sentinel
  namespace: monitoring
spec:
  replicas: 1
  selector:
    matchLabels:
      app: perf-sentinel
  template:
    metadata:
      labels:
        app: perf-sentinel
    spec:
      containers:
        - name: perf-sentinel
          image: ghcr.io/robintra/perf-sentinel:latest
          ports:
            - containerPort: 4317   # OTLP gRPC
            - containerPort: 4318   # OTLP HTTP + /metrics
          readinessProbe:
            httpGet:
              path: /metrics
              port: 4318
            initialDelaySeconds: 5
          resources:
            requests:
              memory: "16Mi"
              cpu: "50m"
            limits:
              memory: "64Mi"
              cpu: "200m"
          securityContext:
            readOnlyRootFilesystem: true
            allowPrivilegeEscalation: false
            runAsNonRoot: true
---
apiVersion: v1
kind: Service
metadata:
  name: perf-sentinel
  namespace: monitoring
spec:
  selector:
    app: perf-sentinel
  ports:
    - name: otlp-grpc
      port: 4317
    - name: otlp-http
      port: 4318
```

### OTel Collector exporter config

In your existing Collector config (DaemonSet or Deployment), add perf-sentinel as an exporter:

```yaml
exporters:
  otlp/perf-sentinel:
    endpoint: perf-sentinel.monitoring:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      exporters: [otlp/perf-sentinel, otlp/your-backend]
```

### Application instrumentation

Services send traces to the Collector via the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var. If using the OTel Operator, this is injected automatically. Otherwise, set it in your Deployment spec:

```yaml
env:
  - name: OTEL_EXPORTER_OTLP_ENDPOINT
    value: "http://otel-collector.monitoring:4317"
  - name: OTEL_EXPORTER_OTLP_PROTOCOL
    value: "grpc"
  - name: OTEL_SERVICE_NAME
    valueFrom:
      fieldRef:
        fieldPath: metadata.labels['app']
```

### Prometheus ServiceMonitor

If you use the Prometheus Operator, scrape perf-sentinel metrics with a ServiceMonitor:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: perf-sentinel
  namespace: monitoring
spec:
  selector:
    matchLabels:
      app: perf-sentinel
  endpoints:
    - port: otlp-http
      path: /metrics
      interval: 15s
```

---

## Cloud provider integrations

perf-sentinel is cloud-agnostic: it receives standard OTLP traces. The key is to route a copy of your traces to perf-sentinel alongside your cloud-native trace backend.

### AWS (X-Ray + OTel Collector)

AWS X-Ray uses a proprietary format, but the [AWS Distro for OpenTelemetry (ADOT)](https://aws-otel.github.io/) Collector can export both to X-Ray and to perf-sentinel:

```yaml
# ADOT Collector config
exporters:
  awsxray:
    region: eu-west-1
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [awsxray, otlp/perf-sentinel]
```

Deploy perf-sentinel as an ECS task or EKS Deployment. For ECS, use the `scratch`-based Docker image (`ghcr.io/robintra/perf-sentinel:latest`).

### GCP (Cloud Trace + OTel Collector)

GCP Cloud Trace supports OTLP ingestion natively. Use the standard OTel Collector with both the `googlecloud` exporter and the perf-sentinel exporter:

```yaml
exporters:
  googlecloud:
    project: my-gcp-project
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [googlecloud, otlp/perf-sentinel]
```

Deploy perf-sentinel as a Cloud Run service or GKE Deployment. For Cloud Run, expose port 4317 (gRPC) and 4318 (HTTP).

### Azure (Application Insights + OTel Collector)

Azure Monitor supports OTLP via the [Azure Monitor OpenTelemetry Exporter](https://learn.microsoft.com/en-us/azure/azure-monitor/app/opentelemetry-configuration). Route traces to both Azure and perf-sentinel:

```yaml
exporters:
  azuremonitor:
    connection_string: ${APPLICATIONINSIGHTS_CONNECTION_STRING}
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [azuremonitor, otlp/perf-sentinel]
```

Deploy perf-sentinel as an AKS Deployment or Azure Container Instance.

### Self-hosted (Jaeger, Tempo, Zipkin)

If you use a self-hosted trace backend, the OTel Collector approach works identically. Add perf-sentinel as an additional OTLP exporter alongside your existing backend exporter. Alternatively, use perf-sentinel's batch mode with trace files exported from Jaeger UI (`--input jaeger-export.json`) or Zipkin UI (`--input zipkin-traces.json`), formats are auto-detected.

---

## Production: via OpenTelemetry Collector

If you already have an [OTel Collector](https://opentelemetry.io/docs/collector/), you will be able to add perf-sentinel as an additional OTLP exporter. Your existing tracing pipeline (Jaeger, Tempo, etc.) keeps working, perf-sentinel analyzes a copy of the same spans.

```yaml
# otel-collector-config.yaml
exporters:
  otlp/perf-sentinel:
    endpoint: "perf-sentinel:4317"
    tls:
      insecure: true

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [otlp/perf-sentinel, otlp/jaeger]   # send to both
```

The OTel Collector ships gzip-compressed exports by default. perf-sentinel accepts both gzip and uncompressed payloads on the OTLP/HTTP endpoint (`POST /v1/traces`), no `compression: none` override required. The decompressed body still respects the `[daemon] max_payload_size` limit (1 MB by default).

This approach is recommended for production deployments because:
- Zero code changes in your services
- No rebuild, no redeployment
- Works regardless of language (Java, C#, Rust, Go, Python, Node.js)
- Sampling and filtering happen at the collector level
- perf-sentinel can be added or removed without touching application code

A full reference configuration is provided in [`examples/otel-collector-config.yaml`](../examples/otel-collector-config.yaml) with a matching Docker Compose file in [`examples/docker-compose-collector.yml`](../examples/docker-compose-collector.yml).

### End-to-end setup with Docker Compose

1. Start the stack:

```bash
docker compose -f examples/docker-compose-collector.yml up -d
```

2. Configure your applications to export OTLP traces to the collector:
   - gRPC: `localhost:4317`
   - HTTP: `localhost:4318`

3. Verify perf-sentinel is receiving spans:

```bash
curl -s http://localhost:14318/metrics | grep perf_sentinel_events_processed_total
```

4. View findings emitted by perf-sentinel on stdout:

```bash
docker compose -f examples/docker-compose-collector.yml logs -f perf-sentinel
```

### Sampling and filtering

For high-traffic environments, the OTel Collector supports tail-based sampling and filtering to reduce the volume of traces forwarded to perf-sentinel.

**Tail-based sampling** keeps complete traces based on criteria evaluated after all spans arrive:

```yaml
processors:
  tail_sampling:
    decision_wait: 10s
    policies:
      - name: errors
        type: status_code
        status_code:
          status_codes: [ERROR]
      - name: specific-services
        type: string_attribute
        string_attribute:
          key: service.name
          values: [game, account, gateway]
      - name: probabilistic
        type: probabilistic
        probabilistic:
          sampling_percentage: 10
```

**Filter processor** drops spans matching specific conditions:

```yaml
processors:
  filter:
    error_mode: ignore
    traces:
      span:
        - 'attributes["service.name"] == "health-check"'
```

Add the processor to the pipeline:

```yaml
service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [tail_sampling, batch]
      exporters: [otlp/perf-sentinel]
```

**Sampling and detection accuracy**.

Anti-pattern detection relies on counting events. Sampling that drops events directly affects which patterns perf-sentinel can flag.

- **Within a kept trace, all spans are preserved**. OTel and Jaeger sample per-trace, not per-span, so an N+1 loop, a chatty service hop or a fanout pattern that lives inside one request still detects cleanly as long as the parent trace is kept.
- **Head-based sampling breaks count-based detections**. A 1% head-based policy drops 99% of traces before they reach the collector, so a 50-call N+1 loop is observed as 3 calls, well below any reasonable threshold. Same for chatty services, fanout, serialized parallelizable calls, pool saturation. Anything threshold-driven gets silently underreported.
- **Tail-based sampling stays compatible with detection** because the policies you would write for incident review (keep errors, keep slow traces, keep specific services) are exactly the ones that surface anti-patterns. The [`tail_sampling` processor](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/processor/tailsamplingprocessor) example above keeps everything under those policies plus a 10% probabilistic sample of the rest.
- **CI runs should keep 100% of traces**. Volume is low (one integration-test run), the cost of full instrumentation is negligible, and missing a regression because of sampling defeats the purpose of the CI gate. The Quick start sections above assume 100% sampling.
- **`pg-stat` mode is sampling-immune**. `pg_stat_statements` aggregates query counters server-side in PostgreSQL, regardless of what the application tracer captured. A query that runs 10 000 times shows up as 10 000 calls even if 99% of the parent traces were dropped at the head. Run `perf-sentinel pg-stat ...` (or pass `--pg-stat` to `analyze` and `report`) as a fallback when you cannot trust the trace volume, or as a primary signal for code paths the tracer does not even cover.

> **Note:** tail-based sampling requires the `otel/opentelemetry-collector-contrib` image (not the core image).

---

## Required span attributes

perf-sentinel detects I/O anti-patterns by looking at specific span attributes. Both the legacy and stable [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/) are supported.

| Purpose         | Legacy attribute (pre-1.21) | Stable attribute (1.21+)    | Example                                   |
|-----------------|-----------------------------|-----------------------------|-------------------------------------------|
| SQL query text  | `db.statement`              | `db.query.text`             | `SELECT * FROM player WHERE game_id = 42` |
| SQL system      | `db.system`                 | `db.system`                 | `postgresql`, `mysql`                     |
| HTTP target URL | `http.url`                  | `url.full`                  | `http://account-svc:5000/api/account/123` |
| HTTP method     | `http.method`               | `http.request.method`       | `GET`, `POST`                             |
| HTTP status     | `http.status_code`          | `http.response.status_code` | `200`, `404`                              |
| Source endpoint | `http.route`                | `http.route`                | `POST /api/game/{id}/start`               |
| Service name    | `service.name` (resource)   | `service.name` (resource)   | `game`, `account-svc`                     |

Spans that have neither a SQL attribute nor an HTTP attribute are skipped: they are not I/O operations. Modern OTel agents (v2.x) emit the stable convention by default. Older agents emit the legacy convention. perf-sentinel handles both transparently.

---

## Dev/staging: per-language instrumentation

When no OTel Collector is available, instrument services directly. The guides below are ordered from easiest to most involved.

### Java (OpenTelemetry Java Agent)

The [OTel Java Agent](https://opentelemetry.io/docs/zero-code/java/agent/) instruments JDBC, R2DBC, HTTP clients, Spring Web and most frameworks automatically, with zero code changes. This is the closest to plug and play.

#### 1. Download the agent

```bash
curl -L -o opentelemetry-javaagent.jar \
  https://github.com/open-telemetry/opentelemetry-java-instrumentation/releases/latest/download/opentelemetry-javaagent.jar
```

#### 2. Run your application with the agent

```bash
export JAVA_TOOL_OPTIONS="-javaagent:/path/to/opentelemetry-javaagent.jar"
export OTEL_SERVICE_NAME=my-service
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_TRACES_SAMPLER=always_on
export OTEL_METRICS_EXPORTER=none
export OTEL_LOGS_EXPORTER=none
java -jar my-app.jar
```

The agent automatically captures:
- `db.query.text` from JDBC (Spring Data JPA, Hibernate) and R2DBC (Spring WebFlux reactive)
- `url.full` from HTTP clients (WebClient, RestTemplate, HttpClient)
- `http.route` from Spring MVC and Spring WebFlux incoming requests
- Trace context propagation across async boundaries, reactive chains and inter-service calls

This has been validated on Spring Boot 4 with WebFlux/R2DBC, Virtual Threads/JPA and standard MVC/JDBC.

#### 3. Docker Compose example

```yaml
services:
  my-service:
    build: ./my-service
    environment:
      - JAVA_TOOL_OPTIONS=-javaagent:/app/opentelemetry-javaagent.jar
      - OTEL_SERVICE_NAME=my-service
      - OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4317
      - OTEL_EXPORTER_OTLP_PROTOCOL=grpc
      - OTEL_TRACES_SAMPLER=always_on
      - OTEL_METRICS_EXPORTER=none
      - OTEL_LOGS_EXPORTER=none
```

Add the agent JAR to your Dockerfile:

```dockerfile
ADD https://github.com/open-telemetry/opentelemetry-java-instrumentation/releases/latest/download/opentelemetry-javaagent.jar /app/opentelemetry-javaagent.jar
```

#### Known limitations

**Project Leyden / AOT cache incompatibility.** The `-javaagent:` flag is incompatible with JEP 483 AOT caches (`-XX:AOTCache`). Bypass it when the agent is active:

```bash
if echo "$JAVA_TOOL_OPTIONS" | grep -q "javaagent"; then
  exec java -jar /app/my-app.jar
else
  exec java -XX:AOTCache=/app/app.aot -jar /app/my-app.jar
fi
```

**Spring Boot starter is not sufficient.** The `spring-boot-starter-opentelemetry` (Spring Boot 4) does not instrument outbound `WebClient` or `RestTemplate` calls with trace context propagation. Use the Java Agent for full N+1 HTTP detection.

---

### Java (Quarkus + quarkus-opentelemetry)

For Quarkus applications (including GraalVM native images where the Java Agent cannot be used), add the `quarkus-opentelemetry` extension:

```xml
<dependency>
    <groupId>io.quarkus</groupId>
    <artifactId>quarkus-opentelemetry</artifactId>
</dependency>
```

Configure in `application.properties`:

```properties
quarkus.otel.exporter.otlp.endpoint=${OTLP_GRPC_ENDPOINT:http://localhost:4317}
quarkus.otel.exporter.otlp.protocol=grpc
quarkus.otel.service.name=my-service
quarkus.otel.enabled=${OTEL_ENABLED:false}
quarkus.otel.metrics.exporter=none
quarkus.otel.logs.exporter=none
```

Set `OTEL_ENABLED=true` and `OTLP_GRPC_ENDPOINT` in your environment to activate tracing. For native images, use the `QUARKUS_` prefix for runtime overrides (e.g., `QUARKUS_OTEL_EXPORTER_OTLP_ENDPOINT`).

---

### .NET (ASP.NET Core + OpenTelemetry SDK)

Works with NativeAOT (`PublishAot=true`). Requires adding NuGet packages and ~15 lines in `Program.cs`.

```xml
<PackageReference Include="OpenTelemetry.Extensions.Hosting" Version="1.12.0" />
<PackageReference Include="OpenTelemetry.Instrumentation.AspNetCore" Version="1.12.0" />
<PackageReference Include="OpenTelemetry.Instrumentation.Http" Version="1.12.0" />
<PackageReference Include="OpenTelemetry.Exporter.OpenTelemetryProtocol" Version="1.12.0" />
```

For .NET 8 projects, use version 1.9.0 instead of 1.12.0 to avoid dependency conflicts.

```csharp
var otlpEndpoint = Environment.GetEnvironmentVariable("OTLP_GRPC_ENDPOINT");
if (!string.IsNullOrEmpty(otlpEndpoint))
{
    builder.Services.AddOpenTelemetry()
        .ConfigureResource(r => r.AddService("my-service"))
        .WithTracing(tracing => tracing
            .AddAspNetCoreInstrumentation()
            .AddHttpClientInstrumentation()
            .AddOtlpExporter(o =>
            {
                o.Endpoint = new Uri(otlpEndpoint);
                o.Protocol = OpenTelemetry.Exporter.OtlpExportProtocol.Grpc;
            }));
}
```

For SQL query detection, add the instrumentation that matches your database access layer:

- **Entity Framework Core** (MySQL, PostgreSQL, SQLite): `.AddEntityFrameworkCoreInstrumentation(o => o.SetDbStatementForText = true)` with `OpenTelemetry.Instrumentation.EntityFrameworkCore`
- **SqlClient** (SQL Server): `.AddSqlClientInstrumentation(o => o.SetDbStatementForText = true)` with `OpenTelemetry.Instrumentation.SqlClient`

The `SetDbStatementForText = true` option is required for perf-sentinel to see the query text. Without it, SQL spans are emitted but `db.statement` is empty.

Note: Entity Framework Core uses named bind parameters (`@__param_0`). Since the actual parameter values are not visible in the query template, perf-sentinel may detect repeated queries as `redundant_sql` (same template, same visible params) rather than `n_plus_one_sql` (same template, different params).

---

### Rust (tracing + opentelemetry-otlp)

Requires adding 4 crates and ~20 lines of initialization code. Use `provider.tracer()` (not `global::tracer()`) to avoid the `PreSampledTracer` trait bound issue.

```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "registry"] }
tracing-opentelemetry = "0.31"
opentelemetry = { version = "0.30", features = ["trace"] }
opentelemetry_sdk = { version = "0.30", features = ["rt-tokio", "trace"] }
opentelemetry-otlp = { version = "0.30", features = ["grpc-tonic"] }
```

```rust
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

let exporter = opentelemetry_otlp::SpanExporter::builder()
    .with_tonic()
    .with_endpoint("http://127.0.0.1:4317")
    .build()
    .expect("failed to create OTLP exporter");

let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
    .with_batch_exporter(exporter)
    .build();

let tracer = provider.tracer("my-service");
let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

tracing_subscriber::registry()
    .with(tracing_subscriber::fmt::layer())
    .with(otel_layer)
    .init();
```

For perf-sentinel to detect SQL anti-patterns, add `db.statement` to your query spans manually:

```rust
let _span = tracing::info_span!("db.query",
    db.statement = "SELECT * FROM player WHERE game_id = 42",
    db.system = "mysql"
);
```

---

