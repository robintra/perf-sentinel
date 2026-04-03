# Guide d'intégration OTLP

perf-sentinel accepte les traces OpenTelemetry via OTLP (gRPC sur le port 4317, HTTP sur le port 4318).

## Demarrage rapide

```bash
perf-sentinel watch
```

Par défaut, il écoute sur `127.0.0.1:4317` (gRPC) et `127.0.0.1:4318` (HTTP).

## Deux approches d'intégration

| Scénario                                                              | Approche                                                                        | Effort                                      | Modifications des services |
|-----------------------------------------------------------------------|---------------------------------------------------------------------------------|---------------------------------------------|----------------------------|
| **Production : les services envoient déjà des traces a un collector** | Ajouter perf-sentinel comme exporteur dans la config du OTel Collector          | Une ligne de YAML                           | Aucune                     |
| **Dev/staging : pas de collector en place**                           | Instrumenter chaque service pour envoyer les traces directement a perf-sentinel | Configuration par langage (voir ci-dessous) | Variable                   |

Si vos services exportent déjà des traces vers Jaeger, Tempo ou un autre backend via un OpenTelemetry Collector, commencez par l'approche collector : elle ne nécessite aucune modification du code applicatif.

---

## Production : via OpenTelemetry Collector

Si vous avez déjà un [OTel Collector](https://opentelemetry.io/docs/collector/), vous pourrez ajouter perf-sentinel comme exporteur OTLP supplémentaire. Votre pipeline de tracing existant (Jaeger, Tempo, etc.) continue de fonctionner ; perf-sentinel analyse une copie des mêmes spans.

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

Cette approche est la cible pour les déploiements en production car :
- Zero modification de code dans vos services
- Pas de rebuild, pas de redéploiement
- Fonctionne quel que soit le langage (Java, C#, Rust, Go, Python, Node.js)
- Le sampling et le filtrage se font au niveau du collector
- perf-sentinel peut être ajouté ou retiré sans toucher au code applicatif

> **Note :** ce chemin d'intégration n'a pas encore été validé de bout en bout. L'instrumentation directe par langage décrite ci-dessous a été testée et validée sur de vrais microservices.

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

## Mode CI (analyse batch)

Pour les pipelines CI, utilisez le mode batch au lieu du mode daemon :

```bash
perf-sentinel analyze --ci --input traces.json
```

Le code de sortie est non-zero si le quality gate echoue. Configurez les seuils dans `.perf-sentinel.toml` :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30
```

---

## Formats d'ingestion

perf-sentinel auto-détecte le format d'entrée avec `perf-sentinel analyze --input` :

| Format                         | Détection                                             | Exemple                    |
|--------------------------------|-------------------------------------------------------|----------------------------|
| **Natif** (perf-sentinel JSON) | Tableau d'objets avec champ `"type"`                  | Format par défaut          |
| **Jaeger JSON**                | Objet avec clé `"data"` contenant `"spans"`           | Exporté depuis l'UI Jaeger |
| **Zipkin JSON v2**             | Tableau d'objets avec `"traceId"` + `"localEndpoint"` | Exporté depuis l'UI Zipkin |

Aucun flag `--format` n'est nécessaire pour l'entrée : le format est détecté automatiquement depuis les premiers octets du fichier.

```bash
# Export Jaeger
perf-sentinel analyze --input jaeger-export.json --ci

# Export Zipkin
perf-sentinel analyze --input zipkin-traces.json --ci
```

## Mode explain

Pour débugger une trace spécifique, utilisez la sous-commande `explain` :

```bash
perf-sentinel explain --input traces.json --trace-id abc123-def456
```

Cela produit une vue arborescente de la trace avec les findings annotés en ligne. Utilisez `--format json` pour une sortie structurée.

## Export SARIF

Pour l'intégration avec GitHub ou GitLab code scanning, exportez les findings en SARIF v2.1.0 :

```bash
perf-sentinel analyze --input traces.json --format sarif > results.sarif
```

Chaque finding est mappé vers un résultat SARIF avec `ruleId`, `level` et `logicalLocations` (service + endpoint).

---

## Troubleshooting

### Aucun event reçu (`events_processed_total = 0`)

1. **Vérifiez la connectivité.** Depuis le container : `curl http://host.docker.internal:4318/metrics`.
2. **Vérifiez l'adresse d'écoute.** perf-sentinel écoute sur `127.0.0.1` par défaut. Pour l'accès Docker, configurez `listen_address = "0.0.0.0"` dans `.perf-sentinel.toml` ou lancez-le nativement sur le host.
3. **Vérifiez le protocole.** Le Java Agent utilise gRPC par défaut (port 4317).

### Events reçus mais aucun finding

1. **Vérifiez les attributs de span.** perf-sentinel ne traite que les spans avec `db.statement`/`db.query.text` (SQL) ou `http.url`/`url.full` (HTTP).
2. **Vérifiez les seuils de détection.** Le seuil N+1 par défaut est 5 occurrences du même template normalisé dans la même trace.
3. **Vérifiez la normalisation des URLs.** perf-sentinel remplace les segments numériques par `{id}` et les UUIDs par `{uuid}`. Les identifiants texte ne sont pas normalisés.

### Erreur AOT cache avec le Java Agent

Le Java Agent (`-javaagent:`) est incompatible avec les AOT caches JEP 483. Désactivez le cache AOT quand l'agent est actif (voir la section Java ci-dessus).

### Le starter Spring Boot ne capture pas les appels HTTP sortants

Le `spring-boot-starter-opentelemetry` (Spring Boot 4) fait le pont Micrometer vers OTel mais n'instrumente pas complètement les appels sortants. Utilisez le Java Agent.
