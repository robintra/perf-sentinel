# Guide d'intégration

perf-sentinel accepte les traces OpenTelemetry via OTLP (gRPC sur 4317, HTTP sur 4318). Ce guide vous accompagne de zéro jusqu'à votre premier finding pour chaque topologie de déploiement.

> **Voir aussi.** Pas familier d'OpenTelemetry ? L'[introduction à OpenTelemetry](INSTRUMENTATION-FR.md#introduction-à-opentelemetry) définit OTLP, le Collector, les spans et les traces en quelques paragraphes. À lire en premier si un de ces termes ne vous est pas familier.

## Sommaire

- [Choisissez votre topologie](#choisissez-votre-topologie) : tableau comparatif des quatre modes de déploiement pris en charge.
- [Démarrage rapide : CI batch](#démarrage-rapide--ci-batch) : exécuter perf-sentinel depuis un pipeline CI contre une fixture de traces.
- [Démarrage rapide : collector central](#démarrage-rapide--collector-central) : déploiement production via OpenTelemetry Collector.
- [Vous venez de Datadog](#vous-venez-de-datadog-dd-trace-sans-opentelemetry) : faire le pont avec le trafic dd-trace via un OpenTelemetry Collector quand vous n'avez pas d'instrumentation OTel.
- [Démarrage rapide : sidecar](#démarrage-rapide--sidecar) : débogage d'un seul service en dev ou staging.
- [Démarrage rapide : daemon direct](#démarrage-rapide--daemon-direct) : développement local.
- [Pour aller plus loin](#pour-aller-plus-loin) : pointeurs vers INSTRUMENTATION-FR.md et CI-FR.md pour les sujets côté application et côté CI.
- [Formats d'ingestion](#formats-dingestion) : règles d'auto-détection JSON natif, OTLP, Jaeger, Zipkin, Tempo, pg_stat_statements et performance_schema MySQL.
- [Mode explain](#mode-explain) : vue arborescente d'une trace.
- [Export SARIF](#export-sarif) : sortie SARIF v2.1.0 pour le code scanning GitHub ou GitLab.
- [Champ de confiance sur les findings](#champ-de-confiance-sur-les-findings) : champ `confidence` JSON / SARIF pour les consommateurs aval.
- [API de requêtage du daemon](#api-de-requêtage-du-daemon) : API HTTP sur le port OTLP HTTP, voir aussi [QUERY-API-FR.md](./QUERY-API-FR.md) pour la référence complète.
- [Configuration avancée du scoring carbone](#configuration-avancée-du-scoring-carbone) : scoring multi-région, Scaphandre, énergie cloud-native, Electricity Maps, calibration.
- [Intégration Tempo](#intégration-tempo) : interroger un backend Grafana Tempo directement avec `perf-sentinel tempo`.
- [Intégration API Jaeger query](#intégration-api-jaeger-query-jaeger-et-victoria-traces) : Jaeger upstream et Victoria Traces via une seule sous-commande.
- [Dépannage](#dépannage) : problèmes d'ingestion et de détection courants.

## Choisissez votre topologie

| Topologie                                                     | Idéal pour                         | Effort         | Modifications des services      |
|---------------------------------------------------------------|------------------------------------|----------------|---------------------------------|
| **[CI batch](#démarrage-rapide--ci-batch)**                   | Pipelines CI, vérifications de PR  | Le plus faible | Aucune (fichiers de traces)     |
| **[Collector central](#démarrage-rapide--collector-central)** | Production, multi-services         | Faible         | Aucune (config YAML uniquement) |
| **[Sidecar](#démarrage-rapide--sidecar)**                     | Dev/staging, débogage d'un service | Faible         | Aucune (Docker uniquement)      |
| **[Daemon direct](#démarrage-rapide--daemon-direct)**         | Dev local, essais rapides          | Moyen          | Variables d'env par langage     |

---

## Démarrage rapide : CI batch

Exécutez perf-sentinel dans votre pipeline CI pour détecter les requêtes N+1 avant qu'elles n'atteignent la production. Pas de daemon, pas de Docker, juste un binaire qui lit un fichier de traces et retourne le code 1 quand le quality gate échoue.

### Installation

```bash
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64
sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel
```

### Configurer les seuils

Créez `.perf-sentinel.toml` à la racine de votre projet :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # zéro tolérance pour les N+1 SQL
io_waste_ratio_max = 0.30          # max 30% d'I/O évitables

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500

[green]
enabled = true
default_region = "eu-west-3"       # optionnel : active les estimations gCO2eq
# surcharges par service pour les déploiements multi-région
# [green.service_regions]
# "api-us"   = "us-east-1"
# "api-asia" = "ap-southeast-1"
```

La sortie CO₂ est structurée (`green_summary.co2.total.{low,mid,high}` plus un tag de méthodologie SCI v1.0, incertitude multiplicative 2×). Le scoring multi-région s'active automatiquement quand les spans portent l'attribut `cloud.region`. Voir `docs/FR/CONFIGURATION-FR.md` et [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#précision-des-estimations-carbone).

### Collecter les traces

Exportez les traces depuis vos tests d'intégration. perf-sentinel détecte automatiquement les formats JSON natif, OTLP JSON, Jaeger et Zipkin v2.

### Analyser

```bash
perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
```

Le processus affiche un rapport JSON sur stdout et retourne le code 0 (succès) ou 1 (échec). Ajoutez ceci à votre job CI :

```yaml
# Exemple GitLab CI
perf:sentinel:
  stage: quality
  script:
    - perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
  artifacts:
    paths: [perf-sentinel-report.json]
    when: always
  allow_failure: true   # commencez en warning, retirez une fois les seuils calibrés
```

### Investiguer les findings

```bash
# Rapport coloré en terminal
perf-sentinel analyze --input traces.json --config .perf-sentinel.toml

# Vue arborescente d'une trace spécifique
perf-sentinel explain --input traces.json --trace-id <trace-id>

# TUI interactif
perf-sentinel inspect --input traces.json

# SARIF pour GitHub/GitLab code scanning
perf-sentinel analyze --input traces.json --format sarif > results.sarif

# Dashboard HTML single-file pour l'exploration post-mortem en navigateur
perf-sentinel report --input traces.json --output report.html
```

Le dashboard HTML est documenté dans [`HTML-REPORT-FR.md`](./HTML-REPORT-FR.md), avec la liste complète des options, les raccourcis clavier, l'export CSV et le pipeline d'instantané `/api/export/report` du daemon live.

---

## Démarrage rapide : collector central

Déploiement production où les services envoient les traces à un OpenTelemetry Collector. Zéro modification de code, uniquement de la configuration YAML.

### Démarrer perf-sentinel + collector

```bash
# Récupérer le fichier compose d'exemple (sans cloner le dépôt), puis le démarrer
curl -o docker-compose.yml https://raw.githubusercontent.com/robintra/perf-sentinel/main/examples/docker-compose-collector.yml
docker compose up -d
```

L'URL brute suit `main`. Remplacez `main` par un tag de release (par exemple `v0.8.13`) pour figer une version précise.

Cela démarre un OTel Collector sur 4317 (gRPC) + 4318 (HTTP) et perf-sentinel en mode watch derrière lui.

### Pointer vos services vers le collector

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

Si vos services exportent déjà vers un collector existant, ajoutez perf-sentinel comme second exporter :

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

### Générer du trafic et voir les findings

```bash
docker compose logs -f perf-sentinel
```

Les findings sont émis en NDJSON sur stdout une fois le TTL des traces expiré (30s par défaut).

### Surveiller avec Prometheus + Grafana

Les métriques sont exposées à `http://localhost:14318/metrics` avec des exemplars OpenMetrics (clic direct vers votre backend de traces) :

```yaml
# prometheus.yml
scrape_configs:
  - job_name: perf-sentinel
    static_configs:
      - targets: ['perf-sentinel:4318']
```

Métriques clés : `perf_sentinel_findings_total{type, severity, service, grouping}`, `perf_sentinel_io_waste_ratio`, `perf_sentinel_events_processed_total`, `perf_sentinel_traces_analyzed_total`, `perf_sentinel_slow_duration_seconds{type, service, grouping}`. Voir [`METRICS-FR.md`](./METRICS-FR.md) pour le schéma complet et [`examples/otel-collector-config.yaml`](../../examples/otel-collector-config.yaml) pour la config du collector.

---

## Vous venez de Datadog (dd-trace, sans OpenTelemetry)

perf-sentinel ingère de l'OTLP, pas le format APM natif de Datadog, et n'embarque aucun adaptateur Datadog. Si vos services sont instrumentés avec **dd-trace** (le traceur propriétaire de Datadog) et que vous n'avez pas d'instrumentation OpenTelemetry, faites le pont avec un OpenTelemetry Collector équipé du [`datadogreceiver`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/receiver/datadogreceiver/README.md). Ce receiver implémente l'API d'intake de traces de l'agent Datadog sur le port 8126, convertit les spans dd-trace en OTLP, et un exporter `otlp` transmet une copie à perf-sentinel. Aucun changement de code applicatif n'est requis : vous repointez dd-trace vers le Collector, et vous pouvez continuer à envoyer vers Datadog en parallèle.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/dd-trace-bridge_dark.svg">
  <img alt="Topologie du pont dd-trace : le datadogreceiver du Collector transmet l'OTLP au daemon watch, à un backend Tempo/Jaeger tiré en batch par tempo/jaeger-query, ou vers un exporter file dont le dump OTLP JSON alimente analyze --input." src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/dd-trace-bridge.svg">
</picture>

perf-sentinel lit nativement la ressource Datadog (`dd.span.Resource`, où dd-trace laisse le SQL obfusqué). La détection SQL fonctionne donc tant que chaque span conserve un signal base de données : la clé stable `db.system.name` (ce qu'émettent les versions récentes du receiver), l'ancienne `db.system`, ou le tag dd-trace `db.type`. Aucun remappage d'attributs dans le Collector n'est nécessaire. Si une version du Collector les retire toutes, ajoutez un processor `transform` qui en restaure une pour que le garde-fou se déclenche.

**Réserve sur N+1 contre requêtes redondantes.** dd-trace pré-obfusque le SQL (les littéraux sont déjà `?`), donc les paramètres par requête qui distinguent un N+1 d'une requête légitimement répétée ont disparu avant que perf-sentinel ne les voie. En mode de détection `auto` par défaut, un vrai N+1 aux durées de requête uniformes peut apparaître comme `redundant_sql` plutôt que `n_plus_one_sql`. Réglez `[detection] sanitizer_aware_classification = "strict"` pour récupérer les cas à forte occurrence (3 fois le `n_plus_one_threshold` configuré, soit 15 requêtes identiques ou plus à la valeur par défaut de 5). Le scoring de gaspillage et de carbone est identique pour les deux types de finding.

**PHP (Laravel, Symfony) via dd-trace-php.** La même réserve d'obfuscation s'applique : un vrai N+1 Laravel ou Symfony peut apparaître comme `redundant_sql` en mode `auto`, utilisez donc `sanitizer_aware_classification = "strict"`. Le pont perd aussi le signal de framework : le `datadogreceiver` pose un scope d'instrumentation `Datadog` fixe et ne mappe aucun attribut `code.*`, donc les findings dd-trace-php n'ont pas de `suggested_fix` adapté au framework (ils retombent sur `php_generic` ou restent non enrichis). Les corrections spécifiques Laravel/Eloquent et Symfony/Doctrine exigent l'instrumentation OpenTelemetry PHP native (scopes `io.opentelemetry.contrib.php.*`), voir [INSTRUMENTATION-FR.md](INSTRUMENTATION-FR.md).

La mention "OpenTelemetry compliant" de Datadog est souvent mal comprise. Elle désigne en général l'agent Datadog qui **ingère** de l'OTLP (entrant, depuis des SDK OTel), ce qui est le sens inverse. Faire **sortir** les données dd-trace vers un backend OTLP passe par le chemin Collector ci-dessous.

> **Stabilité.** La prise en charge des traces du `datadogreceiver` est en **alpha** dans opentelemetry-collector-contrib (en 2026). Le receiver convient à l'évaluation et aux preuves de concept. Surveillez-le si vous le maintenez en permanence devant la production.

Config du Collector (dd-trace en entrée, OTLP en sortie vers perf-sentinel) :

```yaml
receivers:
  datadog:
    endpoint: 0.0.0.0:8126          # intake APM dd-trace
exporters:
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true
  # datadog:                        # optionnel : garder votre backend Datadog existant
  #   api:
  #     key: ${DD_API_KEY}
service:
  pipelines:
    traces:
      receivers: [datadog]
      exporters: [otlp/perf-sentinel]   # ajouter `datadog` ici pour dupliquer vers les deux
```

Pointez dd-trace vers le Collector au lieu de l'agent Datadog :

```bash
DD_TRACE_AGENT_URL=http://otel-collector:8126
```

perf-sentinel reçoit alors une copie de chaque trace et les findings sont émis en NDJSON, exactement comme dans la topologie collector central ci-dessus.

**Batch plutôt que le daemon.** Le chemin Collector ci-dessus alimente le daemon `watch`. Pour un `analyze` ponctuel à la place, la voie la plus simple depuis la 0.9.5 est l'exporter [`file` du Collector](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/exporter/fileexporter/README.md) : sa sortie OTLP JSON (objet unique ou NDJSON, une requête par ligne) est auto-détectée par `analyze --input` comme n'importe quel fichier de traces. Sinon, routez le pont vers un backend Jaeger ou Tempo et tirez-en un lot :

```bash
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service checkout --lookback 15m
perf-sentinel tempo        --endpoint http://tempo:3200   --service checkout --lookback 15m
```

Une trace exportée au format JSON Jaeger depuis l'UI Jaeger (bouton "Download JSON") fonctionne aussi avec `analyze --input`. Quelle que soit la voie batch, la réserve d'obfuscation ci-dessus reste valable : gardez `sanitizer_aware_classification = "strict"`.

---

## Démarrage rapide : sidecar

Déboguez un service unique en dev/staging. perf-sentinel tourne à côté du service, en partageant son namespace réseau.

```bash
# Récupérer le fichier compose d'exemple (sans cloner le dépôt), puis le démarrer
curl -o docker-compose.yml https://raw.githubusercontent.com/robintra/perf-sentinel/main/examples/docker-compose-sidecar.yml
docker compose up -d
```

Config de l'app (pas de saut réseau, même namespace) :

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

```bash
docker compose logs -f perf-sentinel
```

Voir [`examples/docker-compose-sidecar.yml`](../../examples/docker-compose-sidecar.yml).

---

## Démarrage rapide : daemon direct

Développement local sur votre machine hôte.

```bash
perf-sentinel watch
```

L'adresse d'écoute par défaut est `127.0.0.1` sur 4317 (gRPC) et 4318 (HTTP). Pour que les conteneurs Docker puissent atteindre l'hôte, posez `[daemon] listen_address = "0.0.0.0"` dans `.perf-sentinel.toml`. Une adresse d'écoute autre que loopback émet un avertissement au démarrage : les endpoints n'ont pas d'auth applicative, mettez donc un reverse-proxy ou une network policy en frontal, ou posez `[daemon.ack] api_key` pour garder les écritures d'ack (voir `docs/FR/CONFIGURATION-FR.md`).

Config de l'app :

```bash
# Pour les services sur l'hôte
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317

# Pour les services conteneurisés sur Docker Desktop
OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4317
```

Les findings sont émis sur stdout en NDJSON. Les métriques Prometheus sont disponibles à `http://localhost:4318/metrics`.

---

## Pour aller plus loin

- [`INSTRUMENTATION-FR.md`](./INSTRUMENTATION-FR.md) pour la configuration par langage (Java, Quarkus, .NET, Rust) et le chemin OTel Collector en production.
- [`CI-FR.md`](./CI-FR.md) pour le câblage CI (GitHub Actions, GitLab CI, Jenkins), la philosophie du quality gate et la sous-commande `diff` pour les régressions de PR.

## Formats d'ingestion

perf-sentinel auto-détecte le format d'entrée avec `perf-sentinel analyze --input` :

| Format                         | Détection                                             | Exemple                           |
|--------------------------------|-------------------------------------------------------|-----------------------------------|
| **Natif** (perf-sentinel JSON) | Tableau d'objets avec champ `"type"`                  | Format par défaut                 |
| **OTLP JSON**                  | Objet avec clé `"resourceSpans"`                      | Dump exporter `file` du Collector |
| **Jaeger JSON**                | Objet avec clé `"data"` contenant `"spans"`           | Exporté depuis l'UI Jaeger        |
| **Zipkin JSON v2**             | Tableau d'objets avec `"traceId"` + `"localEndpoint"` | Exporté depuis l'UI Zipkin        |

Aucun flag `--format` n'est nécessaire pour l'entrée : le format est détecté automatiquement depuis les premiers octets du fichier.

**L'OTLP JSON en batch (0.9.5+).** `analyze --input` accepte la forme OTLP/JSON du protocole (`ExportTraceServiceRequest`, clés camelCase, ids trace/span en hexadécimal), en objet unique comme en NDJSON de l'exporter `file` du Collector (une requête par ligne). L'OTLP atteint aussi perf-sentinel en live par les listeners du daemon (`watch`) et indirectement par les sous-commandes `tempo` et `jaeger-query`. Si vous êtes sur dd-trace, voir [Vous venez de Datadog](#vous-venez-de-datadog-dd-trace-sans-opentelemetry).

**Entrées de statistiques base de données.** Les sous-commandes `pg-stat` et `mysql-stat` lisent des exports CSV ou JSON de `pg_stat_statements` et de `performance_schema.events_statements_summary_by_digest` respectivement, avec la même auto-détection au premier octet (`[` ou `{` signifie JSON, tout le reste CSV). Les noms de colonnes sont reconnus sans tenir compte de la casse. Les colonnes timer MySQL (picosecondes) sont converties en millisecondes au parsing. Exportez la vue MySQL avec `mysqlsh --result-format=csv` ou tout export CSV/JSON client. `SELECT ... INTO OUTFILE` produit du TSV échappé par antislash, non pris en charge.

```bash
# Export Jaeger
perf-sentinel analyze --input jaeger-export.json --ci

# Export Zipkin
perf-sentinel analyze --input zipkin-traces.json --ci
```

## Mode explain

Pour déboguer une trace spécifique, utilisez la sous-commande `explain` :

```bash
perf-sentinel explain --input traces.json --trace-id abc123-def456
```

Cela produit une vue arborescente de la trace avec les findings annotés en ligne. Utilisez `--format json` pour une sortie structurée.

## Export SARIF

**Qu'est-ce que SARIF.** Le Static Analysis Results Interchange Format est un schéma JSON standard OASIS (v2.1.0 depuis 2020) que les outils d'analyse statique utilisent pour publier leurs findings dans un format agnostique. GitHub Advanced Security et GitLab Ultimate acceptent l'envoi de fichiers SARIF et affichent chaque finding directement sur les pull requests, de la même manière que les résultats ESLint ou Semgrep aujourd'hui. perf-sentinel émet du SARIF pour que les findings d'anti-patterns apparaissent à côté des findings sécurité dans le même tableau de bord code scanning. [Spec](https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html).

Pour l'intégration avec GitHub ou GitLab code scanning, exportez les findings en SARIF v2.1.0 :

```bash
perf-sentinel analyze --input traces.json --format sarif > results.sarif
```

Envoyez le fichier SARIF vers votre tableau de bord code scanning. Chaque finding est mappé vers un résultat SARIF avec `ruleId`, `level`, `logicalLocations` (service + endpoint), un tag personnalisé `properties.confidence` et une valeur standard `rank` SARIF (0-100) dérivée de la confiance.

## Champ de confiance sur les findings

Chaque finding émis en JSON ou SARIF porte un champ `confidence` qui indique le contexte source de la détection. Le champ est conçu pour les consommateurs en aval comme perf-lint, une intégration IDE compagnon planifiée qui ajustera la sévérité affichée dans l'IDE selon le niveau de confiance à accorder au finding. Tout outil personnalisé qui consomme les sorties JSON ou SARIF de perf-sentinel peut utiliser ce champ de la même manière.

Valeurs :

| Valeur                | Quand émise                                                            | SARIF `rank` | Interprétation                                                                             |
|-----------------------|------------------------------------------------------------------------|--------------|--------------------------------------------------------------------------------------------|
| `"ci_batch"`          | `perf-sentinel analyze` (mode batch, toujours)                         | `30`         | Confiance faible : la trace vient d'un run CI contrôlé avec des patterns de trafic limités |
| `"daemon_staging"`    | `perf-sentinel watch` avec `[daemon] environment = "staging"` (défaut) | `60`         | Confiance moyenne : patterns de trafic réels observés sur un déploiement staging           |
| `"daemon_production"` | `perf-sentinel watch` avec `[daemon] environment = "production"`       | `90`         | Confiance la plus élevée : trafic réel, échelle réelle, vrais utilisateurs                 |

**Exemple de finding JSON :**

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

**Exemple de fragment de résultat SARIF :**

```json
{
  "ruleId": "n_plus_one_sql",
  "level": "warning",
  "message": { "text": "n_plus_one_sql in order-svc on POST /api/orders/{id}/submit..." },
  "properties": { "confidence": "daemon_production" },
  "rank": 90
}
```

**Configuration dans le daemon :**

```toml
[daemon]
# "staging" (défaut) → confidence = daemon_staging, rank = 60
# "production"       → confidence = daemon_production, rank = 90
environment = "production"
```

La valeur est tamponnée sur chaque finding émis par cette instance de daemon. Les valeurs invalides (tout sauf `staging`/`production`, insensible à la casse) sont rejetées au chargement de la config avec une erreur claire. Le mode batch `analyze` ignore ce champ et émet toujours `ci_batch`.

**Interopérabilité avec perf-lint (planifié).** perf-lint (planifié comme intégration IDE compagnon, pas encore publié) lira le champ `confidence` sur les findings runtime importés et appliquera un multiplicateur de sévérité : les findings `ci_batch` affichés en indications, `daemon_staging` en avertissements, `daemon_production` en erreurs. Ainsi un finding observé sur du trafic production réel remontera plus visiblement dans l'IDE qu'un finding observé uniquement dans une fixture CI.

---

## API de requêtage du daemon

Le daemon expose une API HTTP de requêtage sur le même port que OTLP HTTP et `/metrics` (défaut `4318`). Elle permet à des systèmes externes de récupérer les findings récents, les explications de traces, les corrélations cross-trace et la liveness du daemon sans parser les logs NDJSON. Utile pour l'alerting Prometheus, des panels Grafana personnalisés ou des runbooks SRE.

```bash
# Liveness du daemon
curl -sS http://127.0.0.1:4318/api/status

# Findings critiques récents
curl -sS "http://127.0.0.1:4318/api/findings?severity=critical&limit=10"
```

Voir [`docs/FR/QUERY-API-FR.md`](./QUERY-API-FR.md) pour la référence complète par endpoint, des exemples de réponses réelles capturées, des cas d'usage (alerting Prometheus, dashboard Grafana, runbook SRE) et le contrat de stabilité.

---

## Configuration avancée du scoring carbone

### Scoring multi-région

Si vos services couvrent plusieurs régions cloud, perf-sentinel peut appliquer des coefficients d'intensité carbone par région. Le mécanisme principal est l'attribut de ressource OTel `cloud.region`, que la plupart des SDKs OTel cloud émettent automatiquement. Quand cet attribut est absent (ex. ingestion Jaeger/Zipkin), utilisez la table `[green.service_regions]` pour associer les services à des régions :

```toml
[green]
default_region = "eu-west-3"

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc"  = "ap-southeast-1"
"auth-svc"  = "eu-west-3"
```

La chaîne de résolution de la région est : attribut `cloud.region` du span > `service_regions[service]` > `default_region` > bucket synthétique `"unknown"`. Le rapport JSON inclut un tableau `regions[]` trié par CO₂ décroissant, chaque ligne indiquant le nom de la région, l'intensité carbone du réseau, le PUE, le nombre d'opérations I/O et le CO₂ opérationnel.

### Intégration Scaphandre (on-premise / bare metal)

Pour les serveurs on-premise ou bare metal avec prise en charge d'Intel RAPL, perf-sentinel peut scraper les métriques de puissance par processus de [Scaphandre](https://github.com/hubblo-org/scaphandre) pour remplacer le modèle proxy I/O par des données d'énergie mesurées.

**Prérequis :**
- Scaphandre installé et en cours d'exécution sur chaque hôte, exposant un endpoint Prometheus `/metrics`.
- Accès RAPL disponible (bare metal ou VM avec RAPL passthrough).

**Configuration :**

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5
process_map = { "order-svc" = "java", "game-svc" = "game", "chat-svc" = "dotnet" }
```

Le `process_map` mappe les noms de service perf-sentinel au label `exe` dans la métrique `scaph_process_power_consumption_microwatts` de Scaphandre. Le daemon scrape cet endpoint toutes les `scrape_interval_secs` secondes et calcule un coefficient énergie-par-op par service avec la formule `energy_kwh = (power_watts * interval) / ops / 3_600_000`.

Les services absents du `process_map`, et tous les services quand l'endpoint est injoignable, se rabattent de manière transparente sur le modèle proxy. Le tag de modèle passe à `"scaphandre_rapl"` pour les services qui utilisent l'énergie mesurée. Seul le mode daemon `watch` utilise Scaphandre. La commande batch `analyze` utilise toujours le modèle proxy.

#### Endpoint Scaphandre authentifié

Si l'exporter Scaphandre est placé derrière un reverse proxy avec auth basic ou un ingress bearer-token, ajoutez une entrée `auth_header` :

```toml
[green.scaphandre]
endpoint = "https://scaphandre.my-cluster.example/metrics"
scrape_interval_secs = 5
auth_header = "Authorization: Basic <base64>"
```

La valeur suit le même format `"Name: Value"` que le flag `--auth-header` des sous-commandes `tempo` et `jaeger-query`. Elle est marquée `sensitive`, hyper la masque dans les logs debug et les tables HPACK HTTP/2, et l'impl manuelle de `Debug` de la struct empêche toute fuite via un `tracing::debug!(?config)`.

La variable d'environnement `PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER` est prioritaire sur le fichier de config. Préférez la variable d'environnement en production pour éviter de versionner des secrets. Quand la valeur est définie dans le fichier de config et que la variable d'environnement est absente, un avertissement au démarrage oriente vers la variable d'environnement.

Envoyer un en-tête d'authentification en clair via `http://` déclenche un `tracing::warn!` une fois au démarrage du scraper. Préférez `https://` en production. Un en-tête mal formé désactive le sous-système du scraper avec un `tracing::error!` plutôt que de réessayer en silence.

### Estimation d'énergie cloud (AWS / GCP / Azure)

Pour les VMs cloud qui n'exposent pas RAPL (la plupart des instances hors bare metal), perf-sentinel peut estimer l'énergie par service via les métriques d'utilisation CPU depuis un endpoint Prometheus et le modèle SPECpower.

**Prérequis :**
- Un endpoint compatible Prometheus avec des métriques d'utilisation CPU (via cloudwatch_exporter, stackdriver-exporter, azure-metrics-exporter ou node_exporter).
- perf-sentinel n'interroge PAS les APIs des fournisseurs cloud directement.

**Configuration :**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"
default_instance_type = "m7i.xlarge"
cpu_metric = "node_cpu_seconds_total"

[green.cloud.services.api-us]
provider = "aws"
region = "us-east-1"
instance_type = "m7i.4xlarge"  # Sapphire Rapids

[green.cloud.services.analytics]
provider = "azure"
region = "westeurope"
instance_type = "Standard_D8s_v6"  # Emerald Rapids
```

Le daemon interpole la consommation avec `watts = idle_watts + (max_watts - idle_watts) * (cpu% / 100)` en utilisant les coefficients CCF 2026-04-24 par vCPU embarqués dans le binaire (~390 types d'instances couvrant AWS, GCP, Azure, y compris les architectures modernes Sapphire Rapids, Emerald Rapids, Genoa, Turin, Graviton 3/4 et Cobalt 100). Le tag de modèle est `"cloud_specpower"`. Comme Scaphandre, c'est une fonctionnalité réservée au daemon.

**Précédence des sources d'énergie.** Quand plusieurs sources mesurées sont configurées pour le même service, la plus précise gagne. La chaîne complète : `electricity_maps_api` > `alumet_rapl` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`. Voir `docs/FR/LIMITATIONS-FR.md` pour la discussion sur les limites de précision de chaque source mesurée.

#### Endpoint Prometheus authentifié

Si votre Prometheus est derrière une auth basic, un proxy bearer-token, ou un service hébergé comme Grafana Cloud ou Grafana Mimir, ajoutez une entrée `auth_header` :

```toml
[green.cloud]
prometheus_endpoint = "https://prometheus.grafana-cloud.example/api/prom"
auth_header = "Authorization: Bearer ${GRAFANA_CLOUD_TOKEN}"
```

La valeur suit le même format `"Name: Value"` que le flag `--auth-header` des sous-commandes `tempo` et `jaeger-query`. Elle est marquée `sensitive`, hyper la masque dans les logs debug et les tables HPACK HTTP/2, et l'impl manuelle de `Debug` de la struct empêche toute fuite via un `tracing::debug!(?config)`.

La variable d'environnement `PERF_SENTINEL_CLOUD_AUTH_HEADER` est prioritaire sur le fichier de config. Préférez la variable d'environnement en production pour éviter de versionner des secrets. Quand la valeur est définie dans le fichier de config et que la variable d'environnement est absente, un avertissement au démarrage oriente vers la variable d'environnement.

Envoyer un en-tête d'authentification en clair via `http://` déclenche un `tracing::warn!` une fois au démarrage du scraper. Préférez `https://` en production. Un en-tête mal formé désactive le sous-système du scraper avec un `tracing::error!` plutôt que de réessayer en silence.

### Calibration du modèle proxy avec des mesures terrain

Quand ni Scaphandre ni l'estimation cloud ne sont disponibles mais que vous avez des mesures d'énergie de référence issues d'une source externe (wattmètre, export RAPL, monitoring datacenter), la sous-commande `perf-sentinel calibrate` ajuste les coefficients I/O vers énergie du modèle proxy par service. Le workflow en trois étapes :

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow_dark.svg">
  <img alt="Workflow de calibration" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow.svg">
</picture>

**1. Mesurer.** Exécuter une charge de référence et collecter à la fois les traces (format JSON perf-sentinel standard) et les mesures d'énergie (CSV avec colonnes `timestamp,service,power_watts` ou `timestamp,service,energy_kwh`, auto-détecté depuis l'en-tête).

**2. Calibrer.** Exécuter `perf-sentinel calibrate --traces traces.json --measured-energy energy.csv --output calibration.toml`. La sous-commande corrèle les ops I/O avec les lectures d'énergie par service et fenêtre temporelle, calcule `factor = measured_per_op / default_proxy` et écrit un fichier TOML. Les facteurs > 10x ou < 0.1x émettent des avertissements (probable erreur de mesure).

**3. Utiliser.** Charger le fichier de calibration à la lecture de la config via `[green] calibration_file = ".perf-sentinel-calibration.toml"`. La boucle de scoring multiplie l'énergie proxy par le facteur du service et le tag de modèle reçoit un suffixe `+cal` (par exemple `io_proxy_v2+cal`). La calibration ne s'applique qu'au modèle proxy : l'énergie mesurée Scaphandre/cloud reste prioritaire.

---

## Intégration Tempo

Si votre infrastructure utilise Grafana Tempo comme backend de traces, vous pouvez l'interroger directement avec `perf-sentinel tempo` au lieu d'exporter les traces dans des fichiers.

> **Workflow post-mortem.** Quand une trace est plus ancienne que la fenêtre live de 30 secondes du daemon, Tempo devient la source de rejeu pour `perf-sentinel tempo --trace-id …`. Le workflow complet d'incident, de l'alerte Grafana à l'exemplar, puis au trace_id et au rejeu, est documenté dans [RUNBOOK-FR.md](RUNBOOK-FR.md).

### Analyse d'une trace

```bash
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123def456
```

### Recherche par service

```bash
# Analyser la dernière heure de traces pour order-svc
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 1h

# Mode CI avec quality gate
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 30m --ci
```

### Prérequis

- Tempo doit exposer son API HTTP (port 3200 par défaut).
- Le flag `--endpoint` pointe vers l'URL de base de l'API Tempo.
- Les traces sont récupérées en protobuf OTLP et passent par le pipeline d'analyse standard. La sortie est identique à `perf-sentinel analyze`.

### Tempo en mode microservices (`tempo-distributed`)

Si votre Tempo est déployé via le chart Helm `tempo-distributed` et non via l'image monolithique single-binary, l'API HTTP de requête est exposée par **`tempo-query-frontend`**, pas par `tempo-querier`. `tempo-querier` est un worker interne sans API publique, donc pointer `--endpoint` dessus renvoie HTTP 404 sur chaque `/api/search`. Résolvez le nom d'hôte du query-frontend comme votre environnement le permet (nom de Service Kubernetes, nom de service Docker Compose, ou hôte explicite en bare-metal) :

```bash
perf-sentinel tempo --endpoint http://tempo-query-frontend:3200 \
  --service order-svc --lookback 1h
```

Un 404 dû à un endpoint erroné remonte comme `Tempo returned HTTP 404 for https://.../api/search?...` (l'URL qui a échoué est incluse dans le message) pour rendre la mauvaise configuration diagnostiquable immédiatement.

### Alternative : forwarding générique Tempo

Au lieu d'interroger Tempo, vous pouvez configurer Tempo pour qu'il transmette une copie des traces vers perf-sentinel via [son mécanisme de generic forwarding](https://grafana.com/docs/tempo/latest/operations/manage-advanced-systems/generic_forwarding/). Cela fonctionne en temps réel avec `perf-sentinel watch`.

## Intégration API Jaeger query (Jaeger et Victoria Traces)

Si votre infrastructure utilise Jaeger upstream ou [Victoria Traces](https://docs.victoriametrics.com/victoriatraces/) comme backend de traces, les deux parlent l'API HTTP query de Jaeger et sont couverts par une seule sous-commande, `perf-sentinel jaeger-query`. Contrairement à l'API `/api/search` de Tempo (IDs uniquement), l'API `/api/traces` de Jaeger retourne les traces complètes en une seule requête HTTP, donc la CLI ne parallélise pas les récupérations trace par trace.

### Analyse d'une trace

```bash
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --trace-id abc123def456
```

### Recherche par service

```bash
# Analyser la dernière heure de traces pour order-svc
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc --lookback 1h

# Même recette contre Victoria Traces, qui sert l'API de requête Jaeger sous
# /select/jaeger et non à la racine, donc le préfixe va dans --endpoint
perf-sentinel jaeger-query --endpoint http://victoria-traces:10428/select/jaeger --service order-svc --lookback 1h

# Mode CI avec quality gate
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc --lookback 30m --ci
```

### Prérequis

- Le backend doit exposer l'API HTTP de requête Jaeger (`/api/traces?service=...&start=...&end=...&limit=...` et `/api/traces/<id>`). Jaeger upstream (toutes les versions récentes) et Victoria Traces sont compatibles nativement. `start` et `end` sont les bornes que perf-sentinel envoie, en microsecondes, pour une fenêtre relative comme pour une fenêtre absolue. `lookback` n'est jamais envoyé : Victoria Traces ne le lit que sur son endpoint de graphe de services, jamais sur cette recherche, donc une requête qui le porterait partirait sans borne depuis l'epoch Unix.
- Le flag `--endpoint` pointe vers l'URL de base de l'API de requête, la partie à laquelle la CLI ajoute `/api/traces`. Jaeger upstream la sert à la racine sur le port 16686. Victoria Traces la sert sous `/select/jaeger` sur le port 10428, et a donc besoin de ce préfixe dans le flag.
- Les traces sont récupérées en JSON, parsées par le même chemin `{"data": [...]}` que l'ingestion Jaeger en mode fichier, puis passent dans le pipeline d'analyse standard. La sortie est identique à `perf-sentinel analyze`.
- `--lookback` accepte le même format `1h / 30m / 7d / 2h30m` que la sous-commande `tempo`.
- `--from` et `--to` remplacent `--lookback` par une fenêtre absolue en ISO 8601 UTC (`2026-08-20T15:00:00Z`). Les deux vont ensemble et ne se combinent pas avec `--lookback`. Ils servent à relire la fenêtre exacte d'un incident : une lookback est résolue au moment où la requête part, elle se décale donc à chaque exécution.
- `--max-traces` correspond au paramètre `limit` de la requête backend, qui plafonne le nombre de traces retournées par recherche.

### Réserves

- La recherche côté backend est bornée par la rétention configurée (Jaeger a 48h par défaut, Victoria Traces est configurable). Un `--lookback` ou une fenêtre `--from`/`--to` plus large que la rétention est silencieusement tronqué à la fenêtre conservée. perf-sentinel ne connaît pas la rétention d'un backend, il ne peut donc pas vous prévenir avant la requête.
- Une recherche `limit=N` retourne jusqu'à N traces complètes dans un seul corps HTTP. perf-sentinel plafonne la réponse à 256 MiB, ce qui couvre les charges de production typiques mais peut nécessiter un ajustement si vous recherchez régulièrement des centaines de grosses traces d'un coup. Baissez `--max-traces` si vous atteignez la limite de taille du corps. `--max-traces` est lui-même borné à 10 000 côté CLI.
- **En-tête d'authentification via `--auth-header`.** Passez une ligne d'en-tête au format curl (`"Name: Value"`) pour l'attacher à chaque requête backend. Couvre Bearer tokens, Basic Auth et en-têtes de clé d'API personnalisés. La valeur parsée est marquée `sensitive` et n'apparaît donc jamais dans les logs. Voir `docs/FR/LIMITATIONS-FR.md` pour les notes complètes d'usage (un seul en-tête max par invocation, valeur visible dans `ps`). Depuis 0.5.27, choisir la forme flag émet un événement de niveau `WARN` au démarrage qui oriente vers `--auth-header-env <NAME>` (même pattern que `pg-stat`). La forme variable d'environnement garde la valeur en dehors de la liste des arguments de processus et de l'historique shell.
- **`--endpoint` est une entrée de confiance.** Le validateur rejette les schémas non-http et les URLs avec credentials, mais accepte loopback, RFC 1918 et link-local. Dans un contexte CI où la valeur de l'endpoint pourrait venir d'une PR externe, assainissez-la en amont avant d'invoquer la sous-commande.

---

## Dépannage

### Aucun event reçu (`events_processed_total = 0`)

1. **Vérifiez la connectivité.** Depuis le conteneur : `curl http://host.docker.internal:4318/metrics`. S'il échoue, perf-sentinel n'est pas joignable.
2. **Vérifiez l'adresse d'écoute.** perf-sentinel écoute sur `127.0.0.1` par défaut. Pour l'accès Docker, configurez `listen_address = "0.0.0.0"` dans `.perf-sentinel.toml` ou lancez-le nativement sur l'hôte.
3. **Vérifiez le protocole.** Le Java Agent utilise gRPC par défaut (port 4317). Assurez-vous que `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` correspond au port que vous ciblez.

### Events reçus mais aucun finding

1. **Vérifiez les attributs de span.** perf-sentinel ne traite que les spans avec `db.statement`/`db.query.text` (SQL) ou `http.url`/`url.full` (HTTP). Les autres spans sont ignorés.
2. **Vérifiez le kind du span.** Depuis 0.11.2, un span `SERVER` portant une URL HTTP est un traitement entrant et non un appel sortant, il ne produit donc aucun finding HTTP même si l'attribut est présent. Les flottes sur les anciennes conventions sémantiques, qui posent aussi `http.url` sur le span de traitement, perdent ainsi les appels qu'elles voyaient auparavant. Un appel sortant se présente comme un span `CLIENT` du côté de l'appelant.
3. **Vérifiez les seuils de détection.** Le seuil N+1 par défaut est 5 occurrences du même template normalisé dans la même trace. Si votre trace compte moins de 5 appels répétés, aucun finding n'est généré.
4. **Vérifiez la normalisation des URLs.** perf-sentinel remplace les segments de chemin numériques par `{id}` et les UUIDs par `{uuid}`. Si vos URLs répétées ne diffèrent que par un identifiant texte (par exemple `/account/alice`, `/account/bob`), elles ne seront pas regroupées dans le même template.

### Erreur AOT cache avec le Java Agent

Le Java Agent (`-javaagent:`) est incompatible avec les caches AOT de la JEP 483. Si vous voyez `Unable to map shared spaces` ou `Mismatched values for property jdk.module.addmods`, contournez le cache AOT quand l'agent est actif (voir la section Java de [INSTRUMENTATION-FR.md](./INSTRUMENTATION-FR.md#java-opentelemetry-java-agent-v227-spring-boot-helidon-4x)).

### Le starter Spring Boot ne capture pas les appels HTTP sortants

Le `spring-boot-starter-opentelemetry` (Spring Boot 4) fait le pont entre les métriques Micrometer et OTel mais n'instrumente pas complètement les appels sortants `WebClient` ou `RestTemplate` avec la propagation du contexte de trace. Utilisez le Java Agent pour une instrumentation complète.
