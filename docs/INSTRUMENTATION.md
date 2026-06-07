# perf-sentinel instrumentation guide

This guide covers the parts of the data pipeline that turn an application's runtime activity into the OTLP / JSON input perf-sentinel consumes. For an end-to-end overview, the four supported topologies and the four quick starts, see [INTEGRATION.md](./INTEGRATION.md). For the CI-side of the integration (CI mode, GitHub Actions / GitLab CI / Jenkins recipes, interactive HTML report deployment, PR regression detection), see [CI.md](./CI.md).

## Contents

- [Kubernetes deployment](#kubernetes-deployment): manifests for the daemon and the OTel Collector sidecar.
- [Cloud provider integrations](#cloud-provider-integrations): AWS X-Ray, GCP Cloud Trace, Azure Application Insights, self-hosted Jaeger / Tempo / Zipkin.
- [Production: via OpenTelemetry Collector](#production-via-opentelemetry-collector): central collector setup, sampling and detection accuracy.
- [Required span attributes](#required-span-attributes): the legacy and stable OTel semantic conventions perf-sentinel reads.
- [Dev/staging: per-language instrumentation](#devstaging-per-language-instrumentation):
  - Java
    - [Spring Boot, Helidon 4.x](#java-opentelemetry-java-agent-v227-spring-boot-helidon-4x)
    - [Quarkus 3.33 LTS](#java-quarkus-333-lts--quarkus-opentelemetry--otel-agent-v227)
  - [.NET (ASP.NET Core + Entity Framework Core)](#net-aspnet-core--entity-framework-core--opentelemetry-sdk-115)
  - [Go (pgx)](#go-otelhttp-068--otelpgx-011-otel-sdk-143)
  - Python
    - [Django + psycopg](#python-django-5x--psycopg-otel-sdk-142)
    - [FastAPI + SQLAlchemy + asyncpg](#python-fastapi--sqlalchemy-2x--asyncpg-otel-sdk-142)
  - [Node.js (Nest.js + Prisma)](#nodejs-nestjs--prisma-otel-sdk-0218)
  - [Rust (Diesel, SeaORM)](#rust-tracing-opentelemetry-033-diesel-seaorm)
- [SQL placeholder styles and detection](#sql-placeholder-styles-and-detection): how perf-sentinel maps each instrumentation's SQL placeholder to the sanitizer-aware N+1 detection path.

## Background: OpenTelemetry primer

If you have not used OpenTelemetry before, this short primer is a prerequisite for the rest of this guide. It assumes you know what an HTTP request and a database query are. It does not assume you have ever instrumented an application or run a tracing backend. Other perf-sentinel docs cross-reference this primer for OTel concepts, see [docs/INTEGRATION.md](INTEGRATION.md) and [docs/HELM-DEPLOYMENT.md](HELM-DEPLOYMENT.md#observability).

**What is OpenTelemetry.** OpenTelemetry (often shortened to "OTel") is a Cloud Native Computing Foundation (CNCF) project that defines an open standard for collecting telemetry data (traces, metrics, logs) from any kind of software. It is the merger of two earlier projects (OpenTracing and OpenCensus) consolidated in 2019, governed under CNCF since. The two practical things OTel gives you:

- **A protocol** (OTLP, OpenTelemetry Protocol) that any application can use to ship traces and metrics to any backend that speaks it. OTLP is wire-format-stable, ships in both gRPC and HTTP+protobuf variants, and is what perf-sentinel ingests on ports 4317 (gRPC) and 4318 (HTTP).
- **SDKs** (Java, Python, Go, .NET, Rust, JavaScript, ...) that handle the boring parts: capturing each HTTP/SQL call as a *span*, propagating the trace ID across services, batching, retrying, and sending OTLP. Most language SDKs include auto-instrumentation for popular frameworks (Spring, Quarkus, ASP.NET Core, Django, Express) so the application code itself rarely changes.

**Key concepts.**

- A **span** is a unit of work, typically one HTTP request or one SQL query. It carries a duration, a status, a name (`GET /api/orders`), and a structured attribute bag.
- A **trace** is the tree of spans that share a `trace_id`. A single user request typically crosses several services, each producing several spans, all linked by the same `trace_id`.
- **Semantic conventions** are the OTel-defined attribute names so different SDKs all emit the same field for the same concept. `http.request.method` is always the HTTP verb, `db.system` is always the database engine name, and so on. perf-sentinel reads a small subset of these attributes to detect anti-patterns. The closed list of attributes perf-sentinel reads is in [Required span attributes](#required-span-attributes) below.

**The Collector.** A separate process, the **OpenTelemetry Collector**, is the recommended deployment shape between applications and backends. It receives OTLP from a fleet of applications, applies optional sampling and attribute processing, and forwards to one or more backends in parallel (perf-sentinel, plus Tempo or Jaeger for storage, plus Prometheus exemplars). Running a central Collector decouples the applications from each backend's quirks and lets operators change sampling policy without touching application code. The relevant deployment shapes are covered in [Production: via OpenTelemetry Collector](#production-via-opentelemetry-collector) below.

**Where to learn more.** [opentelemetry.io](https://opentelemetry.io/), [OTLP spec](https://github.com/open-telemetry/opentelemetry-proto), [semantic conventions](https://opentelemetry.io/docs/specs/semconv/).

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

> **Silent skip.** A span dropped for a missing carrying attribute
> produces no warning and no error. A SQL span without `db.statement` /
> `db.query.text`, or an HTTP span without `http.url` / `url.full`,
> simply yields no finding. A thin or empty report can therefore mean
> *no problems* or *no usable instrumentation*. Run
> `perf-sentinel inspect` to see what was actually extracted, and see
> [Instrumentation quality bounds findings](./LIMITATIONS.md#instrumentation-quality-bounds-findings).

> **`http.route` is load-bearing for ack stability.** The acknowledgment
> signature is keyed on the route template, not the instantiated URL.
> Services that emit `http.route` (Spring Boot, ASP.NET Core, Express,
> any modern auto-instrumentation) get acks that survive restarts and
> rotating request ids. Services that fall back to `http.url` /
> `url.full` lose that stability. See
> [`ACK-WORKFLOW.md`](./ACK-WORKFLOW.md#signature-stability-and-service-restarts)
> for the verification recipe.

---

## Dev/staging: per-language instrumentation

When no OTel Collector is available, instrument services directly. The guides below are ordered from easiest to most involved.

### Java (OpenTelemetry Java Agent v2.27+, Spring Boot, Helidon 4.x)

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

**R2DBC and SQL placeholder handling.** R2DBC drivers use database-native bind markers (`$1`, `$2` for PostgreSQL, `?` for MySQL/MariaDB). The Java Agent's built-in statement sanitizer replaces all literals with bare `?` before setting `db.statement`, regardless of the underlying driver. This means perf-sentinel receives `?`-style sanitized templates with empty params for both JDBC and R2DBC stacks. Without the agent (R2DBC SDK only, no auto-instrumentation), `db.statement` would contain the native `$1`/`$2` markers, which perf-sentinel also handles (the SQL normalizer recognizes `$N` as a placeholder since v0.7.7). Either way, the sanitizer-aware N+1 detection path fires correctly.

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

### Java (Quarkus 3.33 LTS + quarkus-opentelemetry + OTel Agent v2.27)

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

### .NET (ASP.NET Core + Entity Framework Core + OpenTelemetry SDK 1.15)

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

Note: `System.Net.Http` redacts the query string to `?*` by default, so outbound HTTP N+1 loops that vary a query parameter (`?seq=1`, `?seq=2`, ...) reach perf-sentinel as identical URLs and are detected as `redundant_http` rather than `n_plus_one_http`. To get `n_plus_one_http` on these loops, set `OTEL_DOTNET_EXPERIMENTAL_HTTPCLIENT_DISABLE_URL_QUERY_REDACTION=true` so the query survives, or model the varying identifier as a path segment (`/api/resource/{id}`). See [LIMITATIONS.md](./LIMITATIONS.md#http-query-string-redaction-and-n1-visibility) for the full rationale.

---

### Go (otelhttp 0.68 + otelpgx 0.11, OTel SDK 1.43)

The Go OTel SDK uses explicit wrapping rather than auto-instrumentation. HTTP and SQL each need a dedicated library.

**Dependencies (go.mod):**

```
go.opentelemetry.io/otel
go.opentelemetry.io/otel/sdk
go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc
go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp
github.com/exaring/otelpgx
```

**HTTP server instrumentation:**

```go
mux := http.NewServeMux()
mux.HandleFunc("/api/orders", handleOrders)
// Wrap the mux with OTel HTTP middleware
handler := otelhttp.NewHandler(mux, "server",
    otelhttp.WithSpanNameFormatter(func(_ string, r *http.Request) string {
        return r.Method + " " + r.URL.Path
    }),
)
http.ListenAndServe(":8080", handler)
```

**SQL instrumentation with pgx:**

```go
cfg, _ := pgxpool.ParseConfig(os.Getenv("DB_DSN"))
cfg.ConnConfig.Tracer = otelpgx.NewTracer()
pool, _ := pgxpool.NewWithConfig(ctx, cfg)
```

`otelpgx` emits `db.statement` with PostgreSQL native positional parameters (`$1`, `$2`). perf-sentinel normalizes these to `$?` with empty `params`, which enables the sanitizer-aware N+1 detection path. No additional configuration is needed.

**Environment variables (Docker Compose example):**

```yaml
environment:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4318
  OTEL_EXPORTER_OTLP_PROTOCOL: http/protobuf
  OTEL_SERVICE_NAME: go-svc
```

---

### Python (Django 5.x + psycopg, OTel SDK 1.42)

Django applications use the auto-instrumentation packages for both HTTP and SQL.

**Dependencies (requirements.txt):**

```
opentelemetry-sdk
opentelemetry-exporter-otlp-proto-grpc
opentelemetry-instrumentation-django
opentelemetry-instrumentation-psycopg
```

**Initialization (manage.py or wsgi.py):**

```python
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.django import DjangoInstrumentor
from opentelemetry.instrumentation.psycopg import PsycopgInstrumentor

provider = TracerProvider()
provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))

DjangoInstrumentor().instrument()
PsycopgInstrumentor().instrument()
```

`psycopg` emits `db.statement` with Python DB-API `%s` placeholders. perf-sentinel recognizes `%s` as a driver placeholder, so the sanitizer-aware N+1 detection path fires without additional configuration.

**Environment variables:**

```yaml
environment:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4317
  OTEL_SERVICE_NAME: django-svc
```

---

### Python (FastAPI + SQLAlchemy 2.x + asyncpg, OTel SDK 1.42)

FastAPI with SQLAlchemy uses the auto-instrumentation packages. SQLAlchemy is in the ORM scope allow-list, so the sanitizer-aware detection path recognizes it as an ORM-driven stack.

**Dependencies (requirements.txt):**

```
opentelemetry-sdk
opentelemetry-exporter-otlp-proto-grpc
opentelemetry-instrumentation-fastapi
opentelemetry-instrumentation-sqlalchemy
opentelemetry-instrumentation-asyncpg
```

**Initialization (main.py):**

```python
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
from opentelemetry.instrumentation.sqlalchemy import SQLAlchemyInstrumentor

provider = TracerProvider()
provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))

FastAPIInstrumentor.instrument_app(app)
SQLAlchemyInstrumentor().instrument(engine=engine)
```

`asyncpg` emits `db.statement` with PostgreSQL native positional parameters (`$1`, `$2`). perf-sentinel normalizes these to `$?` with empty `params`. The `sqlalchemy` instrumentation scope is in the ORM scope allow-list, so the sanitizer-aware N+1 detection fires via the ORM path for this stack.

**Environment variables:**

```yaml
environment:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4317
  OTEL_SERVICE_NAME: fastapi-svc
```

---

### Node.js (Nest.js + Prisma, OTel SDK 0.218)

Nest.js applications use the `@opentelemetry/sdk-node` package with framework-specific instrumentations. Prisma generates SQL, the `pg` client sends it.

**Dependencies (package.json):**

```json
{
  "@opentelemetry/sdk-node": "^0.57",
  "@opentelemetry/exporter-trace-otlp-grpc": "^0.57",
  "@opentelemetry/instrumentation-http": "^0.57",
  "@opentelemetry/instrumentation-pg": "^0.44"
}
```

**Initialization (tracing.ts, loaded via --require):**

```typescript
import { NodeSDK } from '@opentelemetry/sdk-node';
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-grpc';
import { HttpInstrumentation } from '@opentelemetry/instrumentation-http';
import { PgInstrumentation } from '@opentelemetry/instrumentation-pg';

const sdk = new NodeSDK({
  traceExporter: new OTLPTraceExporter(),
  instrumentations: [
    new HttpInstrumentation(),
    new PgInstrumentation({ enhancedDatabaseReporting: true }),
  ],
});
sdk.start();
```

`PgInstrumentation` with `enhancedDatabaseReporting: true` emits `db.statement` with the full SQL query, including resolved parameter values. The `prisma` instrumentation scope is in the ORM scope allow-list, so the sanitizer-aware detection fires via the ORM path.

**Environment variables:**

```yaml
environment:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4317
  OTEL_SERVICE_NAME: nest-svc
  NODE_OPTIONS: --require ./tracing.js
```

---

### Rust (tracing-opentelemetry 0.33, Diesel, SeaORM)

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

For Rust applications using Diesel or SeaORM, the ORM crate emits SQL directly to the `tracing` span. Add `db.statement` and `db.system` to your query spans manually or via the ORM's tracing integration. Both `diesel` and `sea-orm` are in the ORM scope allow-list.

```rust
let _span = tracing::info_span!("db.query",
    db.statement = "SELECT * FROM player WHERE game_id = 42",
    db.system = "postgresql"
);
```

---

## SQL placeholder styles and detection

Different database drivers emit different placeholder syntax in the `db.statement` span attribute. perf-sentinel's SQL normalizer recognizes all common styles and maps them to `$?` or `?` in the normalized template, with `params` kept empty for parameterized queries. This is what enables the sanitizer-aware N+1 detection path (which requires `params == []` and a recognized placeholder in the template).

| Placeholder    | Produced by                                                                                                             | Normalized to  | Example           |
|----------------|-------------------------------------------------------------------------------------------------------------------------|----------------|-------------------|
| `?`            | JDBC agent (Java), R2DBC via Java Agent, MySQL Connector/J 8.2+ native OTel, Go `go-sql-driver/mysql`, Node.js `mysql2` | `?`            | `WHERE id = ?`    |
| `$1`, `$2`     | PostgreSQL native (pgx, asyncpg, sqlx, node-pg)                                                                         | `$?`           | `WHERE id = $?`   |
| `%s`           | Python DB-API (psycopg, MySQLdb, PyMySQL, mysql-connector-python)                                                       | `%s` (kept)    | `WHERE id = %s`   |
| `@p0`, `@Name` | .NET (Npgsql, SqlClient, MySqlConnector/Pomelo)                                                                         | `@p0` (kept)   | `WHERE id = @p0`  |
| `:name`        | Oracle, SQLAlchemy named                                                                                                | `:name` (kept) | `WHERE id = :oid` |

**What this means for operators.** No configuration is needed to enable detection for any of these stacks. The normalizer and the `template_has_placeholder` check in the detection pipeline handle the mapping automatically. The key requirement is that the OTel instrumentation emits `db.statement` on SQL spans. If `db.statement` is missing (some instrumentations omit it by default for security reasons), perf-sentinel cannot detect SQL anti-patterns. Check your instrumentation library's documentation for how to enable statement capture.

**ORM scope markers.** The sanitizer-aware detection path also consults the OTel instrumentation scope (the library name) to decide whether a group of sanitized queries is likely N+1 or just redundant. The following scopes are recognized as ORM-level instrumentations, which raises the confidence that a repeated parameterized query is a loop iteration rather than a cache-warm pattern:

`spring-data`, `hibernate`, `jpa`, `micronaut-data`, `jdbi`, `r2dbc`, `entityframeworkcore`, `entity-framework`, `sqlalchemy`, `django`, `active-record`, `activerecord`, `gorm`, `sequelize`, `prisma`, `typeorm`, `mongoose`, `sea-orm`, `diesel`.

Stacks without an ORM scope (bare driver: `otelpgx`, `asyncpg`, `node-pg`, `psycopg` without Django/SQLAlchemy) rely on the timing-variance and high-occurrence signals instead. See `docs/design/04-DETECTION.md` for the full classification algorithm.

