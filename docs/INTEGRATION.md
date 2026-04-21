# Integration guide

perf-sentinel accepts OpenTelemetry traces via OTLP (gRPC on port 4317, HTTP on port 4318). This guide walks you from zero to your first finding for each deployment topology.

## Choose your topology

| Topology                                                | Best for                          | Effort | Changes to services     |
|---------------------------------------------------------|-----------------------------------|--------|-------------------------|
| **[CI batch](#quick-start-ci-batch-analysis)**          | CI pipelines, pull request checks | Lowest | None (uses trace files) |
| **[Central collector](#quick-start-central-collector)** | Production, multi-service         | Low    | None (YAML config only) |
| **[Sidecar](#quick-start-sidecar)**                     | Dev/staging, single-service debug | Low    | None (Docker only)      |
| **[Direct daemon](#quick-start-direct-daemon)**         | Local dev, quick experiments      | Medium | Per-language env vars   |

---

## Quick start: CI batch analysis

**Use case:** run perf-sentinel in your CI pipeline to catch N+1 queries before they reach production. No daemon, no Docker, just a binary that reads a trace file and exits with code 1 if the quality gate fails.

### Step 1: Install

```bash
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64
sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel
```

### Step 2: Configure thresholds

Create `.perf-sentinel.toml` at your project root:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # zero tolerance for N+1 SQL
io_waste_ratio_max = 0.30          # max 30% avoidable I/O

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500

[green]
enabled = true
default_region = "eu-west-3"       # optional: enables gCO2eq estimates
# per-service overrides for multi-region deployments
# [green.service_regions]
# "api-us"   = "us-east-1"
# "api-asia" = "ap-southeast-1"
```

> CO₂ output is structured: `green_summary.co2.total.{low,mid,high}` plus an SCI v1.0 methodology tag, with a 2× multiplicative uncertainty interval (`low = mid/2`, `high = mid×2`). Multi-region scoring is automatic when OTel spans carry the `cloud.region` attribute. See `docs/CONFIGURATION.md` and `docs/LIMITATIONS.md#carbon-estimates-accuracy` for details.

### Step 3: Collect traces

Export traces from your integration tests. If your tests run with OTel instrumentation, save the output to a JSON file. You can also export from Jaeger UI or Zipkin UI, perf-sentinel auto-detects the format.

### Step 4: Analyze

```bash
perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
```

The process prints a JSON report to stdout and exits with code 0 (pass) or 1 (fail). Add this to your CI job:

```yaml
# GitLab CI example
perf:sentinel:
  stage: quality
  script:
    - perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
  artifacts:
    paths: [perf-sentinel-report.json]
    when: always
  allow_failure: true   # start with warning-only, remove once thresholds are calibrated
```

### Step 5: Investigate findings

```bash
# Colored terminal report
perf-sentinel analyze --input traces.json --config .perf-sentinel.toml

# Tree view of a specific trace
perf-sentinel explain --input traces.json --trace-id <trace-id>

# Interactive TUI
perf-sentinel inspect --input traces.json

# SARIF for GitHub/GitLab code scanning
perf-sentinel analyze --input traces.json --format sarif > results.sarif

# Single-file HTML dashboard for post-mortem browser exploration
perf-sentinel report --input traces.json --output report.html
```

---

### HTML dashboard report

`perf-sentinel report --input traces.json --output report.html` produces a single-file HTML dashboard. Double-click to open in any browser, works offline, no external resources. Target audience: developers exploring a CI artifact who prefer clicking over typing. The dashboard shows findings, trace trees and `GreenOps` metrics with cross-navigation between them (click a finding to see its trace tree, with the offending span highlighted in the tree view).

Flags:
- `--input <FILE>` or `--input -`: trace file or stdin (same format auto-detection as `analyze`: native JSON, Jaeger, Zipkin v2).
- `--output <FILE>`: required, overwritten if it already exists.
- `--config <PATH>`: optional `.perf-sentinel.toml`, same semantics as `analyze --config`.
- `--max-traces-embedded <N>`: cap on embedded Explain traces. When unset, the sink trims lowest-IIS traces to target a ~5 MB HTML file size. A banner in the Findings tab surfaces the trim ratio when it kicks in.
- `--pg-stat <FILE>`: cross-reference a `pg_stat_statements` CSV or JSON export. Enables a pg_stat tab and the Explain-to-pg_stat cross-navigation for SQL spans whose normalized template matches a pg_stat row.
- `--pg-stat-prometheus <URL>`: one-shot HTTP GET against a `postgres_exporter` instance, same effect as `--pg-stat` without the intermediate file. Mutually exclusive with `--pg-stat`.
- `--before <FILE>`: baseline report JSON (the output of `analyze --format json`). Enables a Diff tab showing new findings, resolved findings, severity changes, and per-endpoint I/O deltas relative to the baseline.

Exit codes differ from `analyze --ci`: `report` always exits 0, even when the quality gate fails. The gate status is rendered as a badge in the HTML top bar. Use `analyze --ci` when you need the CI exit-code signal.

Example invocations:

```bash
# Basic post-mortem report
perf-sentinel report --input traces.json --output report.html

# With SQL hotspot cross-reference
perf-sentinel report --input traces.json \
    --pg-stat pg_stat_statements.csv \
    --output report.html

# With scraped Prometheus hotspots instead of a file
perf-sentinel report --input traces.json \
    --pg-stat-prometheus http://prometheus:9090 \
    --output report.html

# PR regression view: diff against a baseline run
perf-sentinel report --input after.json \
    --before before.json \
    --output report.html

# Everything at once
perf-sentinel report --input traces.json \
    --pg-stat pg_stat_statements.csv \
    --before baseline.json \
    --output report.html
```

Keyboard inside the dashboard: `j`/`k` move the Findings selection, `enter` opens the current finding in Explain, `esc` backs out of Explain (or closes the search bar if one is open). `/` opens a substring filter on the active tab, scoped to Findings, pg_stat, Diff or Correlations.

This is a post-mortem view over a completed trace set. For live inspection of a running daemon, use `perf-sentinel query inspect` (TUI) or the `/api/*` endpoints directly. Tempo-backed workflows compose via the shell: `perf-sentinel tempo --endpoint http://tempo:3200 --search "..." --output traces.json && perf-sentinel report --input traces.json --output report.html`.

---

## Quick start: central collector

**Use case:** production deployment where services already send traces to an OpenTelemetry Collector (or you want to add one). Zero code changes, just YAML configuration.

### Step 1: Start perf-sentinel + collector

```bash
docker compose -f examples/docker-compose-collector.yml up -d
```

This starts:
- An **OTel Collector** listening on ports 4317 (gRPC) and 4318 (HTTP)
- **perf-sentinel** in watch mode, receiving traces from the collector

### Step 2: Point your services at the collector

Set these environment variables in your application containers:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

If your services already export to a collector, add perf-sentinel as an additional exporter in your existing `otel-collector-config.yaml`:

```yaml
exporters:
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      exporters: [otlp/perf-sentinel, otlp/your-existing-backend]
```

### Step 3: Generate traffic

Use your application normally. After the trace TTL expires (default 30 seconds), perf-sentinel emits findings as NDJSON to stdout:

```bash
docker compose -f examples/docker-compose-collector.yml logs -f perf-sentinel
```

### Step 4: Monitor with Prometheus + Grafana

perf-sentinel exposes Prometheus metrics at `http://localhost:14318/metrics` with OpenMetrics exemplars (click-through from Grafana to your trace backend):

```bash
curl -s http://localhost:14318/metrics | grep perf_sentinel
```

Add it as a Prometheus scrape target:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: perf-sentinel
    static_configs:
      - targets: ['perf-sentinel:4318']
```

Key metrics:
- `perf_sentinel_findings_total{type, severity}`: findings with exemplar `trace_id` for click-through
- `perf_sentinel_io_waste_ratio`: current I/O waste ratio with exemplar `trace_id`
- `perf_sentinel_events_processed_total`: total spans ingested
- `perf_sentinel_traces_analyzed_total`: total traces completed
- `perf_sentinel_slow_duration_seconds{type}`: histogram of slow span durations (use `histogram_quantile()` for global percentiles across sharded instances)

See [`examples/otel-collector-config.yaml`](../examples/otel-collector-config.yaml) for the full config with sampling and filtering options.

---

## Quick start: sidecar

**Use case:** debug a single service in dev/staging. perf-sentinel runs alongside the service, sharing its network namespace.

### Step 1: Start the sidecar

```bash
docker compose -f examples/docker-compose-sidecar.yml up -d
```

### Step 2: Configure your app

Your app sends traces to `localhost:4318` (HTTP), no network hop since perf-sentinel shares the same network namespace:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

### Step 3: View findings

```bash
docker compose -f examples/docker-compose-sidecar.yml logs -f perf-sentinel
```

See [`examples/docker-compose-sidecar.yml`](../examples/docker-compose-sidecar.yml) for the full configuration.

---

## Quick start: direct daemon

**Use case:** local development. Run perf-sentinel on your host machine and point services at it.

### Step 1: Start the daemon

```bash
perf-sentinel watch
```

By default, it listens on `127.0.0.1:4317` (gRPC) and `127.0.0.1:4318` (HTTP). For Docker containers to reach the host, use:

```toml
# .perf-sentinel.toml
[daemon]
listen_address = "0.0.0.0"
```

### Step 2: Instrument your service

Set the OTLP endpoint in your service (see [per-language guides](#devstaging-per-language-instrumentation) below):

```bash
# For services running on the host
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317

# For services running in Docker
OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4317
```

### Step 3: View findings

Findings stream to stdout as NDJSON. Prometheus metrics are available at `http://localhost:4318/metrics`.

---

## Kubernetes deployment

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

> **Note:** tail-based sampling requires the `otel/opentelemetry-collector-contrib` image (not the core image). Sampling below 100% will cause perf-sentinel to miss some anti-patterns in un-sampled traces.

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

## CI mode (batch analysis)

For CI pipelines, use batch mode instead of daemon mode:

```bash
perf-sentinel analyze --ci --input traces.json
```

Exit code is non-zero if the quality gate fails. Configure thresholds in `.perf-sentinel.toml`:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30
```

---

## CI integration recipes

Ready-to-copy templates for the three major CI providers live in
[`docs/ci-templates/`](./ci-templates/). Pick the one that matches your
provider, drop it into your repository, adapt the three variables called out
in the template's leading comment block (version pin, trace path, config
path) and you are done.

| Provider       | Template                                                  | What it surfaces                                  |
|----------------|-----------------------------------------------------------|---------------------------------------------------|
| GitHub Actions | [`github-actions.yml`](./ci-templates/github-actions.yml) | SARIF in GitHub Code Scanning + sticky PR comment |
| GitLab CI      | [`gitlab-ci.yml`](./ci-templates/gitlab-ci.yml)           | SARIF artifact + Code Quality widget on the MR    |
| Jenkins        | [`jenkinsfile.groovy`](./ci-templates/jenkinsfile.groovy) | Warnings Next Generation issue tree + trend chart |

### Quality-gate philosophy

All three templates run `perf-sentinel analyze --ci` as the gating step. The
`--ci` flag does one thing: when any threshold defined in
`[thresholds]` of `.perf-sentinel.toml` is breached, the process exits with
code `1`. The CI provider then turns that into a red build, which is the
signal you want on a pull request.

The recommended setup runs perf-sentinel **twice** in the same job: once
without `--ci` to always produce a SARIF artifact (so reviewers can inspect
findings even when the gate fails) and once with `--ci` to enforce the
gate. The Jenkins and GitLab templates do this explicitly. The GitHub
template uses `continue-on-error` to achieve the same effect in a single
invocation.

### Where SARIF surfaces in each provider

- **GitHub Code Scanning** lists each finding under the Security tab of the
  repository, with inline source annotations on the PR diff when the
  `code_location` field is present. Requires `permissions.security-events:
  write` on the workflow.
- **GitLab Code Quality** widget shows up on the merge request page, with
  severity colors derived from the perf-sentinel `severity` field
  (`critical -> critical`, `warning -> major`, `info -> info`).
- **Jenkins Warnings Next Generation** publishes a structured issue tree
  with a trend chart per build. The plugin natively understands SARIF
  v2.1.0 and supports its own `qualityGates` declaration as a defense in
  depth on top of the perf-sentinel `--ci` exit code.

For a worked Spring Boot + Maven + Jenkins example with the
`kinexoPipeline` shared library, see
[`ENTERPRISE-JAVA-INTEGRATION-FR.md`](../ENTERPRISE-JAVA-INTEGRATION-FR.md)
(French only, for now).

---

## Ingestion formats

perf-sentinel auto-detects the input format when using `perf-sentinel analyze --input`:

| Format                          | Detection                                             | Example                 |
|---------------------------------|-------------------------------------------------------|-------------------------|
| **Native** (perf-sentinel JSON) | Array of objects with `"type"` field                  | Default format          |
| **Jaeger JSON**                 | Object with `"data"` key containing `"spans"`         | Exported from Jaeger UI |
| **Zipkin JSON v2**              | Array of objects with `"traceId"` + `"localEndpoint"` | Exported from Zipkin UI |

No `--format` flag is needed for input: the format is detected automatically from the first few bytes of the file.

```bash
# Jaeger export
perf-sentinel analyze --input jaeger-export.json --ci

# Zipkin export
perf-sentinel analyze --input zipkin-traces.json --ci
```

## Explain mode

To debug a specific trace, use the `explain` subcommand:

```bash
perf-sentinel explain --input traces.json --trace-id abc123-def456
```

This produces a tree view of the trace with findings annotated inline. Use `--format json` for structured output.

## SARIF export

For GitHub or GitLab code scanning integration, export findings as SARIF v2.1.0:

```bash
perf-sentinel analyze --input traces.json --format sarif > results.sarif
```

Upload the SARIF file to your code scanning dashboard. Each finding maps to a SARIF result with `ruleId`, `level`, `logicalLocations` (service + endpoint), a custom `properties.confidence` tag and a standard SARIF `rank` value (0-100) derived from the confidence.

## Finding confidence field

Every finding emitted in JSON or SARIF carries a `confidence` field indicating the source context of the detection. Downstream consumers, in particular [perf-lint](https://github.com/robintra/perf-lint), use this field to boost or reduce the severity shown in the IDE depending on how much trust to place in the finding.

Values:

| Value                 | When emitted                                                            | SARIF `rank` | Interpretation                                                                      |
|-----------------------|-------------------------------------------------------------------------|--------------|-------------------------------------------------------------------------------------|
| `"ci_batch"`          | `perf-sentinel analyze` (batch mode, always)                            | `30`         | Low confidence: the trace came from a controlled CI run with limited traffic shapes |
| `"daemon_staging"`    | `perf-sentinel watch` with `[daemon] environment = "staging"` (default) | `60`         | Medium confidence: real traffic patterns observed on a staging deployment           |
| `"daemon_production"` | `perf-sentinel watch` with `[daemon] environment = "production"`        | `90`         | Highest confidence: real traffic, real scale, real users                            |

**Example JSON finding:**

```json
{
  "type": "n_plus_one_sql",
  "severity": "warning",
  "trace_id": "abc123",
  "service": "order-svc",
  "source_endpoint": "POST /api/orders/{id}/submit",
  "pattern": { "template": "SELECT * FROM order_item WHERE order_id = ?", "occurrences": 6, "window_ms": 250, "distinct_params": 6 },
  "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
  "first_timestamp": "2026-04-08T03:14:01.050Z",
  "last_timestamp": "2026-04-08T03:14:01.300Z",
  "confidence": "daemon_production"
}
```

**Example SARIF result fragment:**

```json
{
  "ruleId": "n_plus_one_sql",
  "level": "warning",
  "message": { "text": "n_plus_one_sql in order-svc on POST /api/orders/{id}/submit..." },
  "properties": { "confidence": "daemon_production" },
  "rank": 90
}
```

**How to configure the value in the daemon:**

```toml
[daemon]
# "staging" (default) → confidence = daemon_staging, rank = 60
# "production"        → confidence = daemon_production, rank = 90
environment = "production"
```

The value is stamped on every finding emitted by that daemon instance. Invalid values (anything other than `staging`/`production`, case-insensitive) are rejected at config load with a clear error. Batch `analyze` mode ignores this field and always emits `ci_batch`.

**perf-lint interop.** perf-lint reads the `confidence` field on imported runtime findings and applies a severity multiplier: `ci_batch` findings are shown as hints, `daemon_staging` as warnings, `daemon_production` as errors. This way a finding that has been observed on real production traffic surfaces louder in the IDE than one observed only in a CI fixture.

---

## PR regression detection (`diff` subcommand)

The `diff` subcommand compares two trace sets and emits a delta report listing new findings, resolved findings, severity changes and per-endpoint I/O op count deltas. The natural fit is a PR check that compares the PR branch's traces against the base branch's traces.

```yaml
# .github/workflows/perf-sentinel-diff.yml
name: perf-sentinel diff

on:
  pull_request:
    branches: [main]

permissions:
  contents: read
  pull-requests: write

jobs:
  diff:
    runs-on: ubuntu-latest
    env:
      PERF_SENTINEL_VERSION: "0.4.8"
    steps:
      - uses: actions/checkout@b4ffde65f46336ab88eb53be808477a3936bae11 # v4.1.1
        with:
          fetch-depth: 0

      - name: Install perf-sentinel
        run: |
          set -euo pipefail
          BASE_URL="https://github.com/robintra/perf-sentinel/releases/download/v${PERF_SENTINEL_VERSION}"
          curl -sSLf -o perf-sentinel-linux-amd64 "${BASE_URL}/perf-sentinel-linux-amd64"
          curl -sSLf -o SHA256SUMS.txt            "${BASE_URL}/SHA256SUMS.txt"
          grep 'perf-sentinel-linux-amd64' SHA256SUMS.txt | sha256sum -c -
          mkdir -p "${GITHUB_WORKSPACE}/bin"
          install -m 0755 perf-sentinel-linux-amd64 "${GITHUB_WORKSPACE}/bin/perf-sentinel"
          echo "${GITHUB_WORKSPACE}/bin" >> "${GITHUB_PATH}"

      # Run integration tests on the PR branch and capture traces.
      - name: Collect PR-branch traces
        run: ./scripts/run-integration-tests.sh
        env:
          OTEL_EXPORTER_OTLP_FILE_PATH: pr-traces.json

      # Re-run on the base branch.
      - name: Collect base-branch traces
        run: |
          git checkout ${{ github.event.pull_request.base.sha }} -- .
          ./scripts/run-integration-tests.sh
        env:
          OTEL_EXPORTER_OTLP_FILE_PATH: base-traces.json

      - name: Diff
        run: |
          perf-sentinel diff \
            --before base-traces.json \
            --after pr-traces.json \
            --config .perf-sentinel.toml \
            --format json \
            --output diff.json
          # SARIF for GitHub Code Scanning (only new findings).
          perf-sentinel diff \
            --before base-traces.json \
            --after pr-traces.json \
            --config .perf-sentinel.toml \
            --format sarif \
            --output diff.sarif

      - name: Upload SARIF
        if: hashFiles('diff.sarif') != ''
        uses: github/codeql-action/upload-sarif@4dd16135b69a43b6c8efb853346f8437d92d3c93 # v3.26.6
        with:
          sarif_file: diff.sarif
          category: perf-sentinel-diff

      - name: Comment regression summary on PR
        run: |
          NEW=$(jq '.new_findings | length' diff.json)
          RESOLVED=$(jq '.resolved_findings | length' diff.json)
          REGRESSIONS=$(jq '[.severity_changes[] | select(.after_severity == "critical" or (.after_severity == "warning" and .before_severity == "info"))] | length' diff.json)
          {
            echo "## perf-sentinel diff vs base"
            echo
            echo "- $NEW new finding(s)"
            echo "- $RESOLVED resolved finding(s)"
            echo "- $REGRESSIONS severity regression(s)"
          } > pr-comment.md

      - uses: marocchino/sticky-pull-request-comment@331f8f5b4215f0445d3c07b4967662a32a2d3e31 # v2.9.0
        with:
          header: perf-sentinel-diff
          path: pr-comment.md

      - name: Fail on regression
        run: |
          NEW=$(jq '.new_findings | length' diff.json)
          REGRESSIONS=$(jq '[.severity_changes[] | select(.after_severity == "critical")] | length' diff.json)
          if [ "$NEW" -gt 0 ] || [ "$REGRESSIONS" -gt 0 ]; then
            echo "::error::diff introduces $NEW new finding(s) and $REGRESSIONS critical regression(s)"
            exit 1
          fi
```

Tweak the threshold logic in the final step to match your team's policy. Some teams gate on any new finding, others tolerate Info-level new findings and only fail on Warning or Critical regressions.

---

## Daemon query API

The daemon exposes an HTTP query API on the same port as OTLP HTTP and `/metrics` (default `4318`). It lets external systems pull recent findings, trace explanations, cross-trace correlations and daemon liveness without parsing NDJSON logs. Useful for Prometheus alerting, custom Grafana panels or SRE runbooks.

```bash
# Daemon liveness
curl -sS http://127.0.0.1:4318/api/status

# Recent critical findings
curl -sS "http://127.0.0.1:4318/api/findings?severity=critical&limit=10"
```

See [`docs/QUERY-API.md`](./QUERY-API.md) for the full per-endpoint reference, real captured response examples, use cases (Prometheus alerting, Grafana dashboard, SRE runbook) and the stability contract.

---

## Advanced carbon scoring setup

### Multi-region scoring

If your services span multiple cloud regions, perf-sentinel can apply per-region carbon intensity coefficients. The primary mechanism is the OTel `cloud.region` resource attribute, which most cloud-hosted OTel SDKs emit automatically. When this attribute is absent (e.g., Jaeger/Zipkin ingestion), use the `[green.service_regions]` table to map services to regions:

```toml
[green]
default_region = "eu-west-3"

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc"  = "ap-southeast-1"
"auth-svc"  = "eu-west-3"
```

The region resolution chain is: span `cloud.region` attribute > `service_regions[service]` > `default_region` > synthetic `"unknown"` bucket. The JSON report includes a `regions[]` array sorted by CO2 descending, with each row showing the region name, grid intensity, PUE, I/O op count and operational CO2.

### Scaphandre integration (on-premise / bare metal)

For on-premise or bare-metal servers with Intel RAPL support, perf-sentinel can scrape [Scaphandre's](https://github.com/hubblo-org/scaphandre) per-process power metrics to replace the I/O proxy model with measured energy data.

**Prerequisites:**
- Scaphandre installed and running on each host, exposing a Prometheus `/metrics` endpoint.
- RAPL access available (bare metal or VM with RAPL passthrough).

**Configuration:**

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5
process_map = { "order-svc" = "java", "game-svc" = "game", "chat-svc" = "dotnet" }
```

The `process_map` maps perf-sentinel service names to the `exe` label in Scaphandre's `scaph_process_power_consumption_microwatts` metric. The daemon scrapes this endpoint every `scrape_interval_secs` and computes a per-service energy-per-op coefficient using the formula: `energy_kwh = (power_watts * interval) / ops / 3_600_000`.

Services not present in `process_map` or when the endpoint is unreachable, fall back to the proxy model transparently. The model tag flips to `"scaphandre_rapl"` for services using measured energy. Only the `watch` daemon mode uses Scaphandre; the `analyze` batch command always uses the proxy model.

### Cloud-native energy estimation (AWS / GCP / Azure)

For cloud VMs that do not expose RAPL (most non-bare-metal instances), perf-sentinel can estimate per-service energy using CPU utilization metrics from a Prometheus endpoint and the SPECpower model.

**Prerequisites:**
- A Prometheus-compatible endpoint with CPU utilization metrics (via cloudwatch_exporter, stackdriver-exporter, azure-metrics-exporter or node_exporter).
- perf-sentinel does NOT query cloud provider APIs directly.

**Configuration:**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"
default_instance_type = "c5.xlarge"
cpu_metric = "node_cpu_seconds_total"

[green.cloud.services.api-us]
provider = "aws"
region = "us-east-1"
instance_type = "c5.4xlarge"

[green.cloud.services.analytics]
provider = "azure"
region = "westeurope"
instance_type = "Standard_D8s_v3"
```

The daemon interpolates power consumption as `watts = idle_watts + (max_watts - idle_watts) * (cpu% / 100)` using SPECpower data embedded in the binary (~60 common instance types across AWS, GCP, Azure). The model tag is `"cloud_specpower"`. Like Scaphandre, this is a daemon-only feature.

**Energy source precedence.** When both Scaphandre and cloud energy are configured for the same service, Scaphandre wins (direct RAPL measurement is more precise than CPU% interpolation). The full chain: `electricity_maps_api` > `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.

### Calibrate the proxy model from on-site measurements

When neither Scaphandre nor cloud energy are available but you have reference energy measurements from an external source (power meter, RAPL export, datacenter monitoring), the `perf-sentinel calibrate` subcommand tunes the I/O-to-energy proxy coefficients per service. The three-step workflow:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow_dark.svg">
  <img alt="Calibration workflow" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow.svg">
</picture>

**1. Measure.** Run a reference workload and collect both traces (standard perf-sentinel JSON format) and energy measurements (CSV with `timestamp,service,power_watts` or `timestamp,service,energy_kwh` columns, auto-detected from the header).

**2. Calibrate.** Run `perf-sentinel calibrate --traces traces.json --measured-energy energy.csv --output calibration.toml`. The subcommand correlates I/O ops with energy readings per service and time window, computes `factor = measured_per_op / default_proxy` and writes a TOML file. Factors > 10x or < 0.1x emit warnings (likely measurement error).

**3. Use.** Load the calibration file at config time via `[green] calibration_file = ".perf-sentinel-calibration.toml"`. The scoring loop multiplies the proxy energy by the per-service factor and the model tag gets a `+cal` suffix (e.g. `io_proxy_v2+cal`). Calibration only applies to the proxy model: Scaphandre/cloud measured energy still overrides.

---

## Tempo integration

If your infrastructure uses Grafana Tempo as the trace backend, you can query it directly with `perf-sentinel tempo` instead of exporting traces to files.

> **Post-mortem workflow.** When a trace is older than the daemon's 30-second live window, Tempo becomes the replay source for `perf-sentinel tempo --trace-id …`. The full incident workflow (Grafana alert → exemplar → trace_id → replay) is documented in [RUNBOOK.md](RUNBOOK.md).

### Single trace analysis

```bash
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123def456
```

### Service-based search

```bash
# Analyze the last hour of traces for order-svc
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 1h

# CI mode with quality gate
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 30m --ci
```

### Requirements

- Tempo must expose its HTTP API (default port 3200).
- The `--endpoint` flag points to the Tempo API base URL.
- Traces are fetched as OTLP protobuf and run through the standard analysis pipeline. The output is identical to `perf-sentinel analyze`.

### Tempo in microservices mode (`tempo-distributed`)

If your Tempo is deployed via the `tempo-distributed` Helm chart rather than the monolithic single-binary image, the HTTP query API is exposed by **`tempo-query-frontend`**, not by `tempo-querier`. `tempo-querier` is an internal worker with no public-facing API, so pointing `--endpoint` at it returns HTTP 404 on every `/api/search` request. Resolve the query-frontend hostname the way your environment does it (Kubernetes Service name, Docker Compose service name, or an explicit host for bare-metal):

```bash
perf-sentinel tempo --endpoint http://tempo-query-frontend:3200 \
  --service order-svc --lookback 1h
```

A 404 from a wrong endpoint now surfaces as `Tempo returned HTTP 404 for https://.../api/search?...` (the failing URL is included in the message) so this misconfiguration is diagnosable at a glance.

### Alternative: Tempo generic forwarding

Instead of querying Tempo, you can configure Tempo to forward a copy of traces to perf-sentinel via [generic forwarding](https://grafana.com/docs/tempo/latest/operations/manage-advanced-systems/generic_forwarding/). This avoids querying Tempo and works in real-time with `perf-sentinel watch`.

---

## Troubleshooting

### No events received (`events_processed_total = 0`)

1. **Check connectivity.** From inside the container: `curl http://host.docker.internal:4318/metrics`. If it fails, perf-sentinel is not reachable.
2. **Check bind address.** perf-sentinel defaults to `127.0.0.1`. For Docker access, configure `listen_address = "0.0.0.0"` in `.perf-sentinel.toml` or run natively on the host.
3. **Check protocol.** The Java Agent defaults to gRPC (port 4317). Ensure `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` matches the port you are targeting.

### Events received but no findings

1. **Check span attributes.** perf-sentinel only processes spans with `db.statement`/`db.query.text` (SQL) or `http.url`/`url.full` (HTTP). Other spans are skipped.
2. **Check detection thresholds.** The default N+1 threshold is 5 occurrences of the same normalized template within the same trace. If your trace has fewer than 5 repeated calls, no finding is generated.
3. **Check URL normalization.** perf-sentinel replaces numeric path segments with `{id}` and UUIDs with `{uuid}`. If your repeated URLs differ only by a string identifier (e.g., `/account/alice`, `/account/bob`), they will not be grouped into the same template.

### AOT cache error with Java Agent

The Java Agent (`-javaagent:`) is incompatible with JEP 483 AOT caches. If you see `Unable to map shared spaces` or `Mismatched values for property jdk.module.addmods`, bypass the AOT cache when the agent is active (see the Java section above).

### Spring Boot starter does not capture outbound HTTP calls

The `spring-boot-starter-opentelemetry` (Spring Boot 4) bridges Micrometer metrics to OTel but does not fully instrument outbound `WebClient` or `RestTemplate` calls with trace context propagation. Use the Java Agent for complete instrumentation.
