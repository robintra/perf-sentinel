<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frobintra%2Fperf-sentinel%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&suffix=%20stable&label=rust%202024&color=D34516&logo=rust" alt="Rust" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml/badge.svg" alt="Security Audit" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-dark-horizontal.svg">
  <img alt="perf-sentinel" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-horizontal.svg">
</picture>

Analyse les traces d'exécution (requêtes SQL, appels HTTP) pour détecter les requêtes N+1, les appels redondants, et évalue l'intensité I/O par endpoint (GreenOps).

## Pourquoi perf-sentinel ?

Les anti-patterns de performance comme les requêtes N+1 existent dans toute application qui fait des I/O, monolithes comme microservices. Dans les architectures distribuées, un appel utilisateur cascade sur plusieurs services, chacun avec ses propres I/O, et personne n'a de visibilité sur le chemin complet. Les outils existants sont soit spécifiques à un runtime (Hypersistence Utils -> JPA uniquement), soit lourds et propriétaires (Datadog, New Relic), soit limités aux tests unitaires sans vision cross-service.

perf-sentinel adopte une approche différente : **l'analyse au niveau protocole**. Il observe les traces produites par l'application (requêtes SQL, appels HTTP) quel que soit le langage ou l'ORM utilisé. Il n'a pas besoin de comprendre JPA, EF Core ou SeaORM : il voit les requêtes qu'ils génèrent.

## GreenOps : scoring éco-conception intégré

Chaque finding inclut un **I/O Intensity Score (IIS)** : le nombre d'opérations I/O générées par requête utilisateur pour un endpoint donné. Réduire les I/O inutiles (N+1, appels redondants) améliore les temps de réponse *et* réduit la consommation énergétique : ce ne sont pas des objectifs concurrents.

- **I/O Intensity Score** = opérations I/O totales pour un endpoint / nombre d'invocations
- **I/O Waste Ratio** = opérations I/O évitables (issues des findings) / opérations I/O totales

