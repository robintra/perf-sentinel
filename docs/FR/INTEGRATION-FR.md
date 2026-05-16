# Guide d'intégration

perf-sentinel accepte les traces OpenTelemetry via OTLP (gRPC sur 4317, HTTP sur 4318). Ce guide vous accompagne de zéro jusqu'à votre premier finding pour chaque topologie de déploiement.

## Sommaire

- [Choisissez votre topologie](#choisissez-votre-topologie) : tableau comparatif des quatre modes de déploiement supportés.
- [Démarrage rapide : CI batch](#démarrage-rapide--ci-batch) : exécuter perf-sentinel depuis un pipeline CI contre un fixture de traces.
- [Démarrage rapide : collector central](#démarrage-rapide--collector-central) : déploiement production via OpenTelemetry Collector.
- [Démarrage rapide : sidecar](#démarrage-rapide--sidecar) : debug d'un seul service en dev ou staging.
- [Démarrage rapide : daemon direct](#démarrage-rapide--daemon-direct) : développement local.
- [Pour aller plus loin](#pour-aller-plus-loin) : pointeurs vers INSTRUMENTATION-FR.md et CI-FR.md pour les sujets côté application et côté CI.
- [Formats d'ingestion](#formats-dingestion) : règles d'auto-détection JSON natif, OTLP, Jaeger, Zipkin, Tempo, pg_stat_statements.
- [Mode explain](#mode-explain) : vue arborescente d'une trace.
- [Export SARIF](#export-sarif) : sortie SARIF v2.1.0 pour le code scanning GitHub ou GitLab.
- [Champ de confiance sur les findings](#champ-de-confiance-sur-les-findings) : champ `confidence` JSON / SARIF pour les consommateurs aval.
- [API de requêtage du daemon](#api-de-requêtage-du-daemon) : API HTTP sur le port OTLP HTTP, voir aussi [QUERY-API-FR.md](./QUERY-API-FR.md) pour la référence complète.
- [Configuration avancée du scoring carbone](#configuration-avancée-du-scoring-carbone) : scoring multi-région, Scaphandre, énergie cloud-native, Electricity Maps, calibration.
- [Intégration Tempo](#intégration-tempo) : interroger un backend Grafana Tempo directement avec `perf-sentinel tempo`.
- [Intégration API Jaeger query](#intégration-api-jaeger-query-jaeger-et-victoria-traces) : Jaeger upstream et Victoria Traces via une seule sous-commande.
- [Troubleshooting](#troubleshooting) : problèmes d'ingestion et de détection courants.

## Choisissez votre topologie

| Topologie                                                     | Idéal pour                        | Effort         | Modifications des services      |
|---------------------------------------------------------------|-----------------------------------|----------------|---------------------------------|
| **[CI batch](#démarrage-rapide--ci-batch)**                   | Pipelines CI, vérifications de PR | Le plus faible | Aucune (fichiers de traces)     |
| **[Collector central](#démarrage-rapide--collector-central)** | Production, multi-services        | Faible         | Aucune (config YAML uniquement) |
| **[Sidecar](#démarrage-rapide--sidecar)**                     | Dev/staging, debug d'un service   | Faible         | Aucune (Docker uniquement)      |
| **[Daemon direct](#démarrage-rapide--daemon-direct)**         | Dev local, expérimentations       | Moyen          | Variables d'env par langage     |

---

## Démarrage rapide : CI batch

**Cas d'usage :** exécuter perf-sentinel dans votre pipeline CI pour détecter les requêtes N+1 avant la production. Pas de daemon, pas de Docker, un binaire qui lit un fichier de traces et retourne le code 1 si le quality gate échoue.

### Étape 1 : Installation

```bash
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64
sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel
```

### Étape 2 : Configurer les seuils

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
# : surcharges par service pour les déploiements multi-région
# [green.service_regions]
# "api-us"   = "us-east-1"
# "api-asia" = "ap-southeast-1"
```

> La sortie CO₂ est structurée : `green_summary.co2.total.{low,mid,high}` plus un tag de méthodologie SCI v1.0, avec un intervalle d'incertitude multiplicative 2× (`low = mid/2`, `high = mid×2`). Le scoring multi-région est automatique quand les spans OTel portent l'attribut `cloud.region`. Voir `docs/FR/CONFIGURATION-FR.md` et `docs/FR/LIMITATIONS-FR.md#précision-des-estimations-carbone` pour les détails.

### Étape 3 : Collecter les traces

Exportez les traces depuis vos tests d'intégration. Si vos tests tournent avec l'instrumentation OTel, sauvegardez la sortie dans un fichier JSON. Vous pouvez aussi exporter depuis l'UI Jaeger ou Zipkin, perf-sentinel détecte automatiquement le format.

### Étape 4 : Analyser

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

### Étape 5 : Investiguer les findings

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

---

### Dashboard HTML

`perf-sentinel report --input traces.json --output report.html` produit un dashboard HTML single-file. Double-clic pour l'ouvrir dans n'importe quel navigateur, fonctionne hors ligne, sans ressource externe. Public visé : les devs qui explorent un artefact CI et préfèrent cliquer plutôt que taper. Le dashboard affiche les findings, les arbres de traces et les métriques `GreenOps` avec navigation croisée entre sections (clic sur un finding pour voir son arbre de trace, la span responsable est surlignée en rouge).

Flags :
- `--input <FICHIER>` ou `--input -` : fichier de traces ou stdin (même auto-détection de format que `analyze` : JSON natif, Jaeger, Zipkin v2).
- `--output <FICHIER>` : requis, écrasé s'il existe déjà.
- `--config <CHEMIN>` : `.perf-sentinel.toml` optionnel, mêmes sémantiques que `analyze --config`.
- `--max-traces-embedded <N>` : cap sur les traces embarquées pour l'onglet Explain. Sans valeur, la sortie est ajustée automatiquement pour viser une taille HTML d'environ 5 Mo en coupant les traces à plus faible IIS. Un bandeau dans l'onglet Findings remonte le ratio tronqué quand la coupe s'applique, et la CLI loggue une ligne `info!` sur stderr (`Embedded N of M traces ... trimmed for file size`) pour que les logs de build CI portent le même signal.
- `--pg-stat <FICHIER>` : cross-référence un export `pg_stat_statements` CSV ou JSON. Active l'onglet pg_stat et la navigation croisée Explain vers pg_stat sur les spans SQL dont le template normalisé correspond à une ligne pg_stat.
- `--pg-stat-prometheus <URL>` : scrape one-shot d'un `postgres_exporter`, même effet que `--pg-stat` sans le fichier intermédiaire. Mutuellement exclusif avec `--pg-stat`.
- `--pg-stat-auth-header "Nom: Valeur"` : en-tête d'authentification optionnel attaché à la requête `--pg-stat-prometheus` (même format `"Nom: Valeur"` que le flag `--auth-header` des sous-commandes `tempo` et `jaeger-query`). La variable d'environnement `PERF_SENTINEL_PGSTAT_AUTH_HEADER` est prioritaire sur le flag. Préférez la variable d'environnement en production pour éviter d'exposer le secret dans la liste des arguments de processus ou l'historique shell. Quand la valeur est fournie via le flag et que la variable d'environnement est absente, un warning de démarrage oriente vers la variable d'environnement. Requis pour Grafana Cloud, Grafana Mimir ou tout ingress Prometheus appliquant une auth basic ou bearer. La valeur est marquée `sensitive` pour que hyper la masque dans les logs debug et les tables HPACK. L'envoyer en clair via `http://` déclenche un `tracing::warn!`, préférez `https://` en production.
- `--before <FICHIER>` : rapport baseline JSON (la sortie de `analyze --format json`). Active un onglet Diff qui affiche les nouveaux findings, les findings résolus, les changements de sévérité et les deltas d'I/O par endpoint par rapport à la baseline.

Les codes de sortie diffèrent de `analyze --ci` : `report` sort toujours 0, même quand la quality gate échoue. Le statut de la gate est rendu comme un badge dans la barre supérieure du HTML. Utilise `analyze --ci` quand tu as besoin du signal d'exit-code en CI.

Exemples d'invocation :

```bash
# Rapport post-mortem de base
perf-sentinel report --input traces.json --output report.html

# Avec cross-référence hotspots SQL
perf-sentinel report --input traces.json \
    --pg-stat pg_stat_statements.csv \
    --output report.html

# Avec scrape Prometheus à la place du fichier
perf-sentinel report --input traces.json \
    --pg-stat-prometheus http://prometheus:9090 \
    --output report.html

# Vue régression PR : diff contre une baseline
perf-sentinel report --input after.json \
    --before before.json \
    --output report.html

# Tout à la fois
perf-sentinel report --input traces.json \
    --pg-stat pg_stat_statements.csv \
    --before baseline.json \
    --output report.html
```

Clavier dans le dashboard : `j`/`k` déplacent la sélection Findings, `enter` ouvre le finding courant dans Explain, `esc` suit une échelle de priorité à quatre tiers (ferme la cheatsheet, ferme la barre de recherche, sort de l'onglet Explain, efface les puces de filtre actives). `/` ouvre un filtre substring sur l'onglet actif, limité aux onglets Findings, pg_stat, Diff ou Correlations. Tape `?` pour la cheatsheet complète qui liste tous les raccourcis, avec en plus les raccourcis style vim `g f` / `g e` / `g p` / `g d` / `g c` / `g r` qui sautent d'un onglet à l'autre.

Gros résultats : la liste Findings affiche les 500 premières lignes correspondantes et expose un bouton `Show N more findings (remaining M)` sous la liste pour révéler le bloc suivant. Chaque clic sur une puce de filtre, modification de la recherche, ou application d'un hash deep-link remet le compteur à 500, pour que l'utilisateur ne se retrouve jamais paginé sur des lignes qui ne matchent plus.

Partage et export : chaque onglet listable (Findings, pg_stat, Diff, Correlations) expose un bouton **Export CSV** qui télécharge la vue filtrée active au format CSV RFC 4180 (les templates contenant virgules ou guillemets sont échappés correctement, plus un guard OWASP formula-injection qui préfixe une apostrophe sur les cellules commençant par `=`, `+`, `-`, `@` ou une tabulation). Le fragment d'URL reflète l'onglet actif plus la recherche et les puces de filtre, donc partager un lien comme `report.html#pgstat&ranking=mean_time&search=payment` restaure exactement la même vue chez le destinataire. Le thème et le dernier classement pg_stat sélectionné persistent dans `sessionStorage`, limité à l'onglet de navigateur courant.

C'est une vue post-mortem d'un jeu de traces terminé. Pour une inspection live d'un daemon qui tourne, utilise `perf-sentinel query inspect` (TUI) ou directement les endpoints `/api/*`. Pour un workflow Tempo, compose via le shell : `perf-sentinel tempo --endpoint http://tempo:3200 --search "..." --output traces.json && perf-sentinel report --input traces.json --output report.html`.

#### Snapshot depuis un daemon live

Quand un daemon tourne, `GET /api/export/report` émet son état courant sous forme de JSON `Report`, avec la même forme que `analyze --format json`. Pipe le directement dans le dashboard pour un snapshot quasi-live (toujours post-mortem au sens sémantique, juste à courte durée de vie) :

```bash
curl -s http://daemon.internal:4318/api/export/report \
    | perf-sentinel report --input - --output report.html
```

`report --input` auto-détecte la forme JSON : un tableau au top-level est traité comme des événements de trace et passe par normalize/detect/score, un objet est traité comme un Report pré-calculé et embarqué tel quel. Seuls les Reports produits par le daemon portent des `correlations`, donc l'onglet Correlations du dashboard s'active automatiquement quand ce chemin est emprunté. Les daemons en cold-start retournent `503` avec `{"error": "daemon has not yet processed any events"}` jusqu'à ce que le premier batch OTLP arrive.

---

## Démarrage rapide : collector central

**Cas d'usage :** déploiement production où les services envoient déjà des traces à un OpenTelemetry Collector (ou vous souhaitez en ajouter un). Zéro modification de code, uniquement de la configuration YAML.

### Étape 1 : Démarrer perf-sentinel + collector

```bash
docker compose -f examples/docker-compose-collector.yml up -d
```

Cela démarre :
- Un **OTel Collector** qui écoute sur les ports 4317 (gRPC) et 4318 (HTTP)
- **perf-sentinel** en mode watch, recevant les traces du collector

### Étape 2 : Pointer vos services vers le collector

Définissez ces variables d'environnement dans vos conteneurs applicatifs :

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317
OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

Si vos services exportent déjà vers un collector, ajoutez perf-sentinel comme exporteur supplémentaire dans votre `otel-collector-config.yaml` existant :

```yaml
exporters:
  otlp/perf-sentinel:
    endpoint: perf-sentinel:4317
    tls:
      insecure: true

service:
  pipelines:
    traces:
      exporters: [otlp/perf-sentinel, otlp/votre-backend-existant]
```

### Étape 3 : Générer du trafic

Utilisez votre application normalement. Après l'expiration du TTL des traces (30 secondes par défaut), perf-sentinel émet les findings en NDJSON sur stdout :

```bash
docker compose -f examples/docker-compose-collector.yml logs -f perf-sentinel
```

### Étape 4 : Monitoring avec Prometheus + Grafana

perf-sentinel expose des métriques Prometheus à `http://localhost:14318/metrics` avec des exemplars OpenMetrics (clic direct depuis Grafana vers votre backend de traces) :

```bash
curl -s http://localhost:14318/metrics | grep perf_sentinel
```

Ajoutez-le comme cible de scrape Prometheus :

```yaml
# prometheus.yml
scrape_configs:
  - job_name: perf-sentinel
    static_configs:
      - targets: ['perf-sentinel:4318']
```

Métriques clés :
- `perf_sentinel_findings_total{type, severity}` : findings avec exemplar `trace_id` pour le clic direct
- `perf_sentinel_io_waste_ratio` : ratio de gaspillage I/O avec exemplar `trace_id`
- `perf_sentinel_events_processed_total` : total de spans ingérés
- `perf_sentinel_traces_analyzed_total` : total de traces complétées
- `perf_sentinel_slow_duration_seconds{type}` : histogram des durées de spans lents (utiliser `histogram_quantile()` pour des percentiles globaux sur des instances shardées)

Voir [`examples/otel-collector-config.yaml`](../../examples/otel-collector-config.yaml) pour la config complète avec les options de sampling et filtrage.

---

## Démarrage rapide : sidecar

**Cas d'usage :** debug d'un service unique en dev/staging. perf-sentinel tourne à côté du service, partageant son namespace réseau.

### Étape 1 : Démarrer le sidecar

```bash
docker compose -f examples/docker-compose-sidecar.yml up -d
```

### Étape 2 : Configurer votre app

Votre app envoie les traces à `localhost:4318` (HTTP), pas de saut réseau puisque perf-sentinel partage le même namespace réseau :

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
```

### Étape 3 : Voir les findings

```bash
docker compose -f examples/docker-compose-sidecar.yml logs -f perf-sentinel
```

Voir [`examples/docker-compose-sidecar.yml`](../../examples/docker-compose-sidecar.yml) pour la configuration complète.

---

## Démarrage rapide : daemon direct

**Cas d'usage :** développement local. Exécutez perf-sentinel sur votre machine et pointez vos services vers lui.

### Étape 1 : Démarrer le daemon

```bash
perf-sentinel watch
```

Par défaut, il écoute sur `127.0.0.1:4317` (gRPC) et `127.0.0.1:4318` (HTTP). Pour que les conteneurs Docker puissent atteindre l'hôte, utilisez :

```toml
# .perf-sentinel.toml
[daemon]
listen_address = "0.0.0.0"
```

### Étape 2 : Instrumenter votre service

Définissez l'endpoint OTLP dans votre service (voir les [guides par langage](./INSTRUMENTATION-FR.md#devstaging--instrumentation-par-langage) ci-dessous) :

```bash
# Pour les services sur l'hôte
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317

# Pour les services dans Docker
OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4317
```

### Étape 3 : Voir les findings

Les findings sont émis sur stdout en NDJSON. Les métriques Prometheus sont disponibles à `http://localhost:4318/metrics`.

---

## Pour aller plus loin

Les quatre démarrages rapides ci-dessus mènent à un setup fonctionnel. Deux guides compagnons couvrent la suite :

- **[INSTRUMENTATION-FR.md](./INSTRUMENTATION-FR.md)** : comment envoyer des données à perf-sentinel. Instrumentation par langage (Java, Quarkus, .NET, Rust), chemin OTel Collector production avec guidance sampling, intégrations cloud, manifests Kubernetes.
- **[CI-FR.md](./CI-FR.md)** : comment câbler perf-sentinel dans la CI. Invocation en mode batch, recettes copier-coller pour GitHub Actions / GitLab CI / Jenkins, philosophie du quality gate, chemin de déploiement du rapport HTML interactif par provider, et la sous-commande `diff` pour la détection de régressions sur PR.

Les sections de référence ci-dessous restent dans ce document parce qu'elles s'appliquent à toutes les topologies (formats d'entrée et de sortie, API HTTP du daemon, scoring carbone avancé, ingestion Tempo et Jaeger, troubleshooting).

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

Chaque finding est mappé vers un résultat SARIF avec `ruleId`, `level`, `logicalLocations` (service + endpoint), un tag custom `properties.confidence` et une valeur standard `rank` SARIF (0-100) dérivée de la confiance.

## Champ de confiance sur les findings

Chaque finding émis en JSON ou SARIF porte un champ `confidence` qui indique le contexte source de la détection. Le champ est conçu pour les consommateurs en aval comme perf-lint, une intégration IDE compagnon planifiée qui ajustera la sévérité affichée dans l'IDE selon le niveau de confiance à accorder au finding. Tout outil tiers qui consomme les sorties JSON ou SARIF de perf-sentinel peut utiliser ce champ de la même manière.

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

**Fragment de résultat SARIF :**

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

**Interopérabilité avec perf-lint (planifié).** perf-lint (planifié comme intégration IDE compagnon, pas encore publié) lira le champ `confidence` sur les findings runtime importés et appliquera un multiplicateur de sévérité : les findings `ci_batch` affichés en hints, `daemon_staging` en warnings, `daemon_production` en errors. Ainsi un finding observé sur du trafic production réel remontera plus visiblement dans l'IDE qu'un finding observé uniquement dans une fixture CI.

---

## API de requêtage du daemon

Le daemon expose une API HTTP de requêtage sur le même port que OTLP HTTP et `/metrics` (défaut `4318`). Elle permet à des systèmes externes de récupérer les findings récents, les explications de traces, les corrélations cross-trace et la liveness du daemon sans parser les logs NDJSON. Utile pour l'alerting Prometheus, des panels Grafana custom ou des runbooks SRE.

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

Si vos services couvrent plusieurs régions cloud, perf-sentinel peut appliquer des coefficients d'intensité carbone par région. Le mécanisme principal est l'attribut OTel `cloud.region`, que la plupart des SDKs OTel cloud émettent automatiquement. Quand cet attribut est absent (ex. ingestion Jaeger/Zipkin), utilisez la table `[green.service_regions]` :

```toml
[green]
default_region = "eu-west-3"

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc"  = "ap-southeast-1"
"auth-svc"  = "eu-west-3"
```

La chaîne de résolution est : attribut `cloud.region` du span > `service_regions[service]` > `default_region` > bucket synthétique `"unknown"`. Le rapport JSON inclut un tableau `regions[]` trié par CO₂ décroissant.

### Intégration Scaphandre (on-premise / bare metal)

Pour les serveurs on-premise ou bare metal avec support Intel RAPL, perf-sentinel peut scraper les métriques de puissance par processus de [Scaphandre](https://github.com/hubblo-org/scaphandre) pour remplacer le modèle proxy par des données d'énergie mesurées.

**Prérequis :**
- Scaphandre installé et en cours d'exécution, exposant un endpoint Prometheus `/metrics`.
- Accès RAPL disponible (bare metal ou VM avec RAPL passthrough).

**Configuration :**

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5
process_map = { "order-svc" = "java", "game-svc" = "game", "chat-svc" = "dotnet" }
```

Le `process_map` mappe les noms de service perf-sentinel au label `exe` dans la métrique `scaph_process_power_consumption_microwatts` de Scaphandre. Le daemon scrape cet endpoint toutes les `scrape_interval_secs` secondes et calcule un coefficient énergie-par-op par service. Les services absents du `process_map` retombent sur le modèle proxy. Le tag de modèle passe à `"scaphandre_rapl"`. Seul le mode daemon `watch` utilise Scaphandre.

#### Endpoint Scaphandre authentifié

Si l'exporter Scaphandre est placé derrière un reverse proxy avec auth basic ou un ingress bearer-token, ajoutez une entrée `auth_header` :

```toml
[green.scaphandre]
endpoint = "https://scaphandre.mon-cluster.example/metrics"
scrape_interval_secs = 5
auth_header = "Authorization: Basic <base64>"
```

La valeur suit le même format `"Nom: Valeur"` que le flag `--auth-header` des sous-commandes `tempo` et `jaeger-query`. Elle est marquée `sensitive`, hyper la masque dans les logs debug et les tables HPACK HTTP/2, et l'impl manuelle de `Debug` de la struct empêche toute fuite via un `tracing::debug!(?config)`.

La variable d'environnement `PERF_SENTINEL_SCAPHANDRE_AUTH_HEADER` est prioritaire sur le fichier de config. Préférez la variable d'environnement en production pour éviter de committer des secrets dans le contrôle de version. Quand la valeur est définie dans le fichier de config et que la variable d'environnement est absente, un warning de démarrage oriente vers la variable d'environnement.

Envoyer un auth header en clair via `http://` déclenche un `tracing::warn!` au démarrage du scraper, préférez `https://` en production. Un header mal formé désactive le sous-système avec un `tracing::error!` plutôt que de réessayer en silence.

### Estimation d'énergie cloud (AWS / GCP / Azure)

Pour les VMs cloud sans accès RAPL, perf-sentinel peut estimer l'énergie par service via les métriques d'utilisation CPU depuis un endpoint Prometheus et le modèle SPECpower.

**Prérequis :**
- Un endpoint Prometheus avec des métriques d'utilisation CPU (via cloudwatch_exporter, stackdriver-exporter, azure-metrics-exporter ou node_exporter).
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

Le daemon interpole la consommation avec `watts = idle_watts + (max_watts - idle_watts) * (cpu% / 100)` en utilisant les coefficients CCF 2026-04-24 per-vCPU embarqués (~390 types d'instances couvrant AWS, GCP, Azure, y compris les architectures modernes Sapphire Rapids, Emerald Rapids, Genoa, Turin, Graviton 3/4 et Cobalt 100). Le tag de modèle est `"cloud_specpower"`. Fonctionnalité daemon uniquement.

**Précédence des sources d'énergie.** Quand Scaphandre et cloud energy sont configurés pour le même service, Scaphandre gagne (mesure RAPL directe). La chaîne complète : `electricity_maps_api` > `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.

#### Endpoint Prometheus authentifié

Si votre Prometheus est derrière une auth basic, un proxy bearer-token, ou un service managé comme Grafana Cloud ou Grafana Mimir, ajoutez une entrée `auth_header` :

```toml
[green.cloud]
prometheus_endpoint = "https://prometheus.grafana-cloud.example/api/prom"
auth_header = "Authorization: Bearer ${GRAFANA_CLOUD_TOKEN}"
```

La valeur suit le même format `"Nom: Valeur"` que le flag `--auth-header` des sous-commandes `tempo` et `jaeger-query`. Elle est marquée `sensitive`, hyper la masque dans les logs debug et les tables HPACK HTTP/2, et l'impl manuelle de `Debug` de la struct empêche toute fuite via un `tracing::debug!(?config)`.

La variable d'environnement `PERF_SENTINEL_CLOUD_AUTH_HEADER` est prioritaire sur le fichier de config. Préférez la variable d'environnement en production pour éviter de committer des secrets dans le contrôle de version. Quand la valeur est définie dans le fichier de config et que la variable d'environnement est absente, un warning de démarrage oriente vers la variable d'environnement.

Envoyer un auth header en clair via `http://` déclenche un `tracing::warn!` au démarrage du scraper, préférez `https://` en production. Un header mal formé désactive le sous-système avec un `tracing::error!` plutôt que de réessayer en silence.

### Calibration du modèle proxy avec des mesures terrain

Quand ni Scaphandre ni l'estimation cloud ne sont disponibles mais que vous avez des mesures d'énergie de référence (wattmètre, export RAPL, monitoring datacenter), la sous-commande `perf-sentinel calibrate` ajuste les coefficients I/O vers énergie du modèle proxy par service. Le workflow en trois étapes :

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow_dark.svg">
  <img alt="Workflow de calibration" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/calibration-workflow.svg">
</picture>

**1. Mesurer.** Exécuter une charge de référence et collecter à la fois les traces (format JSON perf-sentinel) et les mesures d'énergie (CSV avec colonnes `timestamp,service,power_watts` ou `timestamp,service,energy_kwh`, auto-détecté depuis l'en-tête).

**2. Calibrer.** Exécuter `perf-sentinel calibrate --traces traces.json --measured-energy energy.csv --output calibration.toml`. La sous-commande corrèle les ops I/O avec les lectures d'énergie par service et fenêtre temporelle, calcule `factor = mesuré_par_op / proxy_par_défaut` et écrit un fichier TOML. Les facteurs > 10x ou < 0.1x émettent des avertissements (probable erreur de mesure).

**3. Utiliser.** Charger le fichier de calibration au démarrage via `[green] calibration_file = ".perf-sentinel-calibration.toml"`. La boucle de scoring multiplie l'énergie proxy par le facteur du service et le tag de modèle reçoit un suffixe `+cal` (par exemple `io_proxy_v2+cal`). La calibration ne s'applique qu'au modèle proxy : l'énergie mesurée Scaphandre/cloud reste prioritaire.

---

## Intégration Tempo

Si votre infrastructure utilise Grafana Tempo comme backend de traces, vous pouvez l'interroger directement avec `perf-sentinel tempo`.

> **Workflow post-mortem.** Quand une trace est plus ancienne que la fenêtre live de 30 secondes du daemon, Tempo devient la source de rejeu pour `perf-sentinel tempo --trace-id …`. Le workflow complet d'incident (alerte Grafana → exemplar → trace_id → rejeu) est documenté dans [RUNBOOK-FR.md](RUNBOOK-FR.md).

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
- Les traces sont récupérées en protobuf OTLP et passent par le pipeline d'analyse standard.

### Tempo en mode microservices (`tempo-distributed`)

Si votre Tempo est déployé via le chart Helm `tempo-distributed` et non via l'image monolithique single-binary, l'API HTTP de requête est exposée par **`tempo-query-frontend`**, pas par `tempo-querier`. `tempo-querier` est un worker interne sans API publique ; pointer `--endpoint` dessus renvoie HTTP 404 sur chaque `/api/search`. Résolvez le hostname du query-frontend comme votre environnement le permet (nom de Service Kubernetes, nom de service Docker Compose, ou hôte explicite en bare-metal) :

```bash
perf-sentinel tempo --endpoint http://tempo-query-frontend:3200 \
  --service order-svc --lookback 1h
```

Un 404 dû à un endpoint erroné remonte désormais comme `Tempo returned HTTP 404 for https://.../api/search?...` (l'URL qui a échoué est incluse dans le message) pour rendre la mauvaise configuration diagnosticable immédiatement.

### Alternative : forwarding générique Tempo

Au lieu d'interroger Tempo, vous pouvez configurer Tempo pour qu'il forwarde une copie des traces vers perf-sentinel via [son mécanisme de generic forwarding](https://grafana.com/docs/tempo/latest/operations/manage-advanced-systems/generic_forwarding/). Cela évite d'interroger Tempo et fonctionne en temps réel avec `perf-sentinel watch`.

## Intégration API Jaeger query (Jaeger et Victoria Traces)

Si votre infrastructure utilise Jaeger upstream ou [Victoria Traces](https://docs.victoriametrics.com/victoriatraces/) comme backend de traces, les deux parlent l'API HTTP query de Jaeger et sont couverts par un seul subcommand, `perf-sentinel jaeger-query`. Contrairement à l'API `/api/search` de Tempo (ID-only), l'API `/api/traces` de Jaeger retourne les traces complètes en une seule requête HTTP, donc la CLI ne parallélise pas les fetches trace par trace.

### Analyse d'une trace

```bash
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --trace-id abc123def456
```

### Recherche par service

```bash
# Analyser la dernière heure de traces pour order-svc
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc --lookback 1h

# Même recette contre Victoria Traces (API-compatible)
perf-sentinel jaeger-query --endpoint http://victoria-traces:10428 --service order-svc --lookback 1h

# Mode CI avec quality gate
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc --lookback 30m --ci
```

### Prérequis

- Le backend doit exposer l'API HTTP de requête Jaeger (`/api/traces?service=...&lookback=...&limit=...` et `/api/traces/<id>`). Jaeger upstream (toutes les versions récentes) et Victoria Traces sont compatibles nativement.
- Le flag `--endpoint` pointe vers l'URL de base de l'API de requête (typiquement port 16686 pour Jaeger, port 10428 pour Victoria Traces).
- Les traces sont récupérées en JSON, parsées par le même chemin `{"data": [...]}` que l'ingestion Jaeger file-mode, puis passent dans le pipeline d'analyse standard. La sortie est identique à `perf-sentinel analyze`.
- `--lookback` accepte le même format `1h / 30m / 2h30m` que le subcommand `tempo`.
- `--max-traces` mappe vers le paramètre `limit` de la query backend, qui plafonne le nombre de traces retournées par recherche.

### Caveats

- La lookback côté backend est bornée par la rétention configurée (Jaeger a 48h par défaut, Victoria Traces est configurable). Un `--lookback` plus large que la rétention est silencieusement tronqué à la fenêtre conservée.
- Une recherche `limit=N` retourne jusqu'à N traces complètes dans un seul body HTTP. perf-sentinel plafonne la réponse à 256 MiB, ce qui couvre les workloads production typiques mais peut nécessiter un ajustement si vous recherchez régulièrement des centaines de grosses traces d'un coup. Baissez `--max-traces` si vous heurtez la limite de body. `--max-traces` est lui-même borné à 10 000 côté CLI.
- **Auth header via `--auth-header`.** Passez une ligne d'header au format curl (`"Name: Value"`) pour l'attacher à chaque requête backend. Couvre Bearer tokens, Basic Auth et headers API-key custom. La valeur parsée est marquée `sensitive`, elle n'apparaît jamais dans les logs. Voir `docs/FR/LIMITATIONS-FR.md` pour les notes complètes d'usage (un seul header max par invocation, valeur visible dans `ps`). Depuis 0.5.27, choisir la forme flag émet un événement de niveau `WARN` au démarrage qui oriente vers `--auth-header-env <NOM>` (même pattern que `pg-stat`), la forme variable d'environnement garde la valeur en dehors de la liste des arguments de processus et de l'historique shell.
- **`--endpoint` est une entrée de confiance.** Le validateur rejette les schémas non-http et les URLs avec credentials, mais accepte loopback, RFC 1918 et link-local. Dans un contexte CI où la valeur de l'endpoint pourrait venir d'une PR externe, assainissez l'input en amont avant d'invoquer le subcommand.

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

