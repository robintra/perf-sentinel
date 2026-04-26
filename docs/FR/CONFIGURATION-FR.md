# Référence de configuration

perf-sentinel se configure via un fichier `.perf-sentinel.toml`. Tous les champs sont optionnels et ont des valeurs par défaut raisonnables.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands_dark.svg">
  <img alt="Vue d'ensemble des commandes CLI" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands.svg">
</picture>

## Sommaire

- [Sous-commandes](#sous-commandes) : quelles sous-commandes lisent `.perf-sentinel.toml`.
- [Sections](#sections) : référence complète par section (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`, ...).
- [Configuration minimale](#configuration-minimale) : le `.perf-sentinel.toml` le plus court utile.
- [Exemple de configuration complète](#exemple-de-configuration-complète) : chaque section peuplée avec des valeurs d'exemple.
- [Format plat legacy](#format-plat-legacy) : format pré-section conservé pour la compatibilité ascendante.
- [Variables d'environnement](#variables-denvironnement) : quelles variables d'environnement surchargent les valeurs du fichier de config.

## Sous-commandes

| Sous-commande | Description                                                                                                                                                                                                                     |
|---------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `analyze`     | Analyse batch de fichiers de traces. Lit depuis un fichier ou stdin                                                                                                                                                             |
| `explain`     | Vue arborescente d'une trace avec findings annotés en ligne                                                                                                                                                                     |
| `watch`       | Mode daemon : ingestion OTLP temps réel et détection en streaming                                                                                                                                                               |
| `query`       | Interroge un daemon en cours d'exécution. Sortie colorée par défaut, `--format json` pour le scripting. `query inspect` ouvre un TUI live                                                                                       |
| `demo`        | Lance l'analyse sur un jeu de données de démo embarqué                                                                                                                                                                          |
| `bench`       | Benchmark du débit sur un fichier de traces                                                                                                                                                                                     |
| `pg-stat`     | Analyse des exports `pg_stat_statements` (CSV/JSON ou Prometheus)                                                                                                                                                               |
| `inspect`     | TUI interactif pour naviguer les traces, findings et arbres de spans                                                                                                                                                            |
| `diff`        | Compare deux jeux de traces et émet un rapport delta (findings nouveaux/résolus, changements de sévérité, deltas I/O par endpoint). Sortie texte/JSON/SARIF                                                                     |
| `report`      | Dashboard HTML single-file pour l'exploration post-mortem dans un navigateur. Accepte un fichier de traces, un Report JSON pré-calculé, ou stdin via `--input -` (auto-détecte array-d'events vs objet Report, tolérant au BOM) |
| `tempo`       | Récupère des traces depuis une API HTTP Grafana Tempo (par ID de trace ou par recherche service puis fetch) et les pipe dans le pipeline d'analyse. Gaté derrière la feature `tempo`                                            |
| `calibrate`   | Corrèle un fichier de traces avec des mesures d'énergie réelles (Scaphandre, CSV cloud monitoring) et émet un TOML de coefficients I/O-vers-énergie à charger via `[green] calibration_file`                                    |

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

| Champ                                  | Type   | Défaut | Description                                                                                                                                                                      |
|----------------------------------------|--------|--------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `n_plus_one_min_occurrences`           | entier | `5`    | Nombre minimum d'occurrences (avec des paramètres distincts) pour signaler un pattern N+1                                                                                        |
| `window_duration_ms`                   | entier | `500`  | Fenêtre temporelle en millisecondes dans laquelle les opérations répétées sont considérées comme un pattern N+1                                                                  |
| `slow_query_threshold_ms`              | entier | `500`  | Seuil de durée en millisecondes au-dessus duquel une opération est considérée comme lente                                                                                        |
| `slow_query_min_occurrences`           | entier | `3`    | Nombre minimum d'occurrences lentes du même template pour générer un finding                                                                                                     |
| `max_fanout`                           | entier | `20`   | Nombre maximum de spans enfants par parent avant de signaler un fanout excessif (plage : 1-100000)                                                                               |
| `chatty_service_min_calls`             | entier | `15`   | Nombre minimum d'appels HTTP sortants par trace pour signaler un service bavard. Severite : warning > seuil, critical > 3x seuil.                                                |
| `pool_saturation_concurrent_threshold` | entier | `10`   | Nombre maximal de spans SQL concurrents par service pour signaler un risque de saturation du pool de connexions. Utilise un algorithme de balayage sur les timestamps des spans. |
| `serialized_min_sequential`            | entier | `3`    | Nombre minimum d'appels séquentiels indépendants (même parent, sans chevauchement, templates différents) pour signaler des appels potentiellement parallélisables.               |
| `sanitizer_aware_classification`       | chaîne | `"auto"`| Classification des groupes SQL dont les littéraux ont été remplacés par `?` par le sanitizer d'instruction d'un agent OpenTelemetry. Une valeur parmi `"auto"`, `"always"`, `"never"`. Voir la note ci-dessous.                                                              |

#### `sanitizer_aware_classification`

Les agents OpenTelemetry activent par défaut le sanitizer d'instructions
SQL pour éviter de laisser fuir des PII dans les attributs de trace.
Lorsqu'il est actif, chaque span d'un N+1 induit par un ORM arrive dans
perf-sentinel avec le même template et aucun paramètre extractible. La
règle standard de paramètres distincts rejette donc le groupe et le
détecteur de redondance le récupère sous l'étiquette `redundant_sql` au
lieu de `n_plus_one_sql`. Ce paramètre contrôle l'heuristique qui
restaure la classification correcte :

- `"auto"` (défaut) : émet `n_plus_one_sql` quand un marqueur ORM est
  présent dans les `instrumentation_scopes` des spans (Spring Data,
  Hibernate, EF Core, SQLAlchemy, ActiveRecord, GORM, Prisma, Diesel,
  ...) ou quand la variance des durées par span est suffisante pour
  indiquer des accès à des lignes distinctes. Sinon, le groupe reste à
  la charge du détecteur de redondance.
- `"always"` : reclassifie tout groupe sanitisé qui atteint
  `n_plus_one_min_occurrences` spans en `n_plus_one_sql`. Plus agressif,
  peut requalifier une vraie redondance à un seul paramètre.
- `"never"` : désactive complètement l'heuristique et reproduit le
  comportement antérieur à la 0.5.7.

Les findings reclassifiés par l'heuristique portent
`classification_method = "sanitizer_heuristic"` dans leur représentation
JSON, ce qui permet à un opérateur de repérer où elle se déclenche. Les
findings produits par la règle standard omettent ce champ.

### `[green]`

Configuration du scoring GreenOps alignée sur [SCI v1.0](https://github.com/Green-Software-Foundation/sci) (termes opérationnel + embodié, intervalles de confiance, multi-région).

| Champ                              | Type     | Défaut    | Description                                                                                                                                                                                                                                                                                                                                                                                                                                    |
|------------------------------------|----------|-----------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`                          | booléen  | `true`    | Active le scoring GreenOps (IIS, ratio de gaspillage, top offenders, CO₂)                                                                                                                                                                                                                                                                                                                                                                      |
| `default_region`                   | chaîne   | *(aucun)* | Région cloud de fallback utilisée quand ni l'attribut `cloud.region` du span ni le mapping `service_regions` ne résout une région. Exemples : `"eu-west-3"`, `"us-east-1"`, `"FR"`                                                                                                                                                                                                                                                             |
| `embodied_carbon_per_request_gco2` | flottant | `0.001`   | Terme `M` SCI v1.0 : émissions de fabrication matérielle amorties par requête (par trace), en gCO₂eq. Indépendant de la région. Mettre à `0.0` pour désactiver le carbone embodié                                                                                                                                                                                                                                                              |
| `use_hourly_profiles`              | booléen  | `true`    | Quand `true`, l'étape de scoring utilise des intensités réseau spécifiques à l'heure pour les 30+ régions disposant de profils horaires embarqués. Les régions avec profils mois x heure (FR, DE, GB, US-East) prennent aussi en compte la variation saisonnière. Les rapports sont tagués `model = "io_proxy_v3"` (mois x heure) ou `"io_proxy_v2"` (horaire annuel plat). Mettre à `false` pour figer les rapports sur le modèle annuel plat |
| `hourly_profiles_file`             | chaîne   | *(aucun)* | Chemin vers un fichier JSON de profils horaires personnalisés. Peut être absolu ou relatif au fichier de config. Les profils personnalisés prennent priorité sur les profils embarqués pour la même clé de région                                                                                                                                                                                                                              |
| `per_operation_coefficients`       | booléen  | `true`    | Quand `true`, le modèle proxy pondère l'énergie par opération : SQL SELECT (0.5x), INSERT/UPDATE (1.5x), DELETE (1.2x) et tailles de payload HTTP (petit <10 Ko : 0.8x, moyen 10 Ko-1 Mo : 1.2x, grand >1 Mo : 2.0x). Ne s'applique pas quand l'énergie mesurée par Scaphandre ou cloud SPECpower est disponible. Mettre à `false` pour utiliser le coefficient plat `ENERGY_PER_IO_OP_KWH`                                                    |
| `include_network_transport`        | booléen  | `false`   | Quand `true`, ajoute un terme d'énergie de transport réseau pour les appels HTTP inter-régions. Requiert `response_size_bytes` sur les spans HTTP (attribut OTel `http.response.body.size`) et la région cible mappée via `[green.service_regions]`. Les appels intra-région sont exclus. Le CO₂ transport apparaît comme `transport_gco2` dans le rapport JSON                                                                                |
| `network_energy_per_byte_kwh`      | flottant | `4e-11`   | Énergie par octet pour le transport réseau (kWh/octet). Défaut 0.04 kWh/Go, milieu de la fourchette 0.03-0.06 de Mytton et al. (2024). Utilisé uniquement quand `include_network_transport = true`                                                                                                                                                                                                                                             |

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

- **`co2`** : objet structuré `{ total, avoidable, operational_gco2, embodied_gco2 }`. `total` et `avoidable` sont tous deux `{ low, mid, high, model, methodology }` avec une **incertitude multiplicative 2×** (`low = mid/2`, `high = mid×2`). Le tag `methodology` distingue `total` (`"sci_v1_numerator"` : `(E × I) + M` sommé sur les traces ou `"sci_v1_numerator+transport"` quand l'énergie transport réseau est incluse) de `avoidable` (`"sci_v1_operational_ratio"` : ratio global aveugle à la région, exclut l'embodié). Valeurs `model`, le plus précis gagne : `"electricity_maps_api"` → `"scaphandre_rapl"` → `"cloud_specpower"` → `"io_proxy_v3"` → `"io_proxy_v2"` → `"io_proxy_v1"`. Quand des facteurs de calibration sont actifs sur les modèles proxy, `+cal` est ajouté (ex. `"io_proxy_v2+cal"`).
- **`regions[]`** : breakdown par région avec `{ region, grid_intensity_gco2_kwh, pue, io_ops, co2_gco2, intensity_source }`, **trié par `co2_gco2` décroissant** (régions à plus fort impact en premier) avec tiebreak alphabétique. `intensity_source` vaut `"annual"`, `"hourly"`, `"monthly_hourly"` ou `"real_time"` (API Electricity Maps) selon quelle source d'intensité carbone a été utilisée pour la région.

Les données d'intensité carbone sont embarquées dans le binaire (aucun appel réseau sortant). Voir `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md` pour la formule complète et la méthodologie et `docs/FR/LIMITATIONS-FR.md#précision-des-estimations-carbone` pour le disclaimer directionnel / non-réglementaire.

#### Profils horaires fournis par l'utilisateur

Mettre `[green] hourly_profiles_file` vers un fichier JSON pour fournir vos propres profils horaires. C'est utile pour les opérateurs de datacenter avec leurs propres PPAs (power purchase agreements) ou pour surcharger les données embarquées avec des mesures locales.

```json
{
  "profiles": {
    "my-datacenter": {
      "type": "flat_year",
      "hours": [45.0, 44.0, 43.0, "... 24 valeurs au total ..."]
    },
    "eu-west-3": {
      "type": "monthly",
      "months": [
        [50.0, 49.0, "... 24 valeurs pour janvier ..."],
        ["... 11 mois supplémentaires ..."]
      ]
    }
  }
}
```

Les profils fournis par l'utilisateur ont priorité sur les profils embarqués pour la même clé de région. Validation au chargement de la config : chaque `flat_year` doit contenir exactement 24 valeurs, chaque `monthly` doit contenir exactement 12 tableaux de 24 valeurs. Toutes les valeurs doivent être finies et non-négatives. Si la clé de région existe dans la table carbone embarquée, un warning est loggé quand la moyenne du profil s'écarte de plus de 5% de la valeur annuelle, mais le profil est quand même accepté.

#### Alias de régions pour les profils horaires

Les alias de code pays et les synonymes de fournisseurs cloud résolvent vers le même profil horaire. Par exemple, `"fr"`, `"francecentral"` et `"europe-west9"` mappent tous vers le profil `eu-west-3` (France). Mappings notables :

- `"us"`, `"eastus"` → `us-east-1` (US-East, la région de déploiement US la plus courante)
- `"westeurope"`, `"nl"` → `eu-west-4` (Pays-Bas)
- `"northeurope"`, `"ie"` → `eu-west-1` (Irlande)
- `"uksouth"`, `"gb"`, `"uk"` → `eu-west-2` (Royaume-Uni)
- `"westus2"` → `us-west-2` (Oregon)

La table complète des alias se trouve dans `score/carbon_profiles.rs`. Si votre clé de région n'est pas aliasée, la valeur annuelle plate de la table carbone principale est utilisée.

#### `[green.scaphandre]` (optionnel, opt-in)

Intégration opt-in avec [Scaphandre](https://github.com/hubblo-org/scaphandre) pour la mesure énergétique par processus sur les hôtes Linux avec support Intel RAPL. Quand cette section est configurée, le daemon `watch` lance une tâche de fond qui scrape l'endpoint Prometheus de Scaphandre toutes les `scrape_interval_secs` secondes et utilise les lectures de puissance mesurées pour remplacer la constante `ENERGY_PER_IO_OP_KWH` fixe pour chaque service mappé.

| Champ                  | Type   | Défaut    | Description                                                                                                                                                                           |
|------------------------|--------|-----------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | chaîne | *(aucun)* | URL complète de l'endpoint Prometheus `/metrics` de Scaphandre. Doit commencer par `http://` ou `https://` (TLS supporté via hyper-rustls). Obligatoire quand la section est présente |
| `scrape_interval_secs` | entier | `5`       | Fréquence de scrape, en secondes. Plage valide : 1-3600                                                                                                                               |
| `process_map`          | table  | `{}`      | Mappe les noms de service perf-sentinel (depuis `service.name` du span) aux labels `exe` Scaphandre                                                                                   |

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

**Comportement de fallback.** Quand l'endpoint est inaccessible, qu'un service n'est pas présent dans `process_map` ou qu'un service a eu zéro ops dans la fenêtre de scrape courante, l'étape de scoring retombe sur le modèle proxy pour ces spans. Le premier échec est logué en niveau `warn` ; les échecs suivants en `debug` pour éviter le spam. La jauge Prometheus `perf_sentinel_scaphandre_last_scrape_age_seconds` permet aux opérateurs de détecter un scraper bloqué.

**Limites de précision (important).** Scaphandre améliore le coefficient énergétique **au niveau service** mais ne donne PAS d'attribution par finding. RAPL est au niveau processus, pas au niveau span : deux findings dans le même processus pendant la même fenêtre de scrape partagent le même coefficient. Voir `docs/FR/LIMITATIONS-FR.md#limites-de-précision-scaphandre` pour la discussion complète.

#### `[green.cloud]` (optionnel, opt-in)

Estimation d'énergie cloud-native via utilisation CPU% + interpolation SPECpower. Quand cette section est configurée, le daemon `watch` scrape les métriques CPU% depuis un endpoint Prometheus/VictoriaMetrics et utilise une table de lookup embarquée (watts idle/max par type d'instance cloud) pour estimer la consommation énergétique par service. Supporte AWS, GCP, Azure et le matériel on-premise avec surcharge manuelle des watts.

| Champ                   | Type   | Défaut    | Description                                                                                                                                   |
|-------------------------|--------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| `prometheus_endpoint`   | chaîne | *(aucun)* | URL de base de l'API HTTP Prometheus (ex. `http://prometheus:9090` ou `https://prometheus:9090`). TLS supporté via hyper-rustls. Obligatoire. |
| `scrape_interval_secs`  | entier | `15`      | Intervalle de polling en secondes (plage : 1-3600).                                                                                           |
| `default_provider`      | chaîne | *(aucun)* | Fournisseur cloud par défaut : `"aws"`, `"gcp"`, `"azure"`.                                                                                   |
| `default_instance_type` | chaîne | *(aucun)* | Type d'instance de fallback pour les services non mappés.                                                                                     |
| `cpu_metric`            | chaîne | *(aucun)* | Métrique/requête PromQL par défaut pour l'utilisation CPU.                                                                                    |

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

#### `[green.electricity_maps]` (optionnel, opt-in)

Intensité carbone en temps réel via l'API Electricity Maps. Mode daemon uniquement.

| Champ                | Type    | Défaut                               | Description                                                 |
|----------------------|---------|--------------------------------------|-------------------------------------------------------------|
| `api_key`            | string  | aucun                                | Token API. Préférez la variable `PERF_SENTINEL_EMAPS_TOKEN` |
| `endpoint`           | string  | `https://api.electricitymaps.com/v3` | URL de base (`http://` ou `https://`)                       |
| `poll_interval_secs` | integer | `300`                                | Intervalle de sondage en secondes (plage : 60-86400)        |

La sous-table `region_map` associe les régions cloud aux zones Electricity Maps :

```toml
[green.electricity_maps]
# Utilisez PERF_SENTINEL_EMAPS_TOKEN au lieu de api_key dans le fichier
poll_interval_secs = 300

[green.electricity_maps.region_map]
"eu-west-3" = "FR"
"us-east-1" = "US-NY"
"ap-northeast-1" = "JP-TK"
```

**Staleness :** si le dernier sondage réussi date de plus de 3x `poll_interval_secs`, le scraper retombe sur les profils horaires embarqués.

**Limites de débit :** le tier gratuit d'Electricity Maps autorise environ 30 requêtes par mois et par zone. Les utilisateurs du tier gratuit doivent mettre `poll_interval_secs = 3600` ou plus. La valeur par défaut de 300s est prévue pour les plans payants.

#### `[green] calibration_file` (optionnel)

Chemin vers un fichier TOML de calibration généré par `perf-sentinel calibrate`. Les facteurs par service multiplient l'énergie proxy par opération.

```toml
[green]
calibration_file = ".perf-sentinel-calibration.toml"
```

**Limites de taille d'entrée pour `perf-sentinel calibrate`.** Les deux entrées sont plafonnées pour éviter une consommation mémoire incontrôlée : le fichier `--traces` utilise `config.max_payload_size` (défaut 1 MiB, identique à `analyze`) et le CSV `--measured-energy` est plafonné à 64 MiB. Calibrate termine avec une erreur claire si l'un des fichiers dépasse sa limite. 64 MiB est généreux pour des milliers d'échantillons RAPL par minute ; si vous avez besoin de plus, augmentez `max_payload_size` et ouvrez une issue décrivant la charge de travail.

#### `[tempo]` (optionnel)

Configuration pour la sous-commande `perf-sentinel tempo`. La sous-commande s'exécute en **mode batch** (pas daemon), récupère les traces depuis l'API HTTP d'un Grafana Tempo et les passe dans le pipeline d'analyse standard. Toutes les valeurs ci-dessous peuvent être définies via les flags CLI (les flags ont priorité).

| Champ        | Type    | Défaut | Description                                       |
|--------------|---------|--------|---------------------------------------------------|
| `endpoint`   | string  | aucun  | URL de l'API HTTP Tempo (ex. `http://tempo:3200`) |
| `max_traces` | integer | `100`  | Nombre maximum de traces en mode recherche        |

### `[daemon]`

Paramètres du mode streaming (`perf-sentinel watch`).

| Champ                   | Type     | Défaut                      | Description                                                                                                                                                                                                                                                                                                                         |
|-------------------------|----------|-----------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`        | chaîne   | `"127.0.0.1"`               | Adresse IP de liaison pour les endpoints OTLP et métriques. Utilisez `127.0.0.1` pour un accès local uniquement. **Attention :** définir une adresse non-loopback expose des endpoints non authentifiés sur le réseau, utilisez un reverse proxy ou une politique réseau                                                            |
| `listen_port_http`      | entier   | `4318`                      | Port pour le récepteur OTLP HTTP et l'endpoint Prometheus `/metrics` (plage : 1-65535)                                                                                                                                                                                                                                              |
| `listen_port_grpc`      | entier   | `4317`                      | Port pour le récepteur OTLP gRPC (plage : 1-65535)                                                                                                                                                                                                                                                                                  |
| `json_socket`           | chaîne   | `"/tmp/perf-sentinel.sock"` | Chemin du socket Unix pour l'ingestion d'événements JSON                                                                                                                                                                                                                                                                            |
| `max_active_traces`     | entier   | `10000`                     | Nombre maximum de traces conservées en mémoire. En cas de dépassement, la trace la plus ancienne est évincée (LRU). Plage : 1 à 1 000 000                                                                                                                                                                                           |
| `trace_ttl_ms`          | entier   | `30000`                     | Durée de vie des traces en millisecondes. Les traces plus anciennes sont évincées et analysées. Plage : 100 à 3 600 000                                                                                                                                                                                                             |
| `sampling_rate`         | flottant | `1.0`                       | Fraction des traces à analyser (0.0 à 1.0). Réduire en dessous de 1.0 pour diminuer la charge dans les environnements à fort trafic                                                                                                                                                                                                 |
| `max_events_per_trace`  | entier   | `1000`                      | Nombre maximum d'événements stockés par trace (buffer circulaire). Les événements les plus anciens sont supprimés en cas de dépassement. Plage : 1 à 100 000                                                                                                                                                                        |
| `max_payload_size`      | entier   | `1048576`                   | Taille maximale en octets d'un payload JSON unique (défaut : 1 Mo). Plage : 1 024 à 104 857 600 (100 Mo)                                                                                                                                                                                                                            |
| `environment`           | chaîne   | `"staging"`                 | Label d'environnement de déploiement. Valeurs acceptées : `"staging"` (défaut, confiance moyenne) ou `"production"` (confiance élevée). Tague chaque finding avec le champ `confidence` correspondant pour les consommateurs en aval (perf-lint planifié). Insensible à la casse ; toute autre valeur est rejetée au chargement de la config |
| `tls_cert_path`         | chaîne   | *(absent)*                  | Chemin vers une chaîne de certificats TLS au format PEM pour les récepteurs OTLP. Quand renseigné avec `tls_key_path`, les listeners gRPC et HTTP utilisent TLS. Quand absent, les listeners utilisent TCP en clair. Chaque listener TLS borne à 128 les handshakes en vol simultanés (non configurable) et coupe les pairs qui ne terminent pas le handshake en 10 secondes                                                                                                                 |
| `tls_key_path`          | chaîne   | *(absent)*                  | Chemin vers la clé privée TLS au format PEM. Doit être renseigné avec `tls_cert_path` (les deux ou aucun). Sous Unix, le daemon avertit si le fichier de clé est lisible par le groupe ou les autres                                                                                                                                |
| `api_enabled`           | booléen  | `true`                      | Active les endpoints de l'API de requêtage du daemon (`/api/findings`, `/api/explain/{trace_id}`, `/api/correlations`, `/api/status`). Mettre à `false` pour désactiver l'API tout en conservant l'ingestion OTLP et `/metrics`                                                                                                     |
| `max_retained_findings` | entier   | `10000`                     | Nombre maximum de findings récents conservés dans le buffer circulaire du daemon pour l'API de requêtage. Les findings les plus anciens sont évincés quand la limite est atteinte. Plage : 0 à 10 000 000, où `0` désactive complètement le store et libère sa mémoire (recommandé quand `api_enabled = false`)                     |

##### Zones de confort et avertissements au démarrage

Les limites du daemon acceptent toute valeur à l'intérieur de leurs bornes dures (rejetées au chargement de la config), mais `perf-sentinel watch` émet un log `WARN` unique au démarrage quand une valeur sort de la zone de confort recommandée. L'avertissement est informatif : le daemon continue de tourner. Sert de garde-fou pour vérifier qu'une valeur inhabituelle est bien volontaire.

| Champ                   | Zone de confort         | Pourquoi une valeur hors zone est inhabituelle                                                                                                                     |
|-------------------------|-------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `max_payload_size`      | 256 Kio à 16 Mio        | Plus petit risque de rejeter des batches OTLP légitimes ; plus grand augmente la latence d'ingestion et le RSS                                                     |
| `max_active_traces`     | 1 000 à 100 000         | Plus petit déclenche une éviction LRU agressive ; plus grand fait croître la mémoire à peu près linéairement                                                       |
| `max_events_per_trace`  | 100 à 10 000            | Plus petit tronque les traces complexes ; plus grand n'améliore que rarement la qualité de détection                                                               |
| `max_retained_findings` | 100 à 100 000 (ou `0`)  | Plus petit évince les findings avant que `/api/findings` ne puisse les servir ; plus grand garde un backlog en mémoire. `0` désactive le store et reste silencieux |
| `trace_ttl_ms`          | 1 000 à 600 000         | Sous 1s, les traces sont vidées avant que les spans lents n'arrivent ; au-dessus de 10min, des traces presque mortes restent en mémoire                            |
| `max_fanout`            | 5 à 1 000               | Plus petit inonde le store de findings de bruit ; plus grand supprime la plupart des détections de fan-out                                                         |

#### `[daemon.correlation]` (optionnel)

Corrélation temporelle cross-trace en mode daemon. Quand activé, le daemon détecte les co-occurrences récurrentes entre findings de services ou traces différents (ex. "chaque fois que le N+1 dans order-svc se déclenche, une saturation du pool apparaît dans payment-svc dans les 2 secondes").

| Champ                | Type     | Défaut  | Description                                                                                                                        |
|----------------------|----------|---------|------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`            | booléen  | `false` | Active la corrélation cross-trace. Nécessite le mode daemon `watch` avec un trafic soutenu pour des résultats utiles               |
| `window_minutes`     | entier   | `10`    | Fenêtre glissante en minutes sur laquelle les co-occurrences sont suivies                                                          |
| `lag_threshold_ms`   | entier   | `2000`  | Décalage temporel maximum en millisecondes entre deux findings pour les considérer co-occurrents                                   |
| `min_co_occurrences` | entier   | `3`     | Nombre minimum de co-occurrences avant de remonter une corrélation                                                                 |
| `min_confidence`     | flottant | `0.5`   | Score de confiance minimum (0.0 à 1.0) pour remonter une corrélation. Calculé comme `co_occurrence_count / total_occurrences_of_A` |
| `max_tracked_pairs`  | entier   | `1000`  | Nombre maximum de paires de findings suivies simultanément. Empêche la croissance mémoire non bornée                               |

```toml
[daemon.correlation]
enabled = true
window_minutes = 10
lag_threshold_ms = 2000
min_co_occurrences = 3
min_confidence = 0.5
```

Les corrélations sont exposées via `GET /api/correlations` (quand `api_enabled = true`) et émises en NDJSON sur le flux stdout du daemon.

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
# Optionnel : activer le TLS sur les listeners gRPC et HTTP.
# Les deux champs doivent être renseignés ensemble (ou les deux absents pour TCP en clair).
# tls_cert_path = "/etc/tls/server-cert.pem"
# tls_key_path = "/etc/tls/server-key.pem"
api_enabled = true
max_retained_findings = 10000

# Optionnel : corrélation cross-trace (mode daemon uniquement)
# [daemon.correlation]
# enabled = true
# window_minutes = 10
# lag_threshold_ms = 2000
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
