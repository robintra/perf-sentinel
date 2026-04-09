# Référence de configuration

perf-sentinel se configure via un fichier `.perf-sentinel.toml`. Tous les champs sont optionnels et ont des valeurs par défaut raisonnables.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../diagrams/svg/cli-commands_dark.svg">
  <img alt="Vue d'ensemble des commandes CLI" src="../diagrams/svg/cli-commands.svg">
</picture>

## Sous-commandes

| Sous-commande | Description                                                               |
|---------------|---------------------------------------------------------------------------|
| `analyze`     | Analyse batch de fichiers de traces. Lit depuis un fichier ou stdin       |
| `explain`     | Vue arborescente d'une trace avec findings annotés en ligne               |
| `watch`       | Mode daemon : ingestion OTLP temps réel et détection en streaming         |
| `demo`        | Lance l'analyse sur un jeu de données de démo embarqué                    |
| `bench`       | Benchmark du débit sur un fichier de traces                               |
| `pg-stat`     | Analyse des exports `pg_stat_statements` (CSV/JSON) pour les hotspots SQL |
| `inspect`     | TUI interactif pour naviguer les traces, findings et arbres de spans      |

## Sections

### `[thresholds]`

Seuils du quality gate. Le quality gate échoue si une règle est violée.

| Champ                         | Type     | Défaut | Description                                                                   |
|-------------------------------|----------|--------|-------------------------------------------------------------------------------|
| `n_plus_one_sql_critical_max` | entier   | `0`    | Nombre maximum de findings N+1 SQL **critiques** avant l'échec du gate        |
| `n_plus_one_http_warning_max` | entier   | `3`    | Nombre maximum de findings N+1 HTTP **warning ou plus** avant l'échec du gate |
| `io_waste_ratio_max`          | flottant | `0.30` | Ratio maximum de gaspillage I/O (0.0 à 1.0) avant l'échec du gate             |

### `[detection]`

Paramètres des algorithmes de détection.

| Champ                        | Type   | Défaut | Description                                                                                                     |
|------------------------------|--------|--------|-----------------------------------------------------------------------------------------------------------------|
| `n_plus_one_min_occurrences` | entier | `5`    | Nombre minimum d'occurrences (avec des paramètres distincts) pour signaler un pattern N+1                       |
| `window_duration_ms`         | entier | `500`  | Fenêtre temporelle en millisecondes dans laquelle les opérations répétées sont considérées comme un pattern N+1 |
| `slow_query_threshold_ms`    | entier | `500`  | Seuil de durée en millisecondes au-dessus duquel une opération est considérée comme lente                       |
| `slow_query_min_occurrences` | entier | `3`    | Nombre minimum d'occurrences lentes du même template pour générer un finding                                    |
| `max_fanout`                 | entier | `20`   | Nombre maximum de spans enfants par parent avant de signaler un fanout excessif (plage : 1-100000)              |
| `chatty_service_min_calls`   | entier | `15`   | Nombre minimum d'appels HTTP sortants par trace pour signaler un service bavard. Severite : warning > seuil, critical > 3x seuil. |
| `pool_saturation_concurrent_threshold` | entier | `10` | Nombre maximal de spans SQL concurrents par service pour signaler un risque de saturation du pool de connexions. Utilise un algorithme de balayage sur les timestamps des spans. |
| `serialized_min_sequential`  | entier | `3`    | Nombre minimum d'appels sequentiels independants (meme parent, sans chevauchement, templates differents) pour signaler des appels potentiellement parallelisables. |

### `[green]`

