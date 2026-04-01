# OTLP integration guide

perf-sentinel accepts OpenTelemetry traces via OTLP (gRPC on port 4317, HTTP on port 4318).

## Quick start

```bash
perf-sentinel watch
```

By default, it listens on `127.0.0.1:4317` (gRPC) and `127.0.0.1:4318` (HTTP).

## Two integration paths

| Scenario | Approach | Effort | Changes to services |
|----------|----------|--------|---------------------|
| **Production: services already send traces to a collector** | Add perf-sentinel as an exporter in the OTel Collector config | One line of YAML | None |
| **Dev/staging: no collector in place** | Instrument each service to send traces directly to perf-sentinel | Per-language setup (see below) | Varies |

If your services already export traces to Jaeger, Tempo, or any backend via an OpenTelemetry Collector, start with the collector approach: it requires zero changes to your application code.

---

## Production: via OpenTelemetry Collector

If you already have an [OTel Collector](https://opentelemetry.io/docs/collector/), you will be able to add perf-sentinel as an additional OTLP exporter. Your existing tracing pipeline (Jaeger, Tempo, etc.) keeps working; perf-sentinel analyzes a copy of the same spans.

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

This approach is the target for production deployments because:
- Zero code changes in your services
- No rebuild, no redeployment
- Works regardless of language (Java, C#, Rust, Go, Python, Node.js)
- Sampling and filtering happen at the collector level
- perf-sentinel can be added or removed without touching application code

> **Note:** this integration path has not been validated end-to-end yet. The per-language direct instrumentation described below has been tested and validated on real microservices.

---

## Required span attributes

perf-sentinel detects I/O anti-patterns by looking at specific span attributes. Both the legacy and stable [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/) are supported.

| Purpose | Legacy attribute (pre-1.21) | Stable attribute (1.21+) | Example |
|---------|---------------------------|-------------------------|---------|
| SQL query text | `db.statement` | `db.query.text` | `SELECT * FROM player WHERE game_id = 42` |
| SQL system | `db.system` | `db.system` | `postgresql`, `mysql` |
| HTTP target URL | `http.url` | `url.full` | `http://account-svc:5000/api/account/123` |
| HTTP method | `http.method` | `http.request.method` | `GET`, `POST` |
| HTTP status | `http.status_code` | `http.response.status_code` | `200`, `404` |
| Source endpoint | `http.route` | `http.route` | `POST /api/game/{id}/start` |
| Service name | `service.name` (resource) | `service.name` (resource) | `game`, `account-svc` |

Spans that have neither a SQL attribute nor an HTTP attribute are skipped: they are not I/O operations. Modern OTel agents (v2.x) emit the stable convention by default. Older agents emit the legacy convention. perf-sentinel handles both transparently.

---

## Dev/staging: per-language instrumentation

When no OTel Collector is available, instrument services directly. The guides below are ordered from easiest to most involved.

### Java (OpenTelemetry Java Agent)

The [OTel Java Agent](https://opentelemetry.io/docs/zero-code/java/agent/) instruments JDBC, R2DBC, HTTP clients, Spring Web, and most frameworks automatically, with zero code changes. This is the closest to plug and play.

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
- Trace context propagation across async boundaries, reactive chains, and inter-service calls

This has been validated on Spring Boot 4 with WebFlux/R2DBC, Virtual Threads/JPA, and standard MVC/JDBC.

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
