# Perf Sentinel instrumentation guide

This guide covers the parts of the data pipeline that turn an application's runtime activity into the OTLP / JSON input Perf Sentinel consumes. For an end-to-end overview, the four supported topologies and the four quick starts, see [INTEGRATION.md](./INTEGRATION.md). For the CI-side of the integration (CI mode, GitHub Actions / GitLab CI / Jenkins recipes, interactive HTML report deployment, PR regression detection), see [CI.md](./CI.md).

> **Not using an OpenTelemetry SDK?** Teams on Datadog can feed Perf Sentinel by bridging dd-trace through the OTel Collector `datadogreceiver`, with no application change. This per-language guide does not apply to that path, see [Coming from Datadog](./INTEGRATION.md#coming-from-datadog-dd-trace-no-opentelemetry).

## Contents

- [Kubernetes deployment](#kubernetes-deployment): manifests for the daemon and the OTel Collector sidecar.
- [Cloud provider integrations](#cloud-provider-integrations): AWS X-Ray, GCP Cloud Trace, Azure Application Insights, self-hosted Jaeger / Tempo / Zipkin.
- [Production: via OpenTelemetry Collector](#production-via-opentelemetry-collector): central collector setup, sampling and detection accuracy.
- [Required span attributes](#required-span-attributes): the legacy and stable OTel semantic conventions Perf Sentinel reads.
- [Dev/staging: per-language instrumentation](#devstaging-per-language-instrumentation):
  - Java
    - [Spring Boot, Helidon 4.x](#java-opentelemetry-java-agent-v227-spring-boot-helidon-4x)
    - [Spring Boot 4 starter, no agent](#java-spring-boot-4-starter-spring-boot-starter-opentelemetry)
    - [Quarkus 3.33 LTS](#java-quarkus-333-lts--quarkus-opentelemetry--otel-agent-v227)
  - [.NET (ASP.NET Core + Entity Framework Core)](#net-aspnet-core--entity-framework-core--opentelemetry-sdk-115)
  - [Go (pgx)](#go-otelhttp-068--otelpgx-011-otel-sdk-143)
  - Python
    - [Django + psycopg](#python-django-5x--psycopg-otel-sdk-142)
    - [FastAPI + SQLAlchemy + asyncpg](#python-fastapi--sqlalchemy-2x--asyncpg-otel-sdk-142)
  - [Node.js (Nest.js + Prisma)](#nodejs-nestjs--prisma-otel-sdk-057)
  - [Rust (Diesel, SeaORM)](#rust-tracing-opentelemetry-031-diesel-seaorm)
  - [Ruby (Rails + ActiveRecord)](#ruby-rails--activerecord-opentelemetry-ruby)
  - [PHP (Laravel / Eloquent, Symfony / Doctrine)](#php-laravel--eloquent-symfony--doctrine-opentelemetry-php)
- [SQL placeholder styles and detection](#sql-placeholder-styles-and-detection): how Perf Sentinel maps each instrumentation's SQL placeholder to the sanitizer-aware N+1 detection path.

## Background: OpenTelemetry primer

If you have not used OpenTelemetry before, this short primer is a prerequisite for the rest of this guide. It assumes you know what an HTTP request and a database query are. It does not assume you have ever instrumented an application or run a tracing backend. Other Perf Sentinel docs cross-reference this primer for OTel concepts, see [docs/INTEGRATION.md](INTEGRATION.md) and [docs/HELM-DEPLOYMENT.md](HELM-DEPLOYMENT.md#observability).

**What is OpenTelemetry.** OpenTelemetry (often shortened to "OTel") is a Cloud Native Computing Foundation (CNCF) project that defines an open standard for collecting telemetry data (traces, metrics, logs) from any kind of software. It is the merger of two earlier projects (OpenTracing and OpenCensus) consolidated in 2019, governed under CNCF since. The two practical things OTel gives you:

- **A protocol** (OTLP, OpenTelemetry Protocol) that any application can use to ship traces and metrics to any backend that speaks it. OTLP is wire-format-stable, ships in both gRPC and HTTP+protobuf variants, and is what Perf Sentinel ingests on ports 4317 (gRPC) and 4318 (HTTP).
- **SDKs** (Java, Python, Go, .NET, Rust, JavaScript, ...) that handle the boring parts: capturing each HTTP/SQL call as a *span*, propagating the trace ID across services, batching, retrying, and sending OTLP. Most language SDKs include auto-instrumentation for popular frameworks (Spring, Quarkus, ASP.NET Core, Django, Express) so the application code itself rarely changes.

**Key concepts.**

- A **span** is a unit of work, typically one HTTP request or one SQL query. It carries a duration, a status, a name (`GET /api/orders`), and a structured attribute bag.
- A **trace** is the tree of spans that share a `trace_id`. A single user request typically crosses several services, each producing several spans, all linked by the same `trace_id`.
- **Semantic conventions** are the OTel-defined attribute names so different SDKs all emit the same field for the same concept. `http.request.method` is always the HTTP verb, `db.system` is always the database engine name, and so on. Perf Sentinel reads a small subset of these attributes to detect anti-patterns. The closed list of attributes Perf Sentinel reads is in [Required span attributes](#required-span-attributes) below.

**The Collector.** A separate process, the **OpenTelemetry Collector**, is the recommended deployment shape between applications and backends. It receives OTLP from a fleet of applications, applies optional sampling and attribute processing, and forwards to one or more backends in parallel (Perf Sentinel, plus Tempo or Jaeger for storage, plus Prometheus exemplars). Running a central Collector decouples the applications from each backend's quirks and lets operators change sampling policy without touching application code. The relevant deployment shapes are covered in [Production: via OpenTelemetry Collector](#production-via-opentelemetry-collector) below.

**Where to learn more.** [opentelemetry.io](https://opentelemetry.io/), [OTLP spec](https://github.com/open-telemetry/opentelemetry-proto), [semantic conventions](https://opentelemetry.io/docs/specs/semconv/).

## Kubernetes deployment

A packaged Helm chart is available under [`charts/perf-sentinel/`](../charts/perf-sentinel/). See [HELM-DEPLOYMENT.md](./HELM-DEPLOYMENT.md) for the full install guide and [`examples/helm/`](../examples/helm/) for a worked example composing the chart with the upstream OpenTelemetry Collector chart. The raw manifests below remain for users who prefer to deploy without Helm.

Perf Sentinel runs as a standard Kubernetes Deployment behind a Service. The OTel Collector runs as a DaemonSet (per-node) or Deployment (centralized), forwarding traces to Perf Sentinel.

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
              memory: "64Mi"
              cpu: "50m"
            limits:
              memory: "256Mi"
              cpu: "500m"
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

In your existing Collector config (DaemonSet or Deployment), add Perf Sentinel as an exporter:

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

If you use the Prometheus Operator, scrape Perf Sentinel metrics with a ServiceMonitor:

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

Perf Sentinel is cloud-agnostic: it receives standard OTLP traces. The key is to route a copy of your traces to Perf Sentinel alongside your cloud-native trace backend.

### AWS (X-Ray + OTel Collector)

AWS X-Ray uses a proprietary format, but the [AWS Distro for OpenTelemetry (ADOT)](https://aws-otel.github.io/) Collector can export both to X-Ray and to Perf Sentinel:

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

Deploy Perf Sentinel as an ECS task or EKS Deployment. For ECS, use the `scratch`-based Docker image (`ghcr.io/robintra/perf-sentinel:latest`).

### GCP (Cloud Trace + OTel Collector)

GCP Cloud Trace supports OTLP ingestion natively. Use the standard OTel Collector with both the `googlecloud` exporter and the Perf Sentinel exporter:

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

Deploy Perf Sentinel as a Cloud Run service or GKE Deployment. For Cloud Run, expose port 4317 (gRPC) and 4318 (HTTP).

### Azure (Application Insights + OTel Collector)

Azure Monitor supports OTLP via the [Azure Monitor OpenTelemetry Exporter](https://learn.microsoft.com/en-us/azure/azure-monitor/app/opentelemetry-configuration). Route traces to both Azure and Perf Sentinel:

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

Deploy Perf Sentinel as an AKS Deployment or Azure Container Instance.

### Self-hosted (Jaeger, Tempo, Zipkin)

If you use a self-hosted trace backend, the OTel Collector approach works identically. Add Perf Sentinel as an additional OTLP exporter alongside your existing backend exporter. Alternatively, use Perf Sentinel's batch mode with an OTLP JSON dump from the Collector `file` exporter, or with trace files exported from Jaeger UI (`--input jaeger-export.json`) or Zipkin UI (`--input zipkin-traces.json`). Formats are auto-detected.

---

## Production: via OpenTelemetry Collector

If you already have an [OTel Collector](https://opentelemetry.io/docs/collector/), you can add Perf Sentinel as an additional OTLP exporter. Your existing tracing pipeline (Jaeger, Tempo, etc.) keeps working, Perf Sentinel analyzes a copy of the same spans.

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

The OTel Collector ships gzip-compressed exports by default, and both endpoints accept them, OTLP/gRPC (`:4317`) and OTLP/HTTP (`POST /v1/traces`), so no `compression: none` override is required. gzip, deflate and uncompressed are the accepted encodings. Any other one the exporter can be set to, `snappy` and `zstd` among them, is refused with a permanent error and has to be changed back to `gzip` or `none`. The decompressed payload respects the `[daemon] max_payload_size` limit (16 MiB by default), and a batch above it is refused with `ResourceExhausted`, which only the Collector's own logs report. On a memory-capped pod, that limit now bounds decode buffers rather than uploaded bytes, so `[daemon] memory_high_water_pct` (off by default) is what keeps a burst of compressed exports from being admitted, see `docs/CONFIGURATION.md`.

Up to and including 0.9.26, the gRPC endpoint refused every compressed export. The Collector logs it as ``rpc error: code = Unimplemented desc = Content is compressed with `gzip` which isn't supported``, treats it as permanent and drops the batch, so the loss shows up nowhere else. On those versions, either set `compression: none` on the exporter or point it at the HTTP endpoint, which has accepted gzip since 0.5.5.

This approach is recommended for production deployments because:
- Zero code changes in your services
- No rebuild, no redeployment
- Works regardless of language (Java, C#, Rust, Go, Python, Node.js)
- Sampling and filtering happen at the collector level
- Perf Sentinel can be added or removed without touching application code

A full reference configuration is provided in [`examples/otel-collector-config.yaml`](../examples/otel-collector-config.yaml) with a matching Docker Compose file in [`examples/docker-compose-collector.yml`](../examples/docker-compose-collector.yml).

### End-to-end setup with Docker Compose

1. Start the stack:

```bash
docker compose -f examples/docker-compose-collector.yml up -d
```

2. Configure your applications to export OTLP traces to the collector:
   - gRPC: `localhost:4317`
   - HTTP: `localhost:4318`

3. Verify Perf Sentinel is receiving spans:

```bash
curl -s http://localhost:14318/metrics | grep perf_sentinel_events_processed_total
```

4. View findings emitted by Perf Sentinel on stdout:

```bash
docker compose -f examples/docker-compose-collector.yml logs -f perf-sentinel
```

### Sampling and filtering

For high-traffic environments, the OTel Collector supports tail-based sampling and filtering to reduce the volume of traces forwarded to Perf Sentinel.

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

**Where to put the sampler.** Sampling exists to bound what a trace
store retains, and Perf Sentinel retains nothing: it holds a per-trace
window in memory for `trace_ttl_ms` and drops it. So the cheapest
correct layout is to fan out from the same receiver and sample only the
branch that feeds storage:

```yaml
service:
  pipelines:
    # Storage: sampled, because Tempo pays per byte retained.
    traces/tempo:
      receivers: [otlp]
      processors: [tail_sampling, batch]
      exporters: [otlp/tempo]
    # Analysis: unsampled, because detection quality pays for it instead.
    traces/perf-sentinel:
      receivers: [otlp]
      processors: [filter, batch]
      exporters: [otlp/perf-sentinel]
```

Sampling in front of Perf Sentinel is supported, but it is lossy in
ways the daemon cannot report: a kept trace is indistinguishable from a
complete one, so nothing in the output says the numbers cover a tenth
of the traffic. If volume forces you to narrow the analysis branch,
narrow it by **scope rather than by chance**: route the namespaces or
services you are actively working on and keep their figures whole,
instead of a probabilistic sample that makes every service's figures
partial.

**Sampling and detection accuracy**.

Anti-pattern detection relies on counting events. Sampling that drops events directly affects which patterns Perf Sentinel can flag.

- **Within a kept trace, all spans are preserved**. OTel and Jaeger sample per-trace, not per-span, so an N+1 loop, a chatty service hop or a fanout pattern that lives inside one request still detects cleanly as long as the parent trace is kept.
- **Head-based sampling breaks count-based detections**. A 1% head-based policy drops 99% of traces before they reach the collector, so a 50-call N+1 loop is observed as 3 calls, well below any reasonable threshold. Same for chatty services, fanout, serialized parallelizable calls, pool saturation. Anything threshold-driven gets silently underreported.
- **Tail-based sampling stays compatible with detection** because the policies you would write for incident review (keep errors, keep slow traces, keep specific services) are exactly the ones that surface anti-patterns. The [`tail_sampling` processor](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/processor/tailsamplingprocessor) example above keeps everything under those policies plus a 10% probabilistic sample of the rest.
- **Counts are understated by any sampling, silently.** Finding counts, occurrence counts and the Prometheus totals describe the traces that arrived, and nothing scales them back up. Ratios are more subtle: a uniform sampler hits numerator and denominator alike, so the I/O waste ratio stays unbiased, but the `errors` and `slow` policies of a tail sampler bias retention toward heavy traces and the ratio drifts with them. Perf Sentinel cannot detect upstream sampling, so it cannot warn about either. Do not publish those numbers as whole-traffic figures, which matters most for `disclose`, whose whole purpose is publishing a measured figure. The daemon's own `[daemon] sampling_rate` is the one case it can see, and it does emit a `tuning` warning for it.
- **Cross-trace correlation goes quiet.** `[daemon.correlation] min_co_occurrences` needs a finding pair to recur inside the window. At a 10% sample the repeated co-occurrences rarely survive, so the correlator reports nothing even when the coupling is real. That silence is not evidence of a healthy topology.
- **CI runs should keep 100% of traces**. Volume is low (one integration-test run), the cost of full instrumentation is negligible, and missing a regression because of sampling defeats the purpose of the CI gate. The quick starts in [INTEGRATION.md](./INTEGRATION.md) assume 100% sampling.
- **`pg-stat` mode is sampling-immune**. `pg_stat_statements` aggregates query counters server-side in PostgreSQL, regardless of what the application tracer captured. A query that runs 10 000 times shows up as 10 000 calls even if 99% of the parent traces were dropped at the head. Run `perf-sentinel pg-stat ...` (or pass `--pg-stat` to `analyze` and `report`) as a fallback when you cannot trust the trace volume, or as a primary signal for code paths the tracer does not even cover.

> **Note:** tail-based sampling requires the `otel/opentelemetry-collector-contrib` image (not the core image).

---

## Required span attributes

Perf Sentinel detects I/O anti-patterns by looking at specific span attributes. Both the legacy and stable [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/) are supported.

| Purpose              | Legacy attribute (pre-1.21)               | Stable attribute (1.21+)             | Example                                   |
|----------------------|-------------------------------------------|--------------------------------------|-------------------------------------------|
| SQL query text       | `db.statement`                            | `db.query.text`                      | `SELECT * FROM player WHERE game_id = 42` |
| SQL system           | `db.system`                               | `db.system.name`                     | `postgresql`, `mysql`                     |
| HTTP target URL      | `http.url`                                | `url.full`                           | `http://account-svc:5000/api/account/123` |
| HTTP method          | `http.method`                             | `http.request.method`                | `GET`, `POST`                             |
| HTTP status          | `http.status_code`                        | `http.response.status_code`          | `200`, `404`                              |
| RPC callee           | `rpc.system` + `rpc.service`/`rpc.method` | (same)                               | `grpc`, `order.v1.OrderService/GetOrder`  |
| Broker system        | `messaging.system`                        | (same)                               | `kafka`, `rabbitmq`, `pulsar`, `aws_sqs`  |
| Broker destination   | `messaging.destination`                   | `messaging.destination.name`         | `orders`, `signature.jobs`                |
| Message size         | `messaging.message.body.size`             | (same)                               | `4096`                                    |
| Source endpoint      | `http.route`, `url.path`                  | `http.route`, `url.path`             | `POST /api/game/{id}/start`               |
| Service name         | `service.name` (resource)                 | `service.name` (resource)            | `game`, `account-svc`                     |
| Service namespace    | `service.namespace` (resource)            | (same)                               | `commerce`                                |
| Kubernetes namespace | `k8s.namespace.name` (resource)           | (same)                               | `prod-eu`                                 |
| Code function        | `code.namespace` + `code.function`        | `code.function.name`                 | `com.example.OrderService.place`          |
| Code file and line   | `code.filepath`, `code.lineno`            | `code.file.path`, `code.line.number` | `OrderService.java`, `42`                 |

Spring Boot services traced through Micrometer Observation (the `spring-boot-starter-opentelemetry` starter, or the Micrometer Zipkin bridge) tag their outbound HTTP spans with `method` and `status` instead of the OTel names. Perf Sentinel reads these two tags as a last resort, and only on a span it already classified as an outbound call through its URL. A non-numeric `status` such as `CLIENT_ERROR` leaves the status empty. Through the starter, over OTLP, every span sits under the `org.springframework.boot` scope, and by default no span carries a `code.*` attribute, so the scope names the language only: a SELECT Hibernate generated still gets the JPA fix, since Hibernate 6 signs it with its table aliases (`d1_0.id`), and the other framework-keyed findings get the Java generic fix. The Zipkin bridge carries no scope, so its findings keep the generic suggestion alone. A service that adds `code.*` attributes to its spans, as the [starter section](#6-code-location) describes, gets the fix its code location points to.

The code attributes are optional. A finding carries a code location when its span, or over OTLP its nearest ancestor within the same service (up to eight levels up), has one of them. The stable names win over the legacy ones, a qualified `code.function.name` also yields the namespace, and over OTLP `code.line.number` is read only as an integer: a line number sent as a string is ignored. Jaeger and Zipkin read the code attributes from the I/O span itself, and accept a line number written as a string.

Spans that carry no SQL, HTTP, RPC, or messaging attribute are skipped: they are not I/O operations. Modern OTel agents (v2.x) emit the stable convention by default. Older agents emit the legacy convention. Perf Sentinel handles both transparently.

**Outbound HTTP is client-side only.** A span whose kind is SERVER never becomes an outbound HTTP call, even when it carries `http.url` or `url.full`. The stable convention puts `url.full` on CLIENT spans only, but legacy instrumentations set `http.url` on the inbound handler span too, and admitting those would count every instrumented hop twice and credit a service with calls it never made. This matches the CLIENT-only rule for RPC below. Three consequences. A SERVER span carrying `db.statement` is still analyzed, because SQL is classified before HTTP. A span that never sets its kind stays eligible for HTTP, so an instrumentation that omits the kind is unaffected. And a rejected SERVER span still supplies its `http.route` as the inbound endpoint that findings are attributed to. Jaeger reads the `span.kind` tag (`server`), Zipkin the `kind` field (`SERVER`).

Which attributes separate one deployment from another is configuration, not a fixed pair. `[detection] grouping_attributes` takes an ordered list of resource or span attributes, defaulting to `["k8s.namespace.name", "service.namespace"]`. The first one present on a span decides identity: two identical findings in two groupings stay two findings, and the key remains part of that identity so `tenant.id=prod` cannot collide with `k8s.namespace.name=prod`. Every listed attribute that is present is captured and displayed, and each surface labels it as `key=value`. A shared cluster where the namespace does not tell tenants apart can group by `tenant.id` instead, provided the application sets it on its spans. The HTML filter uses the first captured configured attribute. When none is present, the finding has no grouping chip. Acknowledgment signatures ignore the list entirely, so one ack still covers every deployment and reordering the list never invalidates an ack. The same configured order applies to batch files, daemon OTLP gRPC and HTTP, Tempo, and Jaeger Query. Jaeger reads values from process tags with span-tag fallback, and Zipkin reads them from span tags.

RPC spans (gRPC, Dubbo, and similar frameworks) carry neither a statement nor a URL, so they are keyed on `rpc.system` and modeled as outbound calls. The target is `rpc.service/rpc.method` (falling back to the span name when either is absent), and findings appear under the `_http` types. This keeps the topological detectors (fanout, chatty, serialized) and the occurrence detectors (n+1, redundant) working on RPC-heavy fleets. RPC spans carry no query text, so `n_plus_one_sql` and the SQL normalizer never apply to them.

Three consequences to be aware of on RPC findings:

- **Only CLIENT spans are modeled.** The `rpc.*` attributes are set on the inbound SERVER handler span as well as the outbound CLIENT span, so Perf Sentinel admits only `SpanKind::Client`. An RPC span with an unset or non-CLIENT kind is treated as inbound work (not an outbound call), so an instrumentation that never sets the span kind produces no RPC findings.
- **Findings surface under the `_http` types.** An RPC N+1 is reported as `n_plus_one_http` and its remediation text mentions an HTTP batch endpoint. The finding is correct about the anti-pattern (the repeated dependency call), only the protocol label and the batch-endpoint wording are HTTP-flavored.
- **Per-call arguments are invisible.** A gRPC request payload lives in the protobuf message body, not in a span attribute, so N distinct calls to the same method share one empty-parameter template. Like a query-redacted HTTP URL (see [LIMITATIONS.md](./LIMITATIONS.md#http-query-string-redaction-and-n1-visibility)), those calls read as `redundant_http` rather than `n_plus_one_http`. The repeated-call signal is real either way, only the "cache vs batch" remediation differs.

Messaging spans (Kafka, RabbitMQ, Pulsar, SQS, NATS, JMS) carry neither a statement nor a URL either, so they are keyed on `messaging.system` and modeled as outbound calls whose target is the destination, falling back to the span name when the destination attribute is absent. One convention covers the whole family. Unlike RPC, they get their own finding types: `n_plus_one_messaging` and `slow_messaging`. There is no redundant counterpart because a publish carries no parameters to compare.

Three consequences to be aware of on messaging findings:

- **Only PRODUCER spans are modeled.** A `CONSUMER` span describes work done on a delivered message, not a call the service made, and a `CLIENT` messaging span is a poll (`receive`) or an ack (`settle`). Admitting them would attribute publishes the service never issued. An instrumentation that never sets the span kind therefore produces no messaging findings.
- **Destinations are compared verbatim.** A topic or queue name is already a template, so it does not go through the HTTP path normalizer. A topic named per tenant yields one template per tenant. See [LIMITATIONS.md](./LIMITATIONS.md#messaging-producer-side-only-no-consumer-analysis).
- **The consumer side is linked, not merged.** The OTel span link on a `CONSUMER` ancestor is carried onto the I/O spans of the handler and rendered by `explain` as `triggered by trace <id>` in the CLI, the TUI and `/api/explain/{trace_id}`, though not in the HTML dashboard. The producer and consumer traces stay separate, so the structural detectors never see across the broker.

> **Silent skip.** A span dropped for a missing carrying attribute
> produces no warning and no error. A SQL span without `db.statement` /
> `db.query.text`, an HTTP span without `http.url` / `url.full`, or a
> SERVER span whose URL describes its own inbound request yields no
> finding. A thin or empty report can therefore mean *no problems* or
> *no usable instrumentation*. Run `perf-sentinel inspect` to see what
> was extracted, and see
> [Instrumentation quality bounds findings](./LIMITATIONS.md#instrumentation-quality-bounds-findings).

> **Ack stability depends on `http.route`.** The acknowledgment
> signature is keyed on the route template, not the instantiated URL.
> Services that emit `http.route` (Spring Boot with the Java agent,
> ASP.NET Core, Express, any modern auto-instrumentation) get acks that
> survive restarts and rotating request ids. The Spring Boot starter
> needs a server convention for it, see
> [Inbound route](#4-inbound-route). Services that fall back to `http.url` /
> `url.full` lose that stability. See
> [`ACK-WORKFLOW.md`](./ACK-WORKFLOW.md#signature-stability-and-service-restarts)
> for the verification recipe.

---

## Dev/staging: per-language instrumentation

When no OTel Collector is available, instrument services directly. The guides below are ordered from easiest to most involved.

### Java (OpenTelemetry Java Agent v2.27+, Spring Boot, Helidon 4.x)

The [OTel Java Agent](https://opentelemetry.io/docs/zero-code/java/agent/) instruments JDBC, R2DBC, HTTP clients, Spring Web and most frameworks automatically, with zero code changes. This is the closest to plug and play.

On Spring Boot 4, the [`spring-boot-starter-opentelemetry` starter](#java-spring-boot-4-starter-spring-boot-starter-opentelemetry) is the other way in, through a Maven dependency and no agent. It suits applications that use the AOT cache or native images, or that must not depend on bytecode instrumentation matching their library versions, at the price of a few additions described in its section. The agent stays the shortest path for Spring Boot 3, for mixed fleets, and for libraries the Spring projects do not observe.

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
- The SQL statement from JDBC (Spring Data JPA, Hibernate) and R2DBC (Spring WebFlux reactive), in `db.statement` by default and in `db.query.text` with `otel.semconv-stability.opt-in=database`. Perf Sentinel reads both
- `url.full` from HTTP clients (WebClient, RestTemplate, HttpClient)
- `http.route` from Spring MVC and Spring WebFlux incoming requests
- Trace context propagation across async boundaries, reactive chains and inter-service calls

This has been validated on Spring Boot 4 with WebFlux/R2DBC, Virtual Threads/JPA and standard MVC/JDBC.

**R2DBC and SQL placeholder handling.** R2DBC drivers use database-native bind markers (`$1`, `$2` for PostgreSQL, `?` for MySQL/MariaDB). The Java Agent's built-in statement sanitizer replaces all literals with bare `?` before setting `db.statement`, regardless of the underlying driver. This means Perf Sentinel receives `?`-style sanitized templates with empty params for both JDBC and R2DBC stacks. Without the agent (R2DBC SDK only, no auto-instrumentation), `db.statement` would contain the native `$1`/`$2` markers, which Perf Sentinel also handles (the SQL normalizer recognizes `$N` as a placeholder since v0.7.7). Either way, the sanitizer-aware N+1 detection path fires correctly.

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

#### On a shared platform

- **Do not rely on `JAVA_TOOL_OPTIONS` set in the image.** When the platform sets the variable itself (a Helm chart's extra environment, a memory preset), it replaces the value from the Dockerfile and the agent never loads, with no error. Put `-javaagent:` in the entrypoint, or in a variable the platform does not own, and check the flags of the running JVM.
- **Configure the agent with real environment variables or system properties.** It starts in `premain`, before Spring, so values in `application.properties`, or in a ConfigMap mounted as such, never reach it.
- **One image, the agent switched per environment.** Ship the jar in every image and toggle it with `OTEL_JAVAAGENT_ENABLED`, `false` by default and `true` where traces are wanted, rather than building one image per case.
- **Leave room in Metaspace.** The agent loads many classes. A `-XX:MaxMetaspaceSize` sized for the application alone can end in `OutOfMemoryError: Metaspace` soon after the agent is enabled. Raise it and measure.
- **Run one tracer per JVM.** If the application also has a Micrometer tracing bridge, HTTP and messaging spans are emitted twice. Disable the in-application tracer where the agent runs: `management.tracing.enabled=false` on Spring Boot 3, `management.tracing.export.enabled=false` on Spring Boot 4.

**Code location.** The agent puts `code.*` attributes on its Spring Data repository spans, by default under the legacy names `code.namespace` and `code.function`, and under the stable `code.function.name` with `otel.semconv-stability.opt-in=code`. Perf Sentinel reads both, and over OTLP the SQL under a repository span inherits its code location (see [Required span attributes](#required-span-attributes)). Lazy loads, flushes and outbound HTTP calls have no repository ancestor, so their findings stay without a call site unless controller spans are turned on with `otel.instrumentation.common.experimental.controller-telemetry.enabled=true`, which names the handler method instead.

#### Known limitations

**Project Leyden / AOT cache incompatibility.** The `-javaagent:` flag is incompatible with JEP 483 AOT caches (`-XX:AOTCache`). Bypass it when the agent is active:

```bash
if echo "$JAVA_TOOL_OPTIONS" | grep -q "javaagent"; then
  exec java -jar /app/my-app.jar
else
  exec java -XX:AOTCache=/app/app.aot -jar /app/my-app.jar
fi
```

**Outbound clients the agent sees, the starter may not.** The agent instruments the HTTP library underneath, so every `RestClient`, `RestTemplate` or `WebClient` call is traced, however the client was built. Under the Spring Boot starter, only clients built from the auto-configured builders are, see [Outbound HTTP clients](#3-outbound-http-clients).

#### CI integration tests (Maven Failsafe)

The setup above assumes a long-running process talking to a live OTLP endpoint. Integration tests are different: they run inside the test runner's own JVM and there is no daemon to send traces to in CI. See [CI.md](./CI.md#ci-mode-batch-analysis) for the batch-mode path this feeds.

**Before agent 2.32.0, Java has no file exporter, and a forked test JVM cannot hand you its stdout either.** Up to SDK 1.65, the one agent 2.31.1 bundles, no Java exporter writes spans to a path you choose. The declarative-configuration exporter `otlp_file/development` defines an `output_stream: file://...` field, but the Java SDK only implements it from 1.66 on, see Option 3 below. That leaves `experimental-otlp/stdout`, which writes OTLP JSON to `System.out`. Maven gets in the way, because Surefire and Failsafe talk to the forked JVM over an encoded protocol carried on that fork's stdout. The agent initialises in `premain` and captures the original `System.out`, the command channel itself, before Surefire installs the wrapper `redirectTestOutputToFile` acts on. Every export is then classified as channel corruption and diverted into `target/failsafe-reports/<timestamp>-jvmRunN.dumpstream`:

```
Corrupted channel by directly writing to native stream in forked JVM 1.
Stream '{"resourceSpans":[{"resource":{"attributes":[{"key":"host.arch",…}]}}]}'.
```

Nothing usable reaches `-output.txt`, and piping the build with `tee` does not help either, since the fork's stdout is the channel rather than the console. This is not a version artefact: Failsafe 3.5.0, 3.2.5 and 2.22.2 all divert it.

So, short of agent 2.32.0, the traces have to leave the JVM the way they do in production, over the network, and something has to be listening. `perf-sentinel capture` is that listener, and it works the same on every agent version.

**Attach the agent to the test JVM, not just the built image.** If integration tests run in-process against `@SpringBootTest` (Maven Failsafe, Gradle `integrationTest`) rather than against the built container, the agent baked into your Dockerfile never sees them. Copy the agent jar into the build, pinned to the version baked into your Dockerfile so both environments instrument the same way:

```xml
<!-- Copy the agent jar into target/ before the integration-test phase. -->
<plugin>
  <groupId>org.apache.maven.plugins</groupId>
  <artifactId>maven-dependency-plugin</artifactId>
  <executions>
    <execution>
      <id>copy-otel-agent</id>
      <phase>pre-integration-test</phase>
      <goals><goal>copy</goal></goals>
      <configuration>
        <artifactItems>
          <artifactItem>
            <groupId>io.opentelemetry.javaagent</groupId>
            <artifactId>opentelemetry-javaagent</artifactId>
            <version>2.27.0</version> <!-- match the version baked into your Dockerfile -->
            <destFileName>opentelemetry-javaagent.jar</destFileName>
          </artifactItem>
        </artifactItems>
        <outputDirectory>${project.build.directory}</outputDirectory>
      </configuration>
    </execution>
  </executions>
</plugin>

<!-- Add -javaagent to the EXISTING failsafe argLine, do not replace it. -->
<plugin>
  <groupId>org.apache.maven.plugins</groupId>
  <artifactId>maven-failsafe-plugin</artifactId>
  <configuration>
    <argLine>@{argLine} -javaagent:${project.build.directory}/opentelemetry-javaagent.jar</argLine>
    <environmentVariables>
      <OTEL_TRACES_EXPORTER>otlp</OTEL_TRACES_EXPORTER>
      <OTEL_EXPORTER_OTLP_ENDPOINT>http://localhost:4317</OTEL_EXPORTER_OTLP_ENDPOINT>
      <OTEL_EXPORTER_OTLP_PROTOCOL>grpc</OTEL_EXPORTER_OTLP_PROTOCOL>
      <OTEL_SERVICE_NAME>my-service</OTEL_SERVICE_NAME>
      <OTEL_TRACES_SAMPLER>always_on</OTEL_TRACES_SAMPLER>
      <OTEL_METRICS_EXPORTER>none</OTEL_METRICS_EXPORTER>
      <OTEL_LOGS_EXPORTER>none</OTEL_LOGS_EXPORTER>
    </environmentVariables>
  </configuration>
</plugin>
```

Keep any existing `<argLine>` content (heap flags, a JaCoCo `@{argLine}` placeholder) and append `-javaagent:...` to it. Overwriting it is a common mistake that silently drops JaCoCo coverage instrumentation. `OTEL_TRACES_SAMPLER=always_on` matters more here than in production: sampling would drop exactly the repeated calls N+1 detection relies on.

**Set the protocol, do not rely on the default.** Agent 2.0 changed it from `grpc` to `http/protobuf`, so the same endpoint means different ports depending on the agent version. An endpoint pointed at the wrong one exports nothing and only warns in the agent's own log, which leaves a capture empty for a reason nothing else names. `:4317` with `grpc`, as above, and `:4318` with `http/protobuf` both work.

Nothing above is specific to Perf Sentinel: it is the standard OTLP setup. Only the listener changes.

##### Option 1, `perf-sentinel capture` (recommended)

The `capture` subcommand receives OTLP and writes a trace file, nothing else. No Collector, no container, no plugin, and the fork stays as it is. Either wrap the test step:

```bash
perf-sentinel capture --output target/traces.json -- mvn verify
perf-sentinel analyze --ci --input target/traces.json
```

or, when the test step cannot be prefixed because your pipeline owns it, run alongside it:

```bash
perf-sentinel capture --output target/traces.json &
CAPTURE=$!
mvn verify
kill -TERM $CAPTURE && wait $CAPTURE
perf-sentinel analyze --ci --input target/traces.json
```

> **Prefix your existing test step, never add a second one.** `capture -- mvn verify` runs the tests once and does not run them again. Adding a new pipeline stage next to the existing one would run the whole integration suite twice, for nothing.

> **A cleaning goal cannot wrap a capture writing into what it cleans.** `capture --output target/traces.json -- mvn clean verify` fails by construction. `capture` opens the file before spawning the command, `clean` then unlinks `target/` under it, and the run ends with an error naming the deleted file rather than a span count for an inode no path points at. Either drop `clean` from the wrapped command, as the recipe above does, or write the trace file outside the cleaned directory (`--output /tmp/traces.json`).

Wrapping is the sturdier of the two: the ports are bound before the command starts, so no export can be lost to a start-up race, and the capture stops when the command exits rather than on a guessed delay. The wrapped command inherits stdout and stderr untouched, and its exit code is propagated, so a failing test run stays a failing job.

The file is NDJSON, one OTLP request per line, the same shape the Collector `file` exporter produces, and format auto-detection reads it with no extra flag. `capture` writes to stderr only, and reports how many spans it received, which is how you tell "no anti-patterns" from "nothing was ever exported". An empty trace file is rejected by `analyze` rather than reported as a clean gate.

Details: [`CLI.md`](./CLI.md), and `perf-sentinel capture --help` for `--listen-address`, `--max-file-size` and `--grace-ms`.

##### Option 2, an OpenTelemetry Collector

If a Collector is already part of the job, keep it. Its `file` exporter produces the same NDJSON, see [Production: via OpenTelemetry Collector](#production-via-opentelemetry-collector). This is the heavier shape, one more container to start and stop, and it makes sense mostly when the same traces have to reach another backend at the same time.

##### Option 3, file export from the agent (2.32.0 and later)

Agent 2.32.0 is the first release bundling SDK 1.66, where `otlp_file/development` implements `output_stream`. At the time of writing (September 2026) only `2.32.0-SNAPSHOT` builds exist. A `file://` value makes the forked JVM write the trace file itself, one OTLP request per line, the NDJSON shape `analyze` reads with no extra flag. Nothing listens on a port, and the fork stays as it is since the file never goes through the command channel.

The exporter is reachable through declarative configuration only. Put a file such as `otel-ci.yaml` next to the POM:

```yaml
file_format: "1.1"
resource:
  attributes:
    - name: service.name
      value: my-service
tracer_provider:
  processors:
    - simple:
        exporter:
          otlp_file/development:
            output_stream: file://${TRACES_FILE}
```

and point the agent at it from the failsafe configuration, in place of the `OTEL_*` variables above:

```xml
<environmentVariables>
  <OTEL_CONFIG_FILE>${project.basedir}/otel-ci.yaml</OTEL_CONFIG_FILE>
  <TRACES_FILE>${project.build.directory}/traces.jsonl</TRACES_FILE>
</environmentVariables>
```

```bash
rm -f target/traces.jsonl
mvn verify
perf-sentinel analyze --ci --input target/traces.jsonl
```

- **The file is opened in append mode.** A second run without `clean` adds its spans to the first one, and every finding comes out twice. Start from an empty file, as the `rm -f` above does. `mvn clean verify` works too, unlike with `capture`, since the forked JVM opens the file after `clean` has run.
- **Once `OTEL_CONFIG_FILE` is set, the agent ignores every `OTEL_*` variable.** Service name, sampler and exporter all come from the YAML.
- **An older agent fails silently.** Agent 2.31.1 with the same configuration ends in `BUILD SUCCESS`, writes no file and diverts the spans to the fork channel. Pin 2.32.0 or later in the `maven-dependency-plugin` block above.

A failing test still leaves a complete, analyzable file.

##### Option 4, no fork at all

`<forkCount>0</forkCount>` removes the fork, therefore the command channel, so `experimental-otlp/stdout` reaches the console and a grep over the build log yields the trace file. It needs no listener, but it has a cost. Test isolation is gone, the capture then carries Maven's own spans alongside the application's, and anything that relied on `<argLine>`, a JaCoCo `@{argLine}` placeholder in particular, must move to `MAVEN_OPTS` or it silently stops applying. Reach for it only when nothing may listen on a port and the agent predates 2.32.0.

**Three neighbouring exporter names do not help here.** `logging` prints a human-readable span summary rather than OTLP JSON, so Perf Sentinel cannot parse it at all. `logging-otlp` does emit OTLP JSON, but through a logger, so each line carries whatever prefix the application's logging setup adds. `otlp_file` and `OTEL_EXPORTER_OTLP_FILE_PATH` do not exist at all, despite reading like they should. The real mechanism is `otlp_file/development` with `output_stream` (Option 3).

---

### Java (Spring Boot 4 starter, spring-boot-starter-opentelemetry)

Spring Boot 4.0 ships its own OpenTelemetry support, the [`spring-boot-starter-opentelemetry`](https://spring.io/blog/2025/11/18/opentelemetry-with-spring-boot/) starter, and the Spring team presents it as its preferred option for Spring Boot applications. There is no agent: the Spring projects create the spans through Micrometer Observation, the Micrometer tracing bridge hands them to the OpenTelemetry SDK that Spring Boot configures, and the SDK exports them over OTLP. Nothing modifies bytecode, so the AOT cache and native images keep working, and the instrumentation always matches the versions of the libraries it observes.

The trade-off is coverage. The starter traces less than the agent out of the box, and its spans follow the Spring conventions rather than the OpenTelemetry ones. The table lists what Perf Sentinel needs and where each part comes from.

| What Perf Sentinel reads           | Out of the box                                        | What to add                                                                                               |
|------------------------------------|-------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| SQL text (`db.query.text`)         | No, Spring Boot does not instrument JDBC              | `datasource-micrometer-spring-boot` and `datasource-micrometer-opentelemetry` ([step 1](#1-dependencies)) |
| Outbound HTTP calls                | Yes, for clients built from the Spring Boot builders  | Build every client from the injected builder ([step 3](#3-outbound-http-clients))                         |
| Inbound route (`http.route`)       | No, the server span carries the raw path only         | A server observation convention ([step 4](#4-inbound-route))                                              |
| RabbitMQ and Kafka publishes       | No, observation is off by default                     | The `observation-enabled` properties ([step 2](#2-configuration))                                         |
| SQL from `@Async` methods and jobs | No, the observation does not follow the thread change | Context propagation, a Quartz listener ([step 5](#5-threads-and-jobs))                                    |
| Code location (`code.*`)           | Only on `@Scheduled` methods                          | Repository observations and call-site attribution ([step 6](#6-code-location))                            |

Run one tracer per JVM. With the Java agent attached to a service that also has the starter, HTTP and messaging spans are emitted twice. Wherever the agent runs, turn the starter's export off with `management.tracing.export.enabled=false`.

#### 1. Dependencies

```xml
<dependency>
    <groupId>org.springframework.boot</groupId>
    <artifactId>spring-boot-starter-opentelemetry</artifactId>
</dependency>
<!-- Spring Boot does not instrument JDBC: one span per statement. -->
<dependency>
    <groupId>net.ttddyy.observation</groupId>
    <artifactId>datasource-micrometer-spring-boot</artifactId>
    <version>2.3.0</version>
</dependency>
<!-- Puts the statement in db.query.text, the attribute Perf Sentinel reads. -->
<dependency>
    <groupId>net.ttddyy.observation</groupId>
    <artifactId>datasource-micrometer-opentelemetry</artifactId>
    <version>2.3.0</version>
</dependency>
```

The 2.x line of [datasource-micrometer](https://github.com/jdbc-observations/datasource-micrometer) targets Spring Boot 4, the 1.x line Spring Boot 3. Keep both modules on the same version. Without the OpenTelemetry module, the statement goes into a `jdbc.query[0]` tag that Perf Sentinel does not read, so the SQL spans are skipped. With it, the statement is sanitized before export (`?` in place of literals), which the sanitizer-aware N+1 detection handles.

#### 2. Configuration

```properties
spring.application.name=my-service
# OTLP over HTTP/protobuf, the default transport: port 4318 and the /v1/traces path.
management.opentelemetry.tracing.export.otlp.endpoint=http://otel-collector:4318/v1/traces
# The default is 0.1, which drops most of the repeated calls N+1 detection relies on.
management.tracing.sampling.probability=1.0
# One span per SQL statement, no connection or result-set spans.
jdbc.includes=query
# Messaging spans and trace propagation through message headers are off by default.
spring.rabbitmq.listener.simple.observation-enabled=true
spring.rabbitmq.template.observation-enabled=true
spring.kafka.listener.observation-enabled=true
spring.kafka.template.observation-enabled=true
# The starter also pushes metrics over OTLP. Turn that off when Prometheus scrapes them.
management.otlp.metrics.export.enabled=false
```

- **No endpoint, no export.** The endpoint has no default. Until it is set, spans are created and propagated but never leave the JVM.
- **`service.name`** comes from `spring.application.name`, and `service.namespace` from `spring.application.group`. Add other resource attributes, such as `k8s.namespace.name` for [`grouping_attributes`](./CONFIGURATION.md), with `management.opentelemetry.resource-attributes.*`.
- **Propagation** produces W3C `traceparent` only and accepts W3C and B3. Change it with `management.tracing.propagation.produce` and `consume`. The older `management.tracing.propagation.type` overrides both when set, so a leftover value silently replaces them.
- **`OTEL_*` variables.** Spring Boot 4.1 maps a subset of them onto these properties, among them `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `OTEL_EXPORTER_OTLP_ENDPOINT` (with `/v1/traces` appended), `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER` and `OTEL_PROPAGATORS`. The other `OTEL_*` variables are ignored. Spring Boot 4.0 reads only `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`.
- **Switching export per environment.** `management.tracing.export.otlp.enabled` turns the OTLP export off without removing the instrumentation, so the same artifact runs everywhere and only the deployment decides where traces go.

#### 3. Outbound HTTP clients

A `RestClient`, `RestTemplate` or `WebClient` is traced, and sends `traceparent`, only when it carries the `ObservationRegistry`. Spring Boot sets it on the builders it auto-configures. A client built from a static factory has neither a span nor trace headers, so its calls are invisible to the N+1 and fan-out detectors, and the downstream service starts a new trace.

```java
@Bean
RestClient inventoryClient(RestClient.Builder builder) { // injected, observed
    return builder.baseUrl("http://inventory").build();
}
// Not traced: RestClient.builder().baseUrl("http://inventory").build()
// Unless the registry is set by hand: RestClient.builder().observationRegistry(registry)
```

These spans name the method and the status in the Micrometer `method` and `status` tags rather than the OpenTelemetry attributes, and Perf Sentinel reads both forms.

#### 4. Inbound route

The default server convention tags the route only on metrics. The span carries the raw path, so findings group by instantiated URL, one endpoint per id, and [acknowledgments](./ACK-WORKFLOW.md#signature-stability-and-service-restarts) lose their stability. Two ways to put `http.route` on the span:

- Declare an `OpenTelemetryServerRequestObservationConvention` bean. The span then follows the OpenTelemetry HTTP conventions, and so do the HTTP server metrics, which are renamed `http.server.request.duration`.
- Extend the default convention and add the route as a high-cardinality key. High-cardinality keys go to traces only, so the Prometheus metrics keep their names and tags.

```java
@Bean
ServerRequestObservationConvention routeConvention() {
    return new DefaultServerRequestObservationConvention() {
        @Override
        public KeyValues getHighCardinalityKeyValues(ServerRequestObservationContext context) {
            KeyValues keyValues = super.getHighCardinalityKeyValues(context);
            String route = context.getPathPattern();
            return route == null ? keyValues : keyValues.and("http.route", route);
        }
    };
}
```

#### 5. Threads and jobs

A query belongs to a trace only if an observation is current on its thread when it runs.

- **`@Async` methods.** On Spring Boot 4.1, set `spring.task.execution.propagate-context=true` for the auto-configured executor. On Spring Boot 4.0, or for an executor you build yourself, register a `ContextPropagatingTaskDecorator`. Capturing every context also hands the caller's security context to the task. To propagate the observation alone:

  ```java
  @Bean
  TaskDecorator observationOnlyTaskDecorator() {
      return new ContextPropagatingTaskDecorator(ContextSnapshotFactory.builder()
              .captureKeyPredicate(ObservationThreadLocalAccessor.KEY::equals)
              .build());
  }
  ```

- **`@Scheduled` methods** get an observation automatically.
- **Quartz jobs** get none, and `@Observed` on a job class does nothing because Quartz instantiates jobs outside Spring AOP. Register a global `JobListener` through a `SchedulerFactoryBeanCustomizer`: open an observation scope in `jobToBeExecuted` and close it in `jobWasExecuted`, since both run on the job thread.

SQL that runs outside any observation (start-up, migrations, a cluster check-in) becomes one root trace per statement. That costs volume and adds statements nothing can act on. An `ObservationPredicate` can drop the `jdbc.*` observations that start with no current observation on the thread. Look at the current observation rather than the parent: with `jdbc.includes=query` the parent of a query is the connection observation, a no-op.

```java
@Bean
ObservationPredicate skipSqlOutsideAnyObservation(ObjectProvider<ObservationRegistry> registry) {
    return (name, context) -> !name.startsWith("jdbc.")
            || registry.getObject().getCurrentObservation() != null;
}
```

#### 6. Code location

The starter puts no `code.*` attribute on HTTP or SQL spans, so findings come without a call site, and the Java suggested fix rests on the `org.springframework.boot` scope and the SQL shape alone (see [Required span attributes](#required-span-attributes)). Perf Sentinel reads the stable `code.function.name`, `code.file.path` and `code.line.number` first, then the legacy names. Over OTLP, an I/O span without any `code.*` attribute takes the code location of its nearest ancestor that has one, up to eight levels up within the same service. Two additions cover most I/O spans.

**A span per repository call.** Spring Data creates no span for a repository method. A `MethodInterceptor` added to every repository proxy can open an observation carrying `code.function.name`, and the SQL the method issues sits under it. Add the advice at position 0 so that the span also covers the flush when the repository commits its own transaction.

```java
@Bean
static BeanPostProcessor repositoryObservation(ObjectProvider<ObservationRegistry> registry) {
    return new BeanPostProcessor() {
        @Override
        public Object postProcessBeforeInitialization(Object bean, String name) {
            if (bean instanceof RepositoryFactoryBeanSupport<?, ?, ?> factoryBean) {
                factoryBean.addRepositoryFactoryCustomizer(factory -> factory.addRepositoryProxyPostProcessor(
                        (proxy, info) -> proxy.addAdvice(0, (MethodInterceptor) invocation -> {
                            ObservationRegistry r = registry.getIfAvailable(() -> ObservationRegistry.NOOP);
                            if (r.getCurrentObservation() == null) {
                                return invocation.proceed(); // no trace to attach to
                            }
                            String function = info.getRepositoryInterface().getName() + "." + invocation.getMethod().getName();
                            return Observation.createNotStarted("repository.invocation", r)
                                    .highCardinalityKeyValue("code.function.name", function)
                                    .observeChecked(invocation::proceed);
                        })));
            }
            return bean;
        }
    };
}
```

**The call site from the stack.** Repository spans leave out lazy loads, proxy initialization and every outbound HTTP call. The application method responsible for them is still on the stack when the span ends, because observation filters run on the calling thread when the observation stops. An `ObservationFilter` on datasource-micrometer's `QueryContext` and on Spring's `ClientRequestObservationContext` can walk the stack with `StackWalker` and keep the first frame that belongs to the application. Three rules keep the result honest:

- **Stop at boundaries.** If a repository call comes first on the stack, leave the span alone: its repository ancestor already names the code. If Hibernate's flush (`org.hibernate.engine.spi.ActionQueue.executeActions`) or a commit (`AbstractPlatformTransactionManager.processCommit`) comes first, the stack does not say where the entity changed, so set nothing.
- **Recognize application classes by where they were loaded from, not by package.** When the service and its shared libraries share a root package, compare each class's `ProtectionDomain` `CodeSource` with the one of the `@SpringBootConfiguration` class, skip generated subclasses (`$$` proxies, Hibernate proxies) and cache the answer per class with `ClassValue`. If the application's location is unknown, treat no class as application code: JDK classes have no `CodeSource` either.
- **Write the line number as an integer.** Observation key values are strings, and Perf Sentinel reads `code.line.number` only as an integer over OTLP. Set the attributes on the tracing span itself, through the `TracingObservationHandler.TracingContext` of the observation context:

  ```java
  TracingObservationHandler.TracingContext tracing = context.get(TracingObservationHandler.TracingContext.class);
  Span span = tracing == null ? null : tracing.getSpan();
  if (span == null || span.isNoop()) {
      return context; // not traced, or not sampled: skip the stack walk
  }
  // frame: the first application frame found by StackWalker
  span.tag("code.function.name", frame.getClassName() + "." + frame.getMethodName());
  span.tag("code.file.path", frame.getFileName());
  span.tag("code.line.number", (long) frame.getLineNumber());
  ```

With both in place, most SQL and HTTP findings of a starter service carry a code location, more than the agent's default, which names repository methods only. Two limits remain. A generic HTTP helper of the service becomes the reported location of every call that goes through it, and an entity getter that triggers a lazy load is reported rather than its caller. The stack walk costs a few microseconds per traced I/O call.

#### 7. Integration tests

`@SpringBootTest` runs inside the Failsafe JVM, so the starter traces integration tests with no agent to attach. Point the fork at [`perf-sentinel capture`](#option-1-perf-sentinel-capture-recommended) through system properties:

```xml
<plugin>
  <groupId>org.apache.maven.plugins</groupId>
  <artifactId>maven-failsafe-plugin</artifactId>
  <configuration>
    <systemPropertyVariables>
      <management.opentelemetry.tracing.export.otlp.endpoint>http://localhost:4318/v1/traces</management.opentelemetry.tracing.export.otlp.endpoint>
      <management.tracing.export.otlp.enabled>true</management.tracing.export.otlp.enabled>
      <management.tracing.sampling.probability>1.0</management.tracing.sampling.probability>
    </systemPropertyVariables>
  </configuration>
</plugin>
```

```bash
perf-sentinel capture --grace-ms 3000 --output target/traces.json -- mvn verify
perf-sentinel analyze --ci --input target/traces.json
```

With no listener, the export fails in the background without failing a test, so this configuration can stay in the POM. The grace period leaves the batch span processor time to flush after the last test.

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

The `SetDbStatementForText = true` option is required for Perf Sentinel to see the query text. Without it, SQL spans are emitted but `db.statement` is empty.

Entity Framework Core uses named bind parameters (`@__param_0`). Since the actual parameter values are not visible in the query template, Perf Sentinel may detect repeated queries as `redundant_sql` (same template, same visible params) rather than `n_plus_one_sql` (same template, different params).

`System.Net.Http` redacts the query string to `?*` by default, so outbound HTTP N+1 loops that vary a query parameter (`?seq=1`, `?seq=2`, ...) reach Perf Sentinel as identical URLs and are detected as `redundant_http` rather than `n_plus_one_http`. To get `n_plus_one_http` on these loops, set `OTEL_DOTNET_EXPERIMENTAL_HTTPCLIENT_DISABLE_URL_QUERY_REDACTION=true` to keep the query string, or model the varying identifier as a path segment (`/api/resource/{id}`). See [LIMITATIONS.md](./LIMITATIONS.md#http-query-string-redaction-and-n1-visibility) for the full rationale.

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

`otelpgx` emits `db.statement` with PostgreSQL native positional parameters (`$1`, `$2`). Perf Sentinel normalizes these to `$?` with empty `params`, which enables the sanitizer-aware N+1 detection path. No additional configuration is needed.

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

`psycopg` emits `db.statement` with Python DB-API `%s` placeholders. Perf Sentinel recognizes `%s` as a driver placeholder, so the sanitizer-aware N+1 detection path fires without additional configuration.

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

`asyncpg` emits `db.statement` with PostgreSQL native positional parameters (`$1`, `$2`). Perf Sentinel normalizes these to `$?` with empty `params`. The `sqlalchemy` instrumentation scope is in the ORM scope allow-list, so the sanitizer-aware N+1 detection fires via the ORM path for this stack.

**Environment variables:**

```yaml
environment:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4317
  OTEL_SERVICE_NAME: fastapi-svc
```

---

### Node.js (Nest.js + Prisma, OTel SDK 0.57)

Nest.js applications use the `@opentelemetry/sdk-node` package with framework-specific instrumentations. Prisma generates the SQL and the `pg` client sends it.

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

### Rust (tracing-opentelemetry 0.31, Diesel, SeaORM)

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

### Ruby (Rails + ActiveRecord, opentelemetry-ruby)

Rails applications use the opentelemetry-ruby instrumentation gems. The `ActiveRecord` instrumentation provides the ORM scope, and the underlying driver instrumentation (`pg`, `mysql2`) emits the SQL `db.statement`.

**Dependencies (Gemfile):**

```ruby
gem 'opentelemetry-sdk'
gem 'opentelemetry-exporter-otlp'
gem 'opentelemetry-instrumentation-rails'
gem 'opentelemetry-instrumentation-active_record'
gem 'opentelemetry-instrumentation-pg'
```

**Initialization (config/initializers/opentelemetry.rb):**

```ruby
require 'opentelemetry/sdk'
require 'opentelemetry/exporter/otlp'
require 'opentelemetry/instrumentation/all'

OpenTelemetry::SDK.configure do |c|
  c.service_name = 'rails-svc'
  c.use 'OpenTelemetry::Instrumentation::Rails'
  c.use 'OpenTelemetry::Instrumentation::ActiveRecord'
  c.use 'OpenTelemetry::Instrumentation::PG', { db_statement: :include }
end
```

The `pg` instrumentation needs `db_statement: :include` (or the default `:obfuscate`, which emits the sanitized template) so the SQL reaches Perf Sentinel. The `OpenTelemetry::Instrumentation::ActiveRecord` scope appears on the span chain and is recognized as an ORM, so the sanitizer-aware N+1 path fires and findings carry ActiveRecord-specific suggested fixes (`includes` / `preload` / `eager_load`).

The `active_record` instrumentation emits this scope only for record-loading queries (`find_by_sql`, `where(...).to_a`). Aggregate queries (`count`, `sum`) carry only the `pg` / `mysql2` driver span, so their findings fall back to the `ruby_generic` fix.

**Environment variables:**

```yaml
environment:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4317
  OTEL_SERVICE_NAME: rails-svc
```

---

### PHP (Laravel / Eloquent, Symfony / Doctrine, opentelemetry-php)

PHP applications use the OpenTelemetry PHP auto-instrumentation extension plus the framework instrumentation packages from `open-telemetry/opentelemetry-php-contrib`. The instrumentations register native scopes (`io.opentelemetry.contrib.php.pdo`, `io.opentelemetry.contrib.php.doctrine`, `io.opentelemetry.contrib.php.laravel`) and set `code.function.name` in `Namespace\Class::method` form, which is what Perf Sentinel keys framework-aware fixes on.

**Dependencies (composer):**

```bash
pecl install opentelemetry
composer require \
  open-telemetry/sdk open-telemetry/exporter-otlp \
  open-telemetry/opentelemetry-auto-pdo \
  open-telemetry/opentelemetry-auto-laravel    # or -auto-symfony + -auto-doctrine
```

**Framework mapping.**

- Laravel/Eloquent: the SQL leaf span is PDO-scoped, but the app-wide `io.opentelemetry.contrib.php.laravel` scope appears on the span chain, so findings carry `php_laravel_eloquent` fixes (`with()` / `load()` eager loading) across every anti-pattern.
- Symfony/Doctrine: the `io.opentelemetry.contrib.php.doctrine` scope is emitted directly on the SQL span (DBAL is instrumented), so SQL findings carry `php_doctrine` fixes (DQL fetch-join). A Symfony app that uses raw PDO instead of Doctrine falls to `php_generic`.

The PDO instrumentation emits the obfuscated SQL template by default, which is enough for detection. The `io.opentelemetry.contrib.php.pdo` scope alone (no Laravel/Doctrine scope) routes to `php_generic`.

**Coming from dd-trace-php?** Bridging through the Collector `datadogreceiver` works for detection but loses the framework signal (no `code.*`, scope is a fixed `Datadog`), so those findings get no framework-aware fix. See [Coming from Datadog](INTEGRATION.md#coming-from-datadog-dd-trace-no-opentelemetry).

**Environment variables:**

```yaml
environment:
  OTEL_PHP_AUTOLOAD_ENABLED: "true"
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector:4317
  OTEL_SERVICE_NAME: php-svc
```

---

## SQL placeholder styles and detection

Different database drivers emit different placeholder syntax in the `db.statement` span attribute. Perf Sentinel's SQL normalizer recognizes all common styles and maps them to `$?` or `?` in the normalized template, with `params` kept empty for parameterized queries. This enables the sanitizer-aware N+1 detection path (which requires `params == []` and a recognized placeholder in the template).

| Placeholder    | Produced by                                                                                                             | Normalized to  | Example           |
|----------------|-------------------------------------------------------------------------------------------------------------------------|----------------|-------------------|
| `?`            | JDBC agent (Java), R2DBC via Java Agent, MySQL Connector/J 8.2+ native OTel, Go `go-sql-driver/mysql`, Node.js `mysql2` | `?`            | `WHERE id = ?`    |
| `$1`, `$2`     | PostgreSQL native (pgx, asyncpg, sqlx, node-pg)                                                                         | `$?`           | `WHERE id = $?`   |
| `%s`           | Python DB-API (psycopg, MySQLdb, PyMySQL, mysql-connector-python)                                                       | `%s` (kept)    | `WHERE id = %s`   |
| `@p0`, `@Name` | .NET (Npgsql, SqlClient, MySqlConnector/Pomelo)                                                                         | `@p0` (kept)   | `WHERE id = @p0`  |
| `:name`        | Oracle, SQLAlchemy named                                                                                                | `:name` (kept) | `WHERE id = :oid` |

**What this means for operators.** No configuration is needed to enable detection for any of these stacks. The normalizer and the `template_has_placeholder` check in the detection pipeline handle the mapping automatically. The key requirement is that the OTel instrumentation emits `db.statement` on SQL spans. If `db.statement` is missing (some instrumentations omit it by default for security reasons), Perf Sentinel cannot detect SQL anti-patterns. Check your instrumentation library's documentation for how to enable statement capture.

**ORM scope markers.** The sanitizer-aware detection path also consults the OTel instrumentation scope (the library name) to decide whether a group of sanitized queries is likely N+1 or just redundant. The following scopes are recognized as ORM-level instrumentations, which raises the confidence that a repeated parameterized query is a loop iteration rather than a cache-warm pattern:

`spring-data`, `hibernate`, `jpa`, `micronaut-data`, `jdbi`, `r2dbc`, `entityframeworkcore`, `entity-framework`, `sqlalchemy`, `django`, `active-record`, `activerecord`, `gorm`, `sequelize`, `prisma`, `typeorm`, `mongoose`, `sea-orm`, `diesel`.

Stacks without an ORM scope (bare driver: `otelpgx`, `asyncpg`, `node-pg`, `psycopg` without Django/SQLAlchemy) rely on the timing-variance and high-occurrence signals instead. See `docs/design/04-DETECTION.md` for the full classification algorithm.
