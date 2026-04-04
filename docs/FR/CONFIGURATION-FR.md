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

### `[green]`

Configuration du scoring GreenOps.

| Champ     | Type    | Défaut    | Description                                                                                                                                                                       |
|-----------|---------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled` | booléen | `true`    | Active le scoring GreenOps (IIS, ratio de gaspillage, top offenders)                                                                                                              |
| `region`  | chaîne  | *(aucun)* | Région cloud ou code pays pour la conversion en gCO2eq. Lorsque défini, le rapport inclut les émissions carbone estimées. Exemples : `"eu-west-3"`, `"us-east-1"`, `"FR"`, `"DE"` |

Lorsque `region` n'est pas défini, le rapport affiche les compteurs d'opérations I/O bruts sans conversion carbone. La table d'intensité carbone est embarquée dans le binaire (aucun appel réseau).

### `[daemon]`

Paramètres du mode streaming (`perf-sentinel watch`).

| Champ                  | Type     | Défaut                      | Description                                                                                                                                                                                                                                                               |
|------------------------|----------|-----------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`       | chaîne   | `"127.0.0.1"`               | Adresse IP de liaison pour les endpoints OTLP et métriques. Utilisez `127.0.0.1` pour un accès local uniquement. **Attention :** définir une adresse non-loopback expose des endpoints non authentifiés sur le réseau : utilisez un reverse proxy ou une politique réseau |
| `listen_port_http`     | entier   | `4318`                      | Port pour le récepteur OTLP HTTP et l'endpoint Prometheus `/metrics` (plage : 1-65535)                                                                                                                                                                                    |
| `listen_port_grpc`     | entier   | `4317`                      | Port pour le récepteur OTLP gRPC (plage : 1-65535)                                                                                                                                                                                                                        |
| `json_socket`          | chaîne   | `"/tmp/perf-sentinel.sock"` | Chemin du socket Unix pour l'ingestion d'événements JSON                                                                                                                                                                                                                  |
| `max_active_traces`    | entier   | `10000`                     | Nombre maximum de traces conservées en mémoire. En cas de dépassement, la trace la plus ancienne est évincée (LRU)                                                                                                                                                        |
| `trace_ttl_ms`         | entier   | `30000`                     | Durée de vie des traces en millisecondes. Les traces plus anciennes sont évincées et analysées                                                                                                                                                                            |
| `sampling_rate`        | flottant | `1.0`                       | Fraction des traces à analyser (0.0 à 1.0). Réduire en dessous de 1.0 pour diminuer la charge dans les environnements à fort trafic                                                                                                                                       |
| `max_events_per_trace` | entier   | `1000`                      | Nombre maximum d'événements stockés par trace (buffer circulaire, max 100000). Les événements les plus anciens sont supprimés en cas de dépassement                                                                                                                       |
| `max_payload_size`     | entier   | `1048576`                   | Taille maximale en octets d'un payload JSON unique (défaut : 1 Mo, max 100 Mo)                                                                                                                                                                                            |

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

[green]
enabled = true
region = "eu-west-3"

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
