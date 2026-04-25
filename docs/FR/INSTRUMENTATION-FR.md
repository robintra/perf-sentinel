# Guide d'instrumentation perf-sentinel

Ce guide couvre les parties du pipeline qui transforment l'activité runtime d'une application en l'entrée OTLP / JSON consommée par perf-sentinel. Pour une vue d'ensemble de bout en bout, les quatre topologies supportées et les quatre démarrages rapides, voir [INTEGRATION-FR.md](./INTEGRATION-FR.md). Pour le côté CI de l'intégration (mode CI, recettes GitHub Actions / GitLab CI / Jenkins, déploiement du rapport HTML interactif, détection de régressions sur PR), voir [CI-FR.md](./CI-FR.md).

## Sommaire

- [Déploiement Kubernetes](#déploiement-kubernetes) : manifests pour le daemon et le sidecar OTel Collector.
- [Intégrations cloud](#intégrations-cloud) : AWS X-Ray, GCP Cloud Trace, Azure Application Insights, Jaeger / Tempo / Zipkin self-hosted.
- [Production : via OpenTelemetry Collector](#production--via-opentelemetry-collector) : configuration du collector central, sampling et précision de détection.
- [Attributs de span requis](#attributs-de-span-requis) : les conventions sémantiques OTel legacy et stables que perf-sentinel lit.
- [Dev/staging : instrumentation par langage](#devstaging--instrumentation-par-langage) : mise en place pas à pas pour Java, Quarkus, .NET, Rust.

## Déploiement Kubernetes

Un chart Helm packagé est disponible sous [`charts/perf-sentinel/`](../../charts/perf-sentinel/). Voir [HELM-DEPLOYMENT-FR.md](./HELM-DEPLOYMENT-FR.md) pour le guide d'installation complet et [`examples/helm/`](../../examples/helm/) pour un exemple complet qui compose le chart avec le chart upstream OpenTelemetry Collector. Les manifests bruts ci-dessous restent utiles aux utilisateurs qui préfèrent déployer sans Helm.

perf-sentinel se déploie comme un Deployment Kubernetes standard derrière un Service. L'OTel Collector tourne en DaemonSet (par noeud) ou Deployment (centralisé), transmettant les traces à perf-sentinel.

### Manifests minimaux

```yaml
# Deployment perf-sentinel
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

### Config exporteur OTel Collector

Dans votre config Collector existante (DaemonSet ou Deployment), ajoutez perf-sentinel comme exporteur :

```yaml
exporters:
  otlp/perf-sentinel:
    endpoint: perf-sentinel.monitoring:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      exporters: [otlp/perf-sentinel, otlp/votre-backend]
```

### Instrumentation des applications

Les services envoient les traces au Collector via la variable d'env standard `OTEL_EXPORTER_OTLP_ENDPOINT`. Si vous utilisez l'OTel Operator, elle est injectée automatiquement. Sinon, définissez-la dans le spec de votre Deployment :

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

### ServiceMonitor Prometheus

Si vous utilisez le Prometheus Operator, scrapez les métriques perf-sentinel avec un ServiceMonitor :

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

## Intégrations cloud

perf-sentinel est agnostique au cloud : il reçoit des traces OTLP standard. L'essentiel est de router une copie de vos traces vers perf-sentinel en parallèle de votre backend de traces cloud.

### AWS (X-Ray + OTel Collector)

AWS X-Ray utilise un format propriétaire, mais l'[AWS Distro for OpenTelemetry (ADOT)](https://aws-otel.github.io/) Collector peut exporter à la fois vers X-Ray et vers perf-sentinel :

```yaml
# Config ADOT Collector
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

Déployez perf-sentinel comme tâche ECS ou Deployment EKS. Pour ECS, utilisez l'image Docker basée sur `scratch` (`ghcr.io/robintra/perf-sentinel:latest`).

### GCP (Cloud Trace + OTel Collector)

GCP Cloud Trace supporte l'ingestion OTLP nativement. Utilisez l'OTel Collector standard avec l'exporteur `googlecloud` et l'exporteur perf-sentinel :

```yaml
exporters:
  googlecloud:
    project: mon-projet-gcp
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

Déployez perf-sentinel comme service Cloud Run ou Deployment GKE. Pour Cloud Run, exposez les ports 4317 (gRPC) et 4318 (HTTP).

### Azure (Application Insights + OTel Collector)

Azure Monitor supporte OTLP via l'[Azure Monitor OpenTelemetry Exporter](https://learn.microsoft.com/en-us/azure/azure-monitor/app/opentelemetry-configuration). Routez les traces vers Azure et perf-sentinel :

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

Déployez perf-sentinel comme Deployment AKS ou Azure Container Instance.

### Auto-hébergé (Jaeger, Tempo, Zipkin)

Si vous utilisez un backend de traces auto-hébergé, l'approche OTel Collector fonctionne de manière identique. Ajoutez perf-sentinel comme exporteur OTLP supplémentaire à côté de votre exporteur backend existant. Alternativement, utilisez le mode batch de perf-sentinel avec des fichiers de traces exportés depuis l'UI Jaeger (`--input jaeger-export.json`) ou Zipkin (`--input zipkin-traces.json`), les formats sont auto-détectés.

---

## Production : via OpenTelemetry Collector

Si vous avez déjà un [OTel Collector](https://opentelemetry.io/docs/collector/), vous pourrez ajouter perf-sentinel comme exporteur OTLP supplémentaire. Votre pipeline de tracing existant (Jaeger, Tempo, etc.) continue de fonctionner, perf-sentinel analyse une copie des mêmes spans.

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
      exporters: [otlp/perf-sentinel, otlp/jaeger]   # envoyer aux deux
```

Cette approche est recommandée pour les déploiements en production car :
- Zero modification de code dans vos services
- Pas de rebuild, pas de redéploiement
- Fonctionne quel que soit le langage (Java, C#, Rust, Go, Python, Node.js)
- Le sampling et le filtrage se font au niveau du collector
- perf-sentinel peut être ajouté ou retiré sans toucher au code applicatif

Une configuration de référence complète est fournie dans [`examples/otel-collector-config.yaml`](../examples/otel-collector-config.yaml) avec un fichier Docker Compose associé dans [`examples/docker-compose-collector.yml`](../examples/docker-compose-collector.yml).

### Mise en place de bout en bout avec Docker Compose

1. Démarrer la stack :

```bash
docker compose -f examples/docker-compose-collector.yml up -d
```

2. Configurer vos applications pour exporter les traces OTLP vers le collector :
   - gRPC : `localhost:4317`
   - HTTP : `localhost:4318`

3. Vérifier que perf-sentinel reçoit des spans :

```bash
curl -s http://localhost:14318/metrics | grep perf_sentinel_events_processed_total
```

4. Voir les findings émis par perf-sentinel sur stdout :

```bash
docker compose -f examples/docker-compose-collector.yml logs -f perf-sentinel
```

### Sampling et filtrage

Pour les environnements à fort trafic, l'OTel Collector supporte le sampling tail-based et le filtrage pour réduire le volume de traces transmises à perf-sentinel.

**Sampling tail-based** : conserve les traces complètes selon des critères évalués après l'arrivée de tous les spans :

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

**Processeur filter** : supprime les spans correspondant à des conditions spécifiques :

```yaml
processors:
  filter:
    error_mode: ignore
    traces:
      span:
        - 'attributes["service.name"] == "health-check"'
```

Ajouter le processeur au pipeline :

```yaml
service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [tail_sampling, batch]
      exporters: [otlp/perf-sentinel]
```

**Sampling et précision de détection**.

La détection d'anti-patterns repose sur du comptage d'événements. Le sampling qui supprime des événements affecte directement les patterns que perf-sentinel peut signaler.

- **Dans une trace conservée, tous les spans sont préservés**. OTel et Jaeger samplent par-trace, pas par-span, donc une boucle N+1, un hop vers un service bavard ou un fanout à l'intérieur d'une seule requête se détectent proprement tant que la trace parente est conservée.
- **Le head-sampling casse les détections count-based**. Une politique head-sampling à 1% drop 99% des traces avant qu'elles n'arrivent au collector, donc une boucle N+1 de 50 appels est observée comme 3 appels, bien sous tout seuil raisonnable. Pareil pour les services bavards, le fanout, les parallélisables sérialisés, la saturation de pool. Tout ce qui est piloté par seuil est silencieusement sous-reporté.
- **Le tail-sampling reste compatible avec la détection** parce que les politiques qu'on écrirait pour la revue d'incident (garder les erreurs, garder les traces lentes, garder certains services) sont exactement celles qui font remonter les anti-patterns. L'exemple [`tail_sampling`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/processor/tailsamplingprocessor) ci-dessus garde tout sous ces politiques plus un échantillonnage probabiliste de 10% du reste.
- **Les runs CI doivent garder 100% des traces**. Le volume est bas (un run de tests d'intégration), le coût de l'instrumentation complète est négligeable, et louper une régression à cause du sampling annule l'intérêt du gate CI. Les sections Quick start ci-dessus supposent un sampling à 100%.
- **Le mode `pg-stat` est immunisé contre le sampling**. `pg_stat_statements` agrège les compteurs de requêtes côté serveur dans PostgreSQL, indépendamment de ce que le tracer applicatif a capturé. Une requête qui s'exécute 10 000 fois apparaît comme 10 000 appels même si 99% des traces parentes ont été dropées au head. Utiliser `perf-sentinel pg-stat ...` (ou passer `--pg-stat` à `analyze` et `report`) comme fallback quand on ne peut pas faire confiance au volume de traces, ou comme signal principal pour les chemins de code que le tracer ne couvre même pas.

> **Note :** le sampling tail-based nécessite l'image `otel/opentelemetry-collector-contrib` (pas l'image core).

---

## Attributs de span requis

perf-sentinel détecte les anti-patterns I/O en examinant des attributs de span spécifiques. Les conventions sémantiques legacy et stables d'[OpenTelemetry](https://opentelemetry.io/docs/specs/semconv/) sont toutes deux supportées.

| Usage             | Attribut legacy (pre-1.21) | Attribut stable (1.21+)     | Exemple                                   |
|-------------------|----------------------------|-----------------------------|-------------------------------------------|
| Texte requête SQL | `db.statement`             | `db.query.text`             | `SELECT * FROM player WHERE game_id = 42` |
| Système SQL       | `db.system`                | `db.system`                 | `postgresql`, `mysql`                     |
| URL cible HTTP    | `http.url`                 | `url.full`                  | `http://account-svc:5000/api/account/123` |
| Méthode HTTP      | `http.method`              | `http.request.method`       | `GET`, `POST`                             |
| Statut HTTP       | `http.status_code`         | `http.response.status_code` | `200`, `404`                              |
| Endpoint source   | `http.route`               | `http.route`                | `POST /api/game/{id}/start`               |
| Nom du service    | `service.name` (ressource) | `service.name` (ressource)  | `game`, `account-svc`                     |

Les spans qui n'ont ni attribut SQL ni attribut HTTP sont ignorés. Les agents OTel modernes (v2.x) émettent la convention stable par défaut. Les agents plus anciens émettent la convention legacy. perf-sentinel gère les deux de manière transparente.

---

## Dev/staging : instrumentation par langage

Quand aucun OTel Collector n'est disponible, instrumentez les services directement. Les guides ci-dessous sont ordonnes du plus simple au plus complexe.

### Java (OpenTelemetry Java Agent)

Le [Java Agent OTel](https://opentelemetry.io/docs/zero-code/java/agent/) instrumente JDBC, R2DBC, les clients HTTP, Spring Web et la plupart des frameworks automatiquement, sans modification de code. C'est l'approche la plus proche du plug and play.

#### 1. Téléchargez l'agent

```bash
curl -L -o opentelemetry-javaagent.jar \
  https://github.com/open-telemetry/opentelemetry-java-instrumentation/releases/latest/download/opentelemetry-javaagent.jar
```

#### 2. Lancez votre application avec l'agent

```bash
export JAVA_TOOL_OPTIONS="-javaagent:/path/to/opentelemetry-javaagent.jar"
export OTEL_SERVICE_NAME=mon-service
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_TRACES_SAMPLER=always_on
export OTEL_METRICS_EXPORTER=none
export OTEL_LOGS_EXPORTER=none
java -jar my-app.jar
```

L'agent capture automatiquement :
- `db.query.text` depuis JDBC (Spring Data JPA, Hibernate) et R2DBC (Spring WebFlux réactif)
- `url.full` depuis les clients HTTP (WebClient, RestTemplate, HttpClient)
- `http.route` depuis Spring MVC et Spring WebFlux (requêtes entrantes)
- Propagation du trace context entre les appels asynchrones, les chaînes réactives et les appels inter-services

Validé sur Spring Boot 4 avec WebFlux/R2DBC, Virtual Threads/JPA et MVC/JDBC standard.

#### Limitations connues

**Incompatibilité Project Leyden / AOT cache.** Le flag `-javaagent:` est incompatible avec les AOT caches JEP 483. Désactivez le cache AOT quand l'agent est actif :

```bash
if echo "$JAVA_TOOL_OPTIONS" | grep -q "javaagent"; then
  exec java -jar /app/my-app.jar
else
  exec java -XX:AOTCache=/app/app.aot -jar /app/my-app.jar
fi
```

**Le starter Spring Boot ne suffit pas.** Le `spring-boot-starter-opentelemetry` (Spring Boot 4) n'instrumente pas les appels sortants `WebClient` ou `RestTemplate` avec propagation du trace context. Utilisez le Java Agent pour une détection N+1 HTTP complète.

---

### Java (Quarkus + quarkus-opentelemetry)

Pour les applications Quarkus (y compris les images natives GraalVM où le Java Agent ne peut pas être utilisé), ajoutez l'extension `quarkus-opentelemetry` :

```xml
<dependency>
    <groupId>io.quarkus</groupId>
    <artifactId>quarkus-opentelemetry</artifactId>
</dependency>
```

Configurez dans `application.properties` :

```properties
quarkus.otel.exporter.otlp.endpoint=${OTLP_GRPC_ENDPOINT:http://localhost:4317}
quarkus.otel.exporter.otlp.protocol=grpc
quarkus.otel.service.name=mon-service
quarkus.otel.enabled=${OTEL_ENABLED:false}
quarkus.otel.metrics.exporter=none
quarkus.otel.logs.exporter=none
```

Activez le tracing en définissant `OTEL_ENABLED=true` et `OTLP_GRPC_ENDPOINT` dans votre environnement. Pour les images natives, utilisez le préfixe `QUARKUS_` pour les surcharges au runtime.

---

### .NET (ASP.NET Core + OpenTelemetry SDK)

Compatible NativeAOT (`PublishAot=true`). Nécessite l'ajout de packages NuGet et ~15 lignes dans `Program.cs`.

```xml
<PackageReference Include="OpenTelemetry.Extensions.Hosting" Version="1.12.0" />
<PackageReference Include="OpenTelemetry.Instrumentation.AspNetCore" Version="1.12.0" />
<PackageReference Include="OpenTelemetry.Instrumentation.Http" Version="1.12.0" />
<PackageReference Include="OpenTelemetry.Exporter.OpenTelemetryProtocol" Version="1.12.0" />
```

Pour les projets .NET 8, utilisez la version 1.9.0 au lieu de 1.12.0 pour éviter les conflits de dépendances.

```csharp
var otlpEndpoint = Environment.GetEnvironmentVariable("OTLP_GRPC_ENDPOINT");
if (!string.IsNullOrEmpty(otlpEndpoint))
{
    builder.Services.AddOpenTelemetry()
        .ConfigureResource(r => r.AddService("mon-service"))
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

Pour la détection des requêtes SQL, ajoutez l'instrumentation correspondant à votre couche d'accès aux données :

- **Entity Framework Core** (MySQL, PostgreSQL, SQLite) : `.AddEntityFrameworkCoreInstrumentation(o => o.SetDbStatementForText = true)` avec `OpenTelemetry.Instrumentation.EntityFrameworkCore`
- **SqlClient** (SQL Server) : `.AddSqlClientInstrumentation(o => o.SetDbStatementForText = true)` avec `OpenTelemetry.Instrumentation.SqlClient`

L'option `SetDbStatementForText = true` est requise pour que perf-sentinel voie le texte des requêtes. Sans elle, les spans SQL sont émis mais `db.statement` est vide.

Note : Entity Framework Core utilise des paramètres nommés (`@__param_0`). Les valeurs réelles n'étant pas visibles dans le template, perf-sentinel peut detecter des requêtes répétées comme `redundant_sql` plutôt que `n_plus_one_sql`.

---

### Rust (tracing + opentelemetry-otlp)

Nécessite l'ajout de 4 crates et ~20 lignes de code d'initialisation. Utilisez `provider.tracer()` (pas `global::tracer()`) pour éviter le problème de trait bound `PreSampledTracer`.

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

let tracer = provider.tracer("mon-service");
let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

tracing_subscriber::registry()
    .with(tracing_subscriber::fmt::layer())
    .with(otel_layer)
    .init();
```

Pour que perf-sentinel détecte les anti-patterns SQL, ajoutez `db.statement` à vos spans de requêtes manuellement :

```rust
let _span = tracing::info_span!("db.query",
    db.statement = "SELECT * FROM player WHERE game_id = 42",
    db.system = "mysql"
);
```

---