Configuration du scoring GreenOps alignée sur [SCI v1.0](https://github.com/Green-Software-Foundation/sci) (termes opérationnel + embodié, intervalles de confiance, multi-région).

| Champ                              | Type     | Défaut    | Description                                                                                                                                                                                                                                                                                                                                                                                        |
|------------------------------------|----------|-----------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`                          | booléen  | `true`    | Active le scoring GreenOps (IIS, ratio de gaspillage, top offenders, CO₂)                                                                                                                                                                                                                                                                                                                          |
| `default_region`                   | chaîne   | *(aucun)* | Région cloud de fallback utilisée quand ni l'attribut `cloud.region` du span ni le mapping `service_regions` ne résout une région. Exemples : `"eu-west-3"`, `"us-east-1"`, `"FR"`                                                                                                                                                                                                                 |
| `embodied_carbon_per_request_gco2` | flottant | `0.001`   | Terme `M` SCI v1.0 : émissions de fabrication matérielle amorties par requête (par trace), en gCO₂eq. Indépendant de la région. Mettre à `0.0` pour désactiver le carbone embodié                                                                                                                                                                                                                  |
| `use_hourly_profiles`              | booléen  | `true`    | Quand `true`, l'étape de scoring utilise des intensités réseau spécifiques à l'heure pour les régions disposant d'un profil horaire UTC embarqué (FR, DE, GB, US-East). Les rapports qui touchent une région profilée sont tagués `model = "io_proxy_v2"` au lieu de `"io_proxy_v1"`. Mettre à `false` pour figer les rapports sur le modèle annuel plat (utile pour les comparaisons historiques) |

#### `[green.service_regions]`

Surcharges de région par service utilisées quand `cloud.region` OTel est absent des spans (ex. ingestion Jaeger / Zipkin). Mappe nom de service → clé de région.

```toml
[green]
default_region = "eu-west-3"
embodied_carbon_per_request_gco2 = 0.001

[green.service_regions]
"order-svc" = "us-east-1"
"chat-svc"  = "ap-southeast-1"
```

#### Chaîne de résolution de région

Pour chaque span, l'étape de scoring carbone résout la région effective dans cet ordre (premier match gagne) :

1. **`event.cloud_region`** : depuis l'attribut de ressource OTel `cloud.region` (ou attribut de span en fallback). Le plus autoritatif.
2. **`[green.service_regions][event.service]`** : surcharge config par service.
3. **`[green] default_region`** : fallback global.

Les ops I/O sans région résolvable atterrissent dans un bucket synthétique `"unknown"` (zéro CO₂ opérationnel ; la ligne apparaît dans `regions[]` pour la visibilité). Le carbone embodié est tout de même émis car les émissions de fabrication matérielle sont indépendantes de la région. La cardinalité des régions est plafonnée à 256 buckets distincts ; le surplus tombe dans le bucket `unknown` pour éviter l'épuisement mémoire en cas d'ingestion mal configurée.

#### Forme de sortie

Quand le scoring vert est activé et qu'au moins un événement est analysé, le `green_summary` du rapport JSON inclut :

- **`co2`** : objet structuré `{ total, avoidable, operational_gco2, embodied_gco2 }`. `total` et `avoidable` sont tous deux `{ low, mid, high, model, methodology }` avec une **incertitude multiplicative 2×** (`low = mid/2`, `high = mid×2`). Le tag `methodology` distingue `total` (`"sci_v1_numerator"` : `(E × I) + M` sommé sur les traces) de `avoidable` (`"sci_v1_operational_ratio"` : ratio global aveugle à la région, exclut l'embodié). Valeurs `model`, le plus précis gagne : `"scaphandre_rapl"` → `"cloud_specpower"` → `"io_proxy_v2"` → `"io_proxy_v1"`.
- **`regions[]`** : breakdown par région avec `{ region, grid_intensity_gco2_kwh, pue, io_ops, co2_gco2, intensity_source }`, **trié par `co2_gco2` décroissant** (régions à plus fort impact en premier) avec tiebreak alphabétique. `intensity_source` vaut `"annual"` ou `"hourly"` selon quelle table carbone a été consultée pour la région.

Les données d'intensité carbone sont embarquées dans le binaire (aucun appel réseau sortant). Voir `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md` pour la formule complète et la méthodologie, et `docs/FR/LIMITATIONS-FR.md#précision-des-estimations-carbone` pour le disclaimer directionnel / non-réglementaire.

#### `[green.scaphandre]` (optionnel, opt-in)

Intégration opt-in avec [Scaphandre](https://github.com/hubblo-org/scaphandre) pour la mesure énergétique par-processus sur les hôtes Linux avec support Intel RAPL. Quand cette section est configurée, le daemon `watch` lance une tâche de fond qui scrape l'endpoint Prometheus de Scaphandre toutes les `scrape_interval_secs` secondes et utilise les lectures de puissance mesurées pour remplacer la constante `ENERGY_PER_IO_OP_KWH` fixe pour chaque service mappé.

| Champ                  | Type   | Défaut    | Description                                                                                                                                                |
|------------------------|--------|-----------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | chaîne | *(aucun)* | URL complète de l'endpoint Prometheus `/metrics` de Scaphandre. Doit commencer par `http://` (TLS non supporté). Obligatoire quand la section est présente |
| `scrape_interval_secs` | entier | `5`       | Fréquence de scrape, en secondes. Plage valide : 1-3600                                                                                                    |
| `process_map`          | table  | `{}`      | Mappe les noms de service perf-sentinel (depuis `service.name` du span) aux labels `exe` Scaphandre                                                        |

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5

[green.scaphandre.process_map]
"order-svc" = "java"
"chat-svc" = "dotnet"
"game-svc" = "game"
```

**Ignoré en mode batch `analyze`.** Seul le daemon `watch` lance le scraper. La commande `analyze` utilise toujours le modèle proxy quelle que soit cette section.

**Comportement de fallback.** Quand l'endpoint est inaccessible, qu'un service n'est pas présent dans `process_map`, ou qu'un service a eu zéro ops dans la fenêtre de scrape courante, l'étape de scoring retombe sur le modèle proxy pour ces spans. Le premier échec est logué en niveau `warn` ; les échecs suivants en `debug` pour éviter le spam. La jauge Prometheus `perf_sentinel_scaphandre_last_scrape_age_seconds` permet aux opérateurs de détecter un scraper bloqué.

**Limites de précision (important).** Scaphandre améliore le coefficient énergétique **au niveau service** mais ne donne PAS d'attribution par-finding. RAPL est au niveau processus, pas au niveau span : deux findings dans le même processus pendant la même fenêtre de scrape partagent le même coefficient. Voir `docs/FR/LIMITATIONS-FR.md#limites-de-précision-scaphandre` pour la discussion complète.

#### `[green.cloud]` (optionnel, opt-in)

Estimation d'énergie cloud-native via utilisation CPU% + interpolation SPECpower. Quand cette section est configurée, le daemon `watch` scrape les métriques CPU% depuis un endpoint Prometheus/VictoriaMetrics et utilise une table de lookup embarquée (watts idle/max par type d'instance cloud) pour estimer la consommation énergétique par service. Supporte AWS, GCP, Azure, et le matériel on-premise avec surcharge manuelle des watts.

| Champ                   | Type   | Défaut    | Description                                                                  |
|-------------------------|--------|-----------|------------------------------------------------------------------------------|
| `prometheus_endpoint`   | chaîne | *(aucun)* | URL de base de l'API HTTP Prometheus (ex. `http://prometheus:9090`). Obligatoire. |
| `scrape_interval_secs`  | entier | `15`      | Intervalle de polling en secondes (plage : 1-3600).                          |
| `default_provider`      | chaîne | *(aucun)* | Fournisseur cloud par défaut : `"aws"`, `"gcp"`, `"azure"`.                 |
| `default_instance_type` | chaîne | *(aucun)* | Type d'instance de fallback pour les services non mappés.                    |
| `cpu_metric`            | chaîne | *(aucun)* | Métrique/requête PromQL par défaut pour l'utilisation CPU.                   |

Les entrées par service dans `[green.cloud.services]` supportent deux formes :

**Instance cloud (lookup dans la table) :**

```toml
[green.cloud]
prometheus_endpoint = "http://prometheus:9090"
scrape_interval_secs = 15
default_provider = "aws"

[green.cloud.services]
"account-svc" = { provider = "aws", instance_type = "c5.4xlarge" }
"api-asia" = { provider = "gcp", instance_type = "n2-standard-8" }
"analytics" = { provider = "azure", instance_type = "Standard_D8s_v3" }
```

**Watts manuels (on-premise ou matériel custom) :**

```toml
[green.cloud.services]
"my-service" = { idle_watts = 45, max_watts = 120 }
```

**Ignoré en mode `analyze` batch.** Seul le daemon `watch` lance le scraper Prometheus.

**Comportement de repli.** Si le endpoint Prometheus est inaccessible, le daemon utilise le modèle proxy pour tous les services configurés cloud. Les types d'instance inconnus retombent sur un défaut au niveau du fournisseur.

**Limites de précision.** Le modèle d'interpolation SPECpower a une précision d'environ +/-30%, meilleure que le modèle proxy mais moins précise que Scaphandre RAPL. Voir `docs/FR/LIMITATIONS-FR.md#limites-de-précision-du-cloud-specpower` pour les détails.

### `[daemon]`

Paramètres du mode streaming (`perf-sentinel watch`).

| Champ                  | Type     | Défaut                      | Description                                                                                                                                                                                                                                                                                                                      |
|------------------------|----------|-----------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`       | chaîne   | `"127.0.0.1"`               | Adresse IP de liaison pour les endpoints OTLP et métriques. Utilisez `127.0.0.1` pour un accès local uniquement. **Attention :** définir une adresse non-loopback expose des endpoints non authentifiés sur le réseau, utilisez un reverse proxy ou une politique réseau                                                         |
| `listen_port_http`     | entier   | `4318`                      | Port pour le récepteur OTLP HTTP et l'endpoint Prometheus `/metrics` (plage : 1-65535)                                                                                                                                                                                                                                           |
| `listen_port_grpc`     | entier   | `4317`                      | Port pour le récepteur OTLP gRPC (plage : 1-65535)                                                                                                                                                                                                                                                                               |
| `json_socket`          | chaîne   | `"/tmp/perf-sentinel.sock"` | Chemin du socket Unix pour l'ingestion d'événements JSON                                                                                                                                                                                                                                                                         |
| `max_active_traces`    | entier   | `10000`                     | Nombre maximum de traces conservées en mémoire. En cas de dépassement, la trace la plus ancienne est évincée (LRU)                                                                                                                                                                                                               |
| `trace_ttl_ms`         | entier   | `30000`                     | Durée de vie des traces en millisecondes. Les traces plus anciennes sont évincées et analysées                                                                                                                                                                                                                                   |
| `sampling_rate`        | flottant | `1.0`                       | Fraction des traces à analyser (0.0 à 1.0). Réduire en dessous de 1.0 pour diminuer la charge dans les environnements à fort trafic                                                                                                                                                                                              |
| `max_events_per_trace` | entier   | `1000`                      | Nombre maximum d'événements stockés par trace (buffer circulaire, max 100000). Les événements les plus anciens sont supprimés en cas de dépassement                                                                                                                                                                              |
| `max_payload_size`     | entier   | `1048576`                   | Taille maximale en octets d'un payload JSON unique (défaut : 1 Mo, max 100 Mo)                                                                                                                                                                                                                                                   |
| `environment`          | chaîne   | `"staging"`                 | Label d'environnement de déploiement. Valeurs acceptées : `"staging"` (défaut, confiance moyenne) ou `"production"` (confiance élevée). Tague chaque finding avec le champ `confidence` correspondant pour les consommateurs en aval (perf-lint). Insensible à la casse ; toute autre valeur est rejetée au chargement de la config |

## Configuration minimale

Un fichier vide ou l'absence de fichier utilise tous les défauts. Une configuration minimale pour la CI peut se limiter aux seuils :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
io_waste_ratio_max = 0.25
```

## Exemple de configuration complète

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30

[detection]
n_plus_one_min_occurrences = 5
window_duration_ms = 500
slow_query_threshold_ms = 500
slow_query_min_occurrences = 3
max_fanout = 20
chatty_service_min_calls = 15
pool_saturation_concurrent_threshold = 10
serialized_min_sequential = 3

[green]
enabled = true
default_region = "eu-west-3"

[daemon]
listen_address = "127.0.0.1"
listen_port_http = 4318
listen_port_grpc = 4317
json_socket = "/tmp/perf-sentinel.sock"
max_active_traces = 10000
trace_ttl_ms = 30000
sampling_rate = 1.0
max_events_per_trace = 1000
max_payload_size = 1048576
```

## Format plat legacy

Pour la rétrocompatibilité, perf-sentinel accepte également un format plat (non sectionné) :

```toml
n_plus_one_threshold = 5
window_duration_ms = 500
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30
```

Lorsque les deux formats sont présents, les valeurs sectionnées ont priorité sur les valeurs plates. Le format sectionné est recommandé pour les nouvelles configurations.

## Variables d'environnement

Les fichiers de configuration ne doivent jamais contenir de secrets. Pour les valeurs sensibles (clés API, tokens), utilisez des variables d'environnement dans vos outils de déploiement. perf-sentinel ne lit pas lui-même de variables d'environnement pour la configuration.
