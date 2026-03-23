# OTLP Setup Guide

perf-sentinel accepts OpenTelemetry traces via OTLP (gRPC on port 4317, HTTP on port 4318).
This guide shows how to configure your application to send traces to perf-sentinel.

## Prerequisites

Start perf-sentinel in daemon mode:

```bash
perf-sentinel watch
```

By default, it listens on:
- `127.0.0.1:4317` (OTLP gRPC)
- `127.0.0.1:4318` (OTLP HTTP)

## Required span attributes

perf-sentinel detects I/O anti-patterns by looking at these span attributes:

| Attribute          | Used for            | Example                                   |
|--------------------|---------------------|-------------------------------------------|
| `db.statement`     | SQL query detection | `SELECT * FROM player WHERE game_id = 42` |
| `db.system`        | SQL operation type  | `postgresql`, `mysql`                     |
| `http.url`         | HTTP call detection | `http://account-svc:5000/api/account/123` |
| `http.method`      | HTTP method         | `GET`, `POST`                             |
| `http.route`       | Source endpoint     | `POST /api/game/{id}/start`               |
| `http.status_code` | HTTP status         | `200`, `404`                              |
| `service.name`     | Service identifier  | `game`, `account-svc`                     |

Spans without `db.statement` or `http.url` are ignored (they are not I/O operations).

---

## Java (Spring Boot + OpenTelemetry Java Agent)

The simplest setup: attach the OTel Java agent to your Spring Boot application.

### 1. Download the agent

```bash
curl -L -o opentelemetry-javaagent.jar \
  https://github.com/open-telemetry/opentelemetry-java-instrumentation/releases/latest/download/opentelemetry-javaagent.jar
```

### 2. Run your application with the agent

```bash
java -javaagent:opentelemetry-javaagent.jar \
  -Dotel.exporter.otlp.endpoint=http://127.0.0.1:4318 \
  -Dotel.exporter.otlp.protocol=http/protobuf \
  -Dotel.traces.exporter=otlp \
  -Dotel.service.name=my-service \
  -jar my-app.jar
```

Or using environment variables:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
export OTEL_TRACES_EXPORTER=otlp
export OTEL_SERVICE_NAME=my-service
java -javaagent:opentelemetry-javaagent.jar -jar my-app.jar
```

The Java agent automatically captures `db.statement`, `http.url`, `http.method`, and `http.route` from JDBC, Spring Web, and HTTP client libraries.

### 3. Run perf-sentinel

```bash
perf-sentinel watch
# Findings are emitted as NDJSON on stdout
```

---

## .NET (ASP.NET Core + OpenTelemetry SDK)

### 1. Add NuGet packages

```bash
dotnet add package OpenTelemetry.Extensions.Hosting
dotnet add package OpenTelemetry.Instrumentation.AspNetCore
dotnet add package OpenTelemetry.Instrumentation.Http
dotnet add package OpenTelemetry.Instrumentation.SqlClient
dotnet add package OpenTelemetry.Exporter.OpenTelemetryProtocol
```

### 2. Configure in Program.cs

```csharp
using OpenTelemetry.Resources;
using OpenTelemetry.Trace;

var builder = WebApplication.CreateBuilder(args);

builder.Services.AddOpenTelemetry()
    .ConfigureResource(r => r.AddService("my-service"))
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddHttpClientInstrumentation()
        .AddSqlClientInstrumentation(o => o.SetDbStatementForText = true)
        .AddOtlpExporter(o =>
        {
            o.Endpoint = new Uri("http://127.0.0.1:4318");
            o.Protocol = OpenTelemetry.Exporter.OtlpExportProtocol.HttpProtobuf;
        }));
```

**Important:** `SetDbStatementForText = true` is required for perf-sentinel to see SQL queries. Without it, `db.statement` is not captured.

To use gRPC (port 4317) instead of HTTP:

```csharp
.AddOtlpExporter(o =>
{
    o.Endpoint = new Uri("http://127.0.0.1:4317");
    o.Protocol = OpenTelemetry.Exporter.OtlpExportProtocol.Grpc;
});
```

### 3. Run perf-sentinel

```bash
perf-sentinel watch
```

---

## Rust (tracing + opentelemetry-otlp)

### 1. Add dependencies

```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-opentelemetry = "0.30"
opentelemetry = "0.29"
opentelemetry_sdk = { version = "0.29", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.29", features = ["tonic"] }
```

### 2. Configure the exporter

```rust
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint("http://127.0.0.1:4317")
        .build()
        .expect("failed to create OTLP exporter");

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer("my-service");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    tracing_subscriber::registry()
        .with(otel_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Your application code here
    // Use tracing::info_span!() to create spans

    provider.shutdown().expect("failed to shutdown tracer");
}
```

### 3. Add SQL/HTTP attributes to spans

For perf-sentinel to detect anti-patterns, your spans need the right attributes:

```rust
use tracing::info_span;

// SQL query span
let _span = info_span!(
    "db.query",
    db.statement = "SELECT * FROM player WHERE game_id = 42",
    db.system = "postgresql"
);

// HTTP client span
let _span = info_span!(
    "http.request",
    http.url = "http://account-svc:5000/api/account/123",
    http.method = "GET",
    http.status_code = 200
);
```

### 4. Run perf-sentinel

```bash
perf-sentinel watch
# Or use gRPC (default for Rust OTLP exporter):
# Listens on 127.0.0.1:4317
```

---

## CI mode (batch analysis)

For CI pipelines, use batch mode instead of daemon mode:

1. Run your integration tests with OTLP export to a file (or pipe traces)
2. Analyze the traces:

```bash
perf-sentinel analyze --ci --input traces.json
```

Exit code is non-zero if the quality gate fails. Configure thresholds in `.perf-sentinel.toml`:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # Zero critical N+1 SQL allowed
n_plus_one_http_warning_max = 3    # Up to 3 warning N+1 HTTP allowed
io_waste_ratio_max = 0.30          # Max 30% I/O waste ratio
```

---

## Via OpenTelemetry Collector

If you already have an OTel Collector, add perf-sentinel as an exporter:

```yaml
# otel-collector-config.yaml
exporters:
  otlp/perf-sentinel:
    endpoint: "127.0.0.1:4317"
    tls:
      insecure: true

service:
  pipelines:
    traces:
      exporters: [otlp/perf-sentinel, jaeger]  # Send to both
```

This lets you keep your existing tracing pipeline (Jaeger, Tempo, etc.) and add perf-sentinel analysis on top.