Aligné avec le modèle **Software Carbon Intensity** ([SCI v1.0 / ISO/IEC 21031:2024](https://github.com/Green-Software-Foundation/sci)) de la Green Software Foundation. Le champ `co2.total` contient le **numérateur SCI** `(E × I) + M` sommé sur les traces analysées, pas le score d'intensité par requête. Le scoring multi-région est automatique quand les spans OTel portent l'attribut `cloud.region`. **Plus de 30 régions cloud** disposent de profils d'intensité carbone horaire intégrés, avec une variation saisonnière (mois x heure) pour FR, DE, GB et US-East. En mode daemon, le coefficient énergétique peut être affiné via [Scaphandre](https://github.com/hubblo-org/scaphandre) (RAPL bare-metal) ou l'estimation cloud-native CPU% + SPECpower pour les VMs AWS/GCP/Azure (section `[green.cloud]`). Les utilisateurs peuvent aussi fournir leurs propres profils horaires via `[green] hourly_profiles_file`.

> **Note :** les estimations CO₂ sont **directionnelles**, pas auditables. Chaque estimation porte un intervalle d'incertitude multiplicative `~2×` (`low = mid/2`, `high = mid×2`) car le modèle proxy I/O est approximatif. perf-sentinel est un **compteur de gaspillage**, pas un outil de comptabilité carbone. Ne l'utilisez pas pour le reporting CSRD ou GHG Protocol Scope 3. Voir [docs/FR/LIMITATIONS-FR.md](docs/FR/LIMITATIONS-FR.md#précision-des-estimations-carbone) pour la méthodologie complète.

## Positionnement

| Critère              | [Hypersistence Optimizer](https://vladmihalcea.com/hypersistence-optimizer/) | [Datadog APM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Digma](https://digma.ai/) | **perf-sentinel** |
|----------------------|------------------------------------------------------------------------------|-------------------------------------------------------|-----------------------------------------------------------------------|----------------------------|-------------------|
| Détection N+1 SQL    | ✅ JPA uniquement                                                             | ⚠️ Manuel (vue trace)                                 | ⚠️ Manuel (vue trace)                                                 | ✅ (JVM)                    | ✅ Polyglotte      |
| Détection N+1 HTTP   | ❌                                                                            | ⚠️ Manuel (vue trace)                                 | ⚠️ Manuel (vue trace)                                                 | ⚠️ Partiel                 | ✅                 |
| Polyglotte           | ❌ Java/JPA                                                                   | ✅ (agents par langage)                                | ✅ (agents par langage)                                                | ⚠️ JVM + .NET              | ✅ Protocol-level  |
| Cross-service        | ❌                                                                            | ✅                                                     | ✅                                                                     | ⚠️ Partiel                 | ✅ Trace ID        |
| Angle GreenOps / SCI | ❌                                                                            | ❌                                                     | ❌                                                                     | ❌                          | ✅ Natif           |
| Léger                | N/A (lib)                                                                    | ❌ (~150 Mo)                                           | ❌ (~150 Mo)                                                           | ❌ (~100 Mo)                | ✅ (<10 Mo RSS)    |
| Open-source          | ❌ Commercial                                                                 | ❌                                                     | ⚠️ Free tier limité                                                   | ⚠️ Freemium                | ✅ AGPL v3         |
| CI/CD quality gate   | ⚠️ (assertions manuelles)                                                    | ❌                                                     | ⚠️ (alertes, pas de gate natif)                                       | ⚠️                         | ✅ Natif           |

## Que remonte-t-il ?

Pour chaque anti-pattern détecté, perf-sentinel remonte :

- **Type :** N+1 SQL, N+1 HTTP, requête redondante, SQL lent, HTTP lent, fanout excessif, service bavard (chatty service), saturation du pool de connexions, ou appels sérialisés
- **Template normalisé :** la requête ou l'URL avec les paramètres remplacés par des placeholders (`?`, `{id}`)
- **Occurrences :** combien de fois le pattern s'est déclenché dans la fenêtre de détection
- **Endpoint source :** quel endpoint applicatif l'a généré (ex : `GET /api/orders`)
- **Suggestion :** par exemple *"batch cette requête"*, *"utilise un batch endpoint"*, *"ajouter un index"*
- **Impact GreenOps :** estimation des I/O évitables, I/O Intensity Score, objet `co2` structuré (`low`/`mid`/`high`, termes opérationnel + embodié SCI v1.0), breakdown par région quand le scoring multi-région est actif

![demo](docs/img/demo.gif)

<details>
<summary>Images fixes</summary>

**Configuration** (`.perf-sentinel.toml`) :

![config](docs/img/demo-config.png)

**Rapport d'analyse :**

![report](docs/img/demo-report.png)

</details>

En mode CI (`perf-sentinel analyze --ci`), la sortie est un rapport JSON structure :

<details>
<summary>Exemple de rapport JSON</summary>

```json
{
  "analysis": {
    "duration_ms": 1,
    "events_processed": 6,
    "traces_analyzed": 1
  },
  "findings": [
    {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "game",
      "source_endpoint": "POST /api/game/42/start",
      "pattern": {
        "template": "SELECT * FROM player WHERE game_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0
      }
    }
  ],
  "green_summary": {
    "total_io_ops": 6,
    "avoidable_io_ops": 5,
    "io_waste_ratio": 0.833,
    "top_offenders": [
      {
        "endpoint": "POST /api/game/42/start",
        "service": "game",
        "io_intensity_score": 6.0,
        "co2_grams": 0.000054
      }
    ],
    "co2": {
      "total":     { "low": 0.000519, "mid": 0.001038, "high": 0.002076, "model": "io_proxy_v1", "methodology": "sci_v1_numerator" },
      "avoidable": { "low": 0.000016, "mid": 0.000032, "high": 0.000064, "model": "io_proxy_v1", "methodology": "sci_v1_operational_ratio" },
      "operational_gco2": 0.000038,
      "embodied_gco2":    0.001
    },
    "regions": [
      { "region": "eu-west-3", "grid_intensity_gco2_kwh": 56.0, "pue": 1.135, "io_ops": 6, "co2_gco2": 0.000038 }
    ]
  },
  "quality_gate": {
    "passed": false,
    "rules": [
      { "rule": "n_plus_one_sql_critical_max", "threshold": 0.0, "actual": 0.0, "passed": true },
      { "rule": "n_plus_one_http_warning_max", "threshold": 3.0, "actual": 0.0, "passed": true },
      { "rule": "io_waste_ratio_max", "threshold": 0.3, "actual": 0.833, "passed": false }
    ]
  }
}
```

</details>

## Démarrage rapide

### Installation depuis crates.io

```bash
cargo install sentinel-cli
```

### Télécharger un binaire précompilé

Des binaires pour Linux (amd64, arm64), macOS (arm64) et Windows (amd64) sont disponibles sur la page [GitHub Releases](https://github.com/robintra/perf-sentinel/releases). Les Mac Intel peuvent utiliser le binaire arm64 via Rosetta 2.

```bash
# Exemple : Linux amd64
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64
sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel
```

### Lancer avec Docker

```bash
docker run --rm -p 4317:4317 -p 4318:4318 ghcr.io/robintra/perf-sentinel:latest
```

### Démo rapide

```bash
perf-sentinel demo
```

### Analyse batch (CI)

```bash
perf-sentinel analyze --input traces.json --ci
```

### Expliquer une trace

```bash
perf-sentinel explain --input traces.json --trace-id abc123
```

### Export SARIF (GitHub/GitLab code scanning)

```bash
perf-sentinel analyze --input traces.json --format sarif
```

### Import depuis Jaeger ou Zipkin

```bash
# Export Jaeger JSON (auto-détecté)
perf-sentinel analyze --input jaeger-export.json

# Zipkin JSON v2 (auto-détecté)
perf-sentinel analyze --input zipkin-traces.json
```

### Analyse pg_stat_statements

```bash
# Analyser un export pg_stat_statements pour détecter les requêtes coûteuses
perf-sentinel pg-stat --input pg_stat.csv

# Référence croisée avec les findings de traces
perf-sentinel pg-stat --input pg_stat.csv --traces traces.json
```

### Inspection interactive (TUI)

```bash
perf-sentinel inspect --input traces.json
```

### Mode streaming (daemon)

```bash
perf-sentinel watch
```

## Architecture

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/diagrams/svg/pipeline_dark.svg">
  <img alt="Architecture du pipeline" src="docs/diagrams/svg/pipeline.svg">
</picture>

## Topologies de déploiement

perf-sentinel supporte trois modèles de déploiement. Choisissez celui qui correspond à votre environnement.

### 1. Analyse batch CI (point de départ recommandé)

Analysez des fichiers de traces pré-collectés dans votre pipeline CI/CD. Le processus retourne le code 1 si le quality gate échoue.

```bash
# Dans votre job CI :
perf-sentinel analyze --ci --input traces.json --config .perf-sentinel.toml
```

Créez un `.perf-sentinel.toml` à la racine de votre projet :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # zéro tolérance pour les N+1 SQL
io_waste_ratio_max = 0.30          # max 30% d'I/O évitables

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500

[green]
enabled = true
default_region = "eu-west-3"                  # optionnel : active la conversion en gCO2eq
embodied_carbon_per_request_gco2 = 0.001      # terme M SCI v1.0, défaut 0,001 g/req

# Surcharges optionnelles par service pour les déploiements multi-région
# (utilisées quand cloud.region OTel est absent des spans) :
# [green.service_regions]
# "order-svc" = "us-east-1"
# "chat-svc"  = "ap-southeast-1"
```

Formats de sortie : `--format text` (coloré, par défaut), `--format json` (structuré), `--format sarif` (GitHub/GitLab code scanning).

### 2. Collector central (recommandé pour la production)

Un [OpenTelemetry Collector](https://opentelemetry.io/docs/collector/) reçoit les traces de tous les services et les transmet à perf-sentinel. Zéro modification de code dans vos services.

```
app-1 --\
app-2 ---+--> OTel Collector --> perf-sentinel (watch)
app-3 --/
```

Des fichiers prêts à l'emploi sont fournis dans [`examples/`](examples/) :

```bash
# Démarrer le collector + perf-sentinel
docker compose -f examples/docker-compose-collector.yml up -d

# Pointez vos apps vers le collector :
#   OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317
```

perf-sentinel diffuse les findings en NDJSON sur stdout et expose des métriques Prometheus avec [Grafana Exemplars](docs/INTEGRATION.md) sur `/metrics` (port 4318).

Voir [`examples/otel-collector-config.yaml`](examples/otel-collector-config.yaml) pour la config complète du collector avec les options de sampling et filtrage.

### 3. Sidecar (diagnostic par service)

perf-sentinel tourne à côté d'un service unique, partageant son namespace réseau. Utile pour du debug isolé.

```bash
docker compose -f examples/docker-compose-sidecar.yml up -d
```

L'app envoie les traces à `localhost:4317` (pas de saut réseau). Voir [`examples/docker-compose-sidecar.yml`](examples/docker-compose-sidecar.yml).

---

Pour l'instrumentation OTLP par langage (Java, .NET, Rust), voir [docs/INTEGRATION.md](docs/INTEGRATION.md). Pour la référence complète de configuration, voir [docs/CONFIGURATION.md](docs/CONFIGURATION.md). Pour la documentation de conception détaillée, voir [docs/design/](docs/design/00-INDEX.md).

## Licence

Ce projet est sous licence [GNU Affero General Public License v3.0](LICENSE).

