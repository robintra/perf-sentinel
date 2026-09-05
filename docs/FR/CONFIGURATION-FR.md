# Référence de configuration

perf-sentinel se configure via un fichier `.perf-sentinel.toml`. Tous les champs sont optionnels et ont des valeurs par défaut raisonnables.

<img alt="Vue d'ensemble des commandes CLI" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands.svg">

## Sommaire

- [Fragments de configuration](#fragments-de-configuration) : chargement multi-fichier déterministe.
- [Sous-commandes](#sous-commandes) : quelles sous-commandes lisent `.perf-sentinel.toml`.
- [Sections](#sections) : référence complète par section (`[thresholds]`, `[detection]`, `[green]`, `[daemon]`, `[reporting]`).
- [Configuration minimale](#configuration-minimale) : le `.perf-sentinel.toml` le plus court utile.
- [Exemple de configuration complète](#exemple-de-configuration-complète) : chaque section peuplée avec des valeurs d'exemple.
- [Migration depuis 0.5.x](#migration-depuis-05x) : les 8 clés top-level legacy retirées en 0.6.0 et comment migrer.
- [Variables d'environnement](#variables-denvironnement) : quelles variables d'environnement surchargent les valeurs du fichier de config.

## Fragments de configuration

perf-sentinel charge les documents TOML du répertoire `.perf-sentinel.d/`
placé à côté de la configuration principale, puis charge
`.perf-sentinel.toml` en dernier. Cette règle vaut aussi avec
`--config chemin/vers/custom.toml` : les fragments viennent de
`chemin/vers/.perf-sentinel.d/` et `custom.toml` reste la surcharge finale.
Le fichier principal est facultatif uniquement sans option `--config`.
Les valeurs par défaut sont utilisées uniquement si ni le fichier principal
implicite ni aucun fragment n'existe. Un fichier illisible ou une erreur de
syntaxe TOML individuelle arrête la commande avec le code 75. Après application
des surcharges, la configuration fusionnée doit aussi passer la désérialisation
typée et la validation, sinon la commande s'arrête.

Le nom d'un fragment doit suivre `NN-nom-minuscule.toml`, avec une priorité
unique `NN` comprise entre `00` et `99`. Les fichiers sont chargés par priorité
croissante. Une priorité dupliquée, une majuscule ou un séparateur ambigu est
refusé. Les fichiers qui ne se terminent pas par `.toml` sont ignorés.

Lorsque les deux valeurs sont des tables, elles fusionnent récursivement.
Toute autre valeur plus tardive remplace la précédente à la même clé. Le
document fusionné final doit toujours respecter le schéma de configuration
typé. Depuis la 0.12.0 ce schéma est strict : une clé ou un nom de table
qu'aucune section ne déclare fait échouer le chargement au lieu d'être
ignoré, donc un réglage mal orthographié arrête la commande au lieu de
laisser silencieusement la valeur par défaut en place. Les exemples réservent
ces plages :

| Priorité    | Usage                                                    |
|-------------|----------------------------------------------------------|
| `00` à `19` | valeurs partagées, seuils et détection                   |
| `20` à `39` | sources d'énergie et mesure GreenOps                     |
| `40` à `49` | sources d'intensité carbone                              |
| `50` à `69` | daemon et topologie de déploiement                       |
| `70` à `89` | reporting et politique propre à l'organisation           |
| `90` à `99` | surcharges locales, à garder de préférence hors du dépôt |

Les fragments prêts à copier dans `examples/` conservent leur priorité dans
leur nom. Gardez ces noms en les copiant dans `.perf-sentinel.d/` :
`30-green-alumet.toml`, `31-green-cloud.toml`,
`32-green-scaphandre.toml`, `33-green-kepler.toml`,
`34-green-redfish.toml` et `40-green-electricity-maps.toml`.
`60-daemon-docker.toml` est une configuration principale autonome pour les
topologies Compose collector et sharded. Montez-le comme `.perf-sentinel.toml`,
puis placez uniquement les fragments GreenOps optionnels dans le répertoire
frère `.perf-sentinel.d/`.

## Sous-commandes

| Sous-commande  | Description                                                                                                                                                                                                                     |
|----------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `analyze`      | Analyse batch de fichiers de traces. Lit depuis un fichier ou stdin                                                                                                                                                             |
| `explain`      | Vue arborescente d'une trace avec findings annotés en ligne                                                                                                                                                                     |
| `watch`        | Mode daemon : ingestion OTLP temps réel et détection en streaming                                                                                                                                                               |
| `query`        | Interroge un daemon en cours d'exécution. Sortie colorée par défaut, `--format json` pour le scripting. `query inspect` ouvre un TUI live                                                                                       |
| `demo`         | Lance l'analyse sur un jeu de données de démo embarqué                                                                                                                                                                          |
| `bench`        | Benchmark du débit sur un fichier de traces                                                                                                                                                                                     |
| `pg-stat`      | Analyse des exports `pg_stat_statements` (CSV/JSON ou Prometheus)                                                                                                                                                               |
| `inspect`      | TUI interactif pour naviguer les traces, findings et arbres de spans                                                                                                                                                            |
| `diff`         | Compare deux jeux de traces et émet un rapport delta (findings nouveaux/résolus, changements de sévérité, deltas I/O par endpoint). Sortie texte/JSON/SARIF                                                                     |
| `report`       | Dashboard HTML single-file pour l'exploration post-mortem dans un navigateur. Accepte un fichier de traces, un Report JSON pré-calculé, ou stdin via `--input -` (auto-détecte array-d'events vs objet Report, tolérant au BOM) |
| `tempo`        | Récupère des traces depuis une API HTTP Grafana Tempo (par ID de trace ou par recherche service puis fetch) et les pipe dans le pipeline d'analyse. Gaté derrière la feature `tempo`                                            |
| `jaeger-query` | Récupère des traces depuis n'importe quel backend qui parle l'API de requête Jaeger (Jaeger, Victoria Traces) et les pipe dans le pipeline d'analyse. Gaté derrière la feature `jaeger-query`                                   |
| `calibrate`    | Corrèle un fichier de traces avec des mesures d'énergie réelles (Scaphandre, CSV cloud monitoring) et émet un TOML de coefficients I/O-vers-énergie à charger via `[green] calibration_file`                                    |

## Sections

### `[thresholds]`

Seuils du quality gate. Le quality gate échoue si une règle est violée.

| Champ                              | Type     | Défaut | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
|------------------------------------|----------|--------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `n_plus_one_sql_critical_max`      | entier   | `0`    | Nombre maximum de findings N+1 SQL **critiques** avant l'échec du gate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `n_plus_one_http_warning_max`      | entier   | `3`    | Nombre maximum de findings N+1 HTTP **warning ou plus** avant l'échec du gate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `n_plus_one_messaging_warning_max` | entier   | `3`    | Nombre maximum de findings N+1 messaging **warning ou plus** avant l'échec du gate. Warning+ plutôt que critique seul, comme HTTP : un client Kafka peut déjà grouper les publications qu'il met en tampon, le compte d'occurrences y est donc un majorant                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `io_waste_ratio_max`               | flottant | `0.30` | Ratio maximum de gaspillage I/O (0.0 à 1.0) avant l'échec du gate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `min_usable_span_ratio`            | flottant | absent | Part minimale (0.0 à 1.0) de spans de forme I/O qui doivent être analysables (spans SQL portant `db.statement`, spans HTTP CLIENT portant une URL complète) avant l'échec du gate, calculée par nature d'I/O et rapportée comme la pire des deux, pour qu'un trafic HTTP sain ne masque pas une surface SQL cassée. Une nature d'I/O portant moins de 20 spans n'est pas jugée, échantillon trop petit pour un ratio qui bloque un build ; quand aucune nature ne franchit ce plancher la règle est ignorée et le rapport porte un avertissement `tuning` qui le dit. Runs batch uniquement (`analyze`, `report`), le daemon n'exporte pas de décompte. Garde-fou contre un faux vert dû à une instrumentation inexploitable : sous le seuil, le gate échoue même avec zéro finding. Absent = règle désactivée. S'applique à l'entrée OTLP seulement, qui porte le décompte par raison (exposé dans `analysis.ingest` du rapport JSON) |

### `[detection]`

Paramètres des algorithmes de détection.

| Champ                                  | Type   | Défaut                                        | Description                                                                                                                                                                                                                                                                                                                                                                               |
|----------------------------------------|--------|-----------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `n_plus_one_min_occurrences`           | entier | `5`                                           | Nombre minimum d'occurrences (avec des paramètres distincts) pour signaler un pattern N+1                                                                                                                                                                                                                                                                                                 |
| `window_duration_ms`                   | entier | `500`                                         | Fenêtre temporelle en millisecondes dans laquelle les opérations répétées sont considérées comme un pattern N+1                                                                                                                                                                                                                                                                           |
| `slow_query_threshold_ms`              | entier | `500`                                         | Seuil de durée en millisecondes au-dessus duquel une opération est considérée comme lente                                                                                                                                                                                                                                                                                                 |
| `slow_query_min_occurrences`           | entier | `3`                                           | Nombre minimum d'occurrences lentes du même template pour générer un finding                                                                                                                                                                                                                                                                                                              |
| `max_fanout`                           | entier | `20`                                          | Nombre maximum de spans enfants par parent avant de signaler un fanout excessif (plage : 1-100000)                                                                                                                                                                                                                                                                                        |
| `chatty_service_min_calls`             | entier | `15`                                          | Nombre minimum d'appels HTTP sortants par trace pour signaler un service bavard. Severite : warning > seuil, critical > 3x seuil.                                                                                                                                                                                                                                                         |
| `pool_saturation_concurrent_threshold` | entier | `10`                                          | Nombre maximal de spans SQL concurrents par service pour signaler un risque de saturation du pool de connexions. Utilise un algorithme de balayage sur les timestamps des spans.                                                                                                                                                                                                          |
| `serialized_min_sequential`            | entier | `3`                                           | Nombre minimum d'appels séquentiels indépendants (même parent, sans chevauchement, templates différents) pour signaler des appels potentiellement parallélisables.                                                                                                                                                                                                                        |
| `grouping_attributes`                  | liste  | `["k8s.namespace.name", "service.namespace"]` | Attributs de ressource ou de span qui séparent un déploiement d'un autre, du plus spécifique au moins. Le premier présent sur un span décide de l'identité du finding, le même problème dans deux namespaces reste donc deux findings. Chaque attribut présent est capturé et affiché. 8 entrées max, une liste vide désactive le regroupement. Les signatures d'acquittement l'ignorent. Depuis 0.19.0 la valeur effective est aussi le label `grouping` des métriques par service du daemon, voir `[daemon] per_grouping_labels`. |
| `sanitizer_aware_classification`       | chaîne | `"auto"`                                      | Classification des groupes SQL dont les littéraux ont été remplacés par un placeholder (`?`, `$?`, `%s`, `@param`, `:name`) par un agent OTel ou un driver de base de données. Une valeur parmi `"auto"`, `"strict"`, `"always"`, `"never"`. Voir la note ci-dessous.                                                                                                                     |
| `sanitizer_aware_min_cv`               | nombre | `0.5`                                         | Coefficient de variation (écart-type sur moyenne) des durées par span au-dessus duquel l'heuristique sanitizer lit un groupe comme un N+1 plutôt qu'une répétition en cache. Plage `(0, 10]`. Voir la note ci-dessous.                                                                                                                                                                     |

#### `sanitizer_aware_classification`

Les agents OpenTelemetry et les drivers de base de données activent par
défaut la sanitization des instructions SQL pour éviter de laisser
fuir des PII dans les attributs de trace. Le style de placeholder
dépend de la stack : les agents JDBC produisent `?`, les drivers
PostgreSQL natifs (pgx, asyncpg, sqlx) produisent `$1`/`$2`
(normalisés en `$?`), les drivers Python DB-API produisent `%s`, les
drivers .NET produisent `@p0`/`@Name`, et Oracle/SQLAlchemy
produisent `:name`. Dans tous les cas, les spans arrivent dans
perf-sentinel avec le même template et aucun paramètre extractible. La
règle standard de paramètres distincts rejette donc le groupe et le
détecteur de redondance le récupère sous l'étiquette `redundant_sql` au
lieu de `n_plus_one_sql`. Ce paramètre contrôle l'heuristique qui
restaure la classification correcte :

- `"auto"` (défaut) : émet `n_plus_one_sql` quand **soit** un marqueur
  ORM est présent dans les `instrumentation_scopes` des spans (Spring
  Data, Hibernate, EF Core, SQLAlchemy, ActiveRecord, GORM, Prisma,
  Diesel, Laravel/Eloquent, Doctrine, ...) **soit** la variance des durées par span est suffisante
  pour indiquer des accès à des lignes distinctes. Sinon, le groupe
  reste à la charge du détecteur de redondance. Meilleur rappel sur les
  stacks production Spring Data, EF Core et similaires.
- `"strict"` : reclassifie uniquement quand un signal primaire
  (marqueur ORM, nombre d'occurrences >= 3 x
  `n_plus_one_min_occurrences`, ou siblings séquentiels) se déclenche
  conjointement avec un signal corroboratif (variance temporelle
  élevée ou nombre d'occurrences élevé). Préserve la précision de
  `redundant_sql` sur les requêtes identiques de compte modéré (boucles
  de polling legacy, lookups de config non mémoïsés, typiquement 5-10
  appels par requête). Au-dessus de la barre (par défaut 15), tout
  groupe sanitisé se déclenche quel que soit le scope ORM, les siblings
  séquentiels ou la variance, sous la garde `looks_sanitized`. À
  utiliser quand les findings `redundant_sql` sont un signal
  exploitable qui ne doit pas être absorbé silencieusement par
  `n_plus_one_sql`. Le laboratoire de simulation fait tourner toutes ses
  stacks ainsi, parce que sous `auto` un marqueur de scope ORM suffit à
  reclassifier en N+1 une répétition de la même requête servie par un
  cache. Ce changement de verdict ne vaut que pour les comptes modérés,
  la barre de haute occurrence ci-dessus se déclenchant aussi sous
  `strict`.
- `"always"` : reclassifie tout groupe sanitisé qui atteint
  `n_plus_one_min_occurrences` spans en `n_plus_one_sql`. Plus agressif,
  peut requalifier une vraie redondance à un seul paramètre.
- `"never"` : désactive complètement l'heuristique et retombe sur le
  check strict `distinct_params`.

Les findings reclassifiés par l'heuristique (sous `"auto"`, `"strict"`
ou `"always"`) portent `classification_method = "sanitizer_heuristic"`
dans leur représentation JSON, ce qui permet à un opérateur de repérer
où elle se déclenche. Les findings produits par la règle standard
omettent ce champ.

#### `sanitizer_aware_min_cv`

Le signal de variance temporelle derrière les modes ci-dessus compare le
coefficient de variation des durées par span du groupe à ce seuil. Des
lookups sur des clés différentes étalent leurs durées entre hits et
miss de cache, les répétitions d'une même requête en cache se
regroupent. Le défaut de `0.5` préfère signaler un N+1 plutôt qu'en
manquer un, puisqu'une erreur ne fait qu'échanger `redundant_sql` contre
`n_plus_one_sql` au même poids d'I/O évitables.

À relever sur un runtime dont la gigue d'ordonnancement étale même les
répétitions en cache : workers PHP-FPM, conteneurs bridés en CPU,
runners CI partagés. Le laboratoire de simulation a mesuré un CV
d'environ 0,75 sur dix lookups Doctrine identiques servis depuis le
cache sous charge, assez pour franchir le défaut et transformer un
finding `redundant_sql` en `n_plus_one_sql`, avec une piste de
remédiation (`leftJoin`, `with()`) qui ne s'applique pas à une
répétition. À `1.0` le groupe garde son verdict `redundant_sql`.

Le même seuil alimente l'heuristique HTTP, qui décide si un appel sortant
répété avec peu de paramètres distincts se lit `n_plus_one_http` ou
`redundant_http`, si bien que le relever déplace les deux verdicts. Ce
qui continue de signaler un vrai N+1 quelle que soit la variance dépend
du mode et du chemin :

- SQL sous `"auto"` : le marqueur de scope ORM à lui seul, donc relever
  le seuil ne change rien sur un groupe instrumenté par un ORM.
- SQL sous `"strict"` : la barre de haute occurrence
  (`3 x n_plus_one_min_occurrences`).
- HTTP : la règle directe, puisque des paramètres de chemin ou de
  requête distincts classent le groupe avant que l'heuristique ne tourne.
- `"always"` ignore la variance entièrement, `"never"` ne la consulte
  jamais.

Une seule valeur sert toute la configuration : un daemon devant plusieurs
runtimes prend le seuil du plus bruyant et accepte la perte sur les
groupes de compte modéré ailleurs.

La valeur est consignée dans le `detection_config` de chaque rapport. Un
rapport écrit avant l'existence de la clé se relit avec `0.5`, la valeur
que son run avait en dur.

### `[green]`

> **Voir aussi.** L'[introduction énergie et SCI](METHODOLOGY-FR.md#introduction-énergie-et-sci-v10) dans la doc méthodologie définit SCI v1.0 (termes E + I + M), RAPL, Scaphandre, SPECpower, Boavizta et l'API Electricity Maps utilisés par les sections de config ci-dessous. À lire une fois si l'un des termes ne vous est pas familier.

Configuration du scoring GreenOps alignée sur [SCI v1.0](https://github.com/Green-Software-Foundation/sci) (termes opérationnel + embodié, intervalles de confiance, multi-région).

| Champ                              | Type     | Défaut    | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
|------------------------------------|----------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`                          | booléen  | `true`    | Active le scoring GreenOps (IIS, ratio de gaspillage, top offenders, CO₂)                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `default_region`                   | chaîne   | *(aucun)* | Région cloud de fallback utilisée quand ni l'attribut `cloud.region` du span ni le mapping `service_regions` ne résout une région. Exemples : `"eu-west-3"`, `"us-east-1"`, `"FR"`                                                                                                                                                                                                                                                                                                                                                |
| `embodied_carbon_per_request_gco2` | flottant | `0.001`   | Terme `M` SCI v1.0 : émissions de fabrication matérielle amorties par requête (par trace), en gCO₂eq. Indépendant de la région. Un zéro est déprécié depuis la 0.9.25 et retombe sur ce défaut avec un avertissement : aucun matériel n'a un carbone incorporé nul, et un coefficient à zéro effaçait le terme M de la divulgation. La valeur appliquée est publiée dans `calibration_inputs`                                                                                                                                     |
| `use_hourly_profiles`              | booléen  | `true`    | Quand `true`, l'étape de scoring utilise des intensités réseau spécifiques à l'heure pour les 30+ régions disposant de profils horaires embarqués. Les régions avec profils mois x heure (FR, DE, GB, US-East) prennent aussi en compte la variation saisonnière. Les rapports sont tagués `model = "io_proxy_v3"` (mois x heure) ou `"io_proxy_v2"` (horaire annuel plat). Mettre à `false` pour figer les rapports sur le modèle annuel plat                                                                                    |
| `hourly_profiles_file`             | chaîne   | *(aucun)* | Chemin vers un fichier JSON de profils horaires personnalisés. Peut être absolu ou relatif au fichier de config. Les profils personnalisés prennent priorité sur les profils embarqués pour la même clé de région                                                                                                                                                                                                                                                                                                                 |
| `per_operation_coefficients`       | booléen  | `true`    | Quand `true`, le modèle proxy pondère l'énergie par opération : SQL SELECT (0.5x), INSERT/UPDATE (1.5x), DELETE (1.2x) et tailles de payload HTTP (petit <10 Ko : 0.8x, moyen 10 Ko-1 Mo : 1.2x, grand >1 Mo : 2.0x). Ne s'applique pas quand l'énergie mesurée par Scaphandre ou cloud SPECpower est disponible. Mettre à `false` pour utiliser le coefficient plat `ENERGY_PER_IO_OP_KWH`                                                                                                                                       |
| `include_network_transport`        | booléen  | ignoré    | Dépréciée et ignorée depuis la 0.9.25 : le terme de transport est toujours calculé, toujours affiché et toujours publié, un interrupteur d'affichage sur un chiffre publié n'avait plus de justification. La clé se parse encore, avec un avertissement. Le terme requiert `response_size_bytes` sur les spans HTTP (attribut OTel `http.response.body.size`) et la région cible mappée via `[green.service_regions]`. Les appels intra-région sont exclus. Le CO₂ transport apparaît comme `transport_gco2` dans le rapport JSON |
| `network_energy_per_byte_kwh`      | flottant | ignoré    | Dépréciée et ignorée depuis la 0.9.25. Le coefficient est fixé à 0.04 kWh/Go pour que chaque divulgation mette le transport à la même échelle, et la divulgation publie la fourchette sourcée 0.001-0.059 kWh/Go à côté (voir LIMITATIONS-FR.md, transport réseau). La clé se parse encore, avec un avertissement                                                                                                                                                                                                                 |

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

- **`co2`** : objet structuré `{ total, avoidable, operational_gco2, embodied_gco2 }`. `total` et `avoidable` sont tous deux `{ low, mid, high, model, methodology }` avec une **incertitude multiplicative 2×** (`low = mid/2`, `high = mid×2`). Le tag `methodology` distingue `total` (`"sci_v1_numerator+transport"` : `(E × I) + M + T` sommé sur les traces, y compris quand `T` vaut zéro) de `avoidable` (`"sci_v1_operational_ratio"` : ratio global aveugle à la région, exclut l'embodié et le transport). Les rapports historiques peuvent porter `"sci_v1_numerator"`. Valeurs `model`, le plus précis gagne : `"electricity_maps_api"` → `"scaphandre_rapl"` → `"kepler_ebpf"` → `"redfish_bmc"` → `"cloud_specpower"` → `"io_proxy_v3"` → `"io_proxy_v2"` → `"io_proxy_v1"`. Quand des facteurs de calibration sont actifs sur les modèles proxy, `+cal` est ajouté (ex. `"io_proxy_v2+cal"`). Le suffixe `+cal` ne s'applique jamais à un tag mesuré.
- **`regions[]`** : breakdown par région avec `{ region, grid_intensity_gco2_kwh, pue, io_ops, co2_gco2, intensity_source }`, **trié par `co2_gco2` décroissant** (régions à plus fort impact en premier) avec tiebreak alphabétique. `intensity_source` vaut `"annual"`, `"hourly"`, `"monthly_hourly"` ou `"real_time"` (API Electricity Maps) selon quelle source d'intensité carbone a été utilisée pour la région.

Les données d'intensité carbone sont embarquées dans le binaire (aucun appel réseau sortant). Voir `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md` pour la formule complète et la méthodologie et [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#précision-des-estimations-carbone) pour le disclaimer directionnel / non-réglementaire.

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
- `"westeurope"`, `"nl"`, `"nl-ams"` → `eu-west-4` (Pays-Bas)
- `"northeurope"`, `"ie"` → `eu-west-1` (Irlande)
- `"uksouth"`, `"gb"`, `"uk"`, `"uk1"` → `eu-west-2` (Royaume-Uni)
- `"westus2"` → `us-west-2` (Oregon)
- `"gra11"`, `"gra"`, `"sbg"`, `"fr-par"`, `"outscale-eu-west-2"` → `eu-west-3` (France)
- `"waw1"`, `"pl-waw"` → `europe-central2` (Pologne)
- `"bhs5"`, `"bhs"` → `ca-central-1` (Québec)

**Clés OVHcloud, Scaleway et OUTSCALE.** OVHcloud nomme un même datacenter de trois façons selon l'API interrogée (`GRA11` pour la région OpenStack Public Cloud, `GRA` pour le code de zone, `gra` pour la chaîne de localisation S3), et les trois sont référencées. Les clés OUTSCALE portent un préfixe `outscale-` parce qu'OUTSCALE réutilise les identifiants de région d'AWS pour d'autres lieux : son `eu-west-2` est Paris là où celui d'AWS est Londres. Un déploiement OUTSCALE déclare donc `default_region = "outscale-eu-west-2"` et non l'identifiant nu, sinon il est scoré sur le réseau britannique.

La table complète des alias se trouve dans `score/carbon_profiles.rs`. Si votre clé de région n'est pas aliasée, la valeur annuelle plate de la table carbone principale est utilisée.

**Tous les backends d'énergie et d'intensité réseau sont réservés au daemon.** `[green.alumet]`, `[green.scaphandre]`, `[green.kepler]`, `[green.redfish]`, `[green.cloud]`, `[green.broker_static]` et `[green.electricity_maps]` sont interrogés par le daemon `watch` et par rien d'autre. Un run batch `analyze` ou `report` ne démarre aucun scraper : il calcule donc avec l'estimation proxy I/O sur les données d'intensité embarquées quoi que disent ces sections, et il émet un avertissement nommant les sections qu'il a dû ignorer. Attribuer une puissance mesurée maintenant à des traces enregistrées plus tôt produirait un chiffre faux plutôt qu'un chiffre absent, d'où l'absence de scraping en batch.

#### `[green.scaphandre]` (optionnel, opt-in)

Intégration opt-in avec [Scaphandre](https://github.com/hubblo-org/scaphandre) pour la mesure énergétique par processus sur les hôtes Linux avec support Intel RAPL. Quand cette section est configurée, le daemon `watch` lance une tâche de fond qui scrape l'endpoint Prometheus de Scaphandre toutes les `scrape_interval_secs` secondes et utilise les lectures de puissance mesurées pour remplacer la constante `ENERGY_PER_IO_OP_KWH` fixe pour chaque service mappé.

**Préférez `[green.alumet]` pour les nouveaux déploiements.** Les deux intégrations lisent les mêmes compteurs RAPL, mais l'échantillonnage d'Alumet est mesurablement moins erroné, comme le caractérisent ses propres auteurs dans [Dissecting the software-based measurement of CPU energy consumption](https://hal.science/hal-04420527v2/document) (Raffin et al.), et il attribue par cgroup plutôt que par processus. Le support Scaphandre est conservé pour les déploiements existants, et `alumet_rapl` surclasse `scaphandre_rapl` dès que les deux alimentent le même service. Voir [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-scaphandre).

| Champ                  | Type   | Défaut    | Description                                                                                                                                                                           |
|------------------------|--------|-----------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | chaîne | *(aucun)* | URL complète de l'endpoint Prometheus `/metrics` de Scaphandre. Doit commencer par `http://` ou `https://` (TLS supporté via hyper-rustls). Obligatoire quand la section est présente |
| `scrape_interval_secs` | entier | `5`       | Fréquence de scrape, en secondes. Plage valide : 1-3600                                                                                                                               |
| `process_map`          | table  | `{}`      | Mappe les noms de service perf-sentinel (depuis `service.name` du span) à un `ProcessMatcher` par service (voir ci-dessous)                                                           |

Chaque entrée `process_map` est une table avec deux champs : `exe_contains` (obligatoire, sous-chaîne comparée au label `exe` de Scaphandre) et `cmdline_contains` (optionnel, sous-chaîne comparée au label `cmdline`). Quand `cmdline_contains` est défini, le matcher exige que les deux sous-chaînes soient présentes. Un seul processus Scaphandre doit matcher par entrée, sinon l'étape de scoring saute ce service pour le tick et émet un log `warn` nommant l'ambiguïté.

```toml
[green.scaphandre]
endpoint = "http://localhost:8080/metrics"
scrape_interval_secs = 5

[green.scaphandre.process_map."order-svc"]
exe_contains = "bin/java"
cmdline_contains = "order-svc.jar"

[green.scaphandre.process_map."chat-svc"]
exe_contains = "bin/java"
cmdline_contains = "chat-svc.jar"

[green.scaphandre.process_map."native-svc"]
exe_contains = "/opt/native-svc/bin/native-svc"
```

**Pourquoi `exe_contains` ET `cmdline_contains`.** Scaphandre émet `exe` comme chemin absolu du runtime (`/usr/lib/jvm/.../bin/java`, `/usr/share/dotnet/dotnet`). Plusieurs services co-localisés partageant un runtime (plusieurs JVMs, plusieurs assemblies .NET) collisionnent sur `exe`, et seul `cmdline` les distingue. Scaphandre concatène en plus argv sans séparateurs : `java -jar /tmp/order-svc.jar` apparaît comme `cmdline="java-jar/tmp/order-svc.jar"`. Configurez `cmdline_contains` avec une sous-chaîne présente dans cette forme concaténée (par exemple le nom du jar ou de la dll), PAS avec une ligne de commande POSIX contenant des espaces.

**Ignoré en mode batch `analyze`.** Seul le daemon `watch` lance le scraper. La commande `analyze` utilise toujours le modèle proxy quelle que soit cette section.

**Comportement de fallback.** Quand l'endpoint est inaccessible, qu'un service n'est pas présent dans `process_map` ou qu'un service a eu zéro ops dans la fenêtre de scrape courante, l'étape de scoring retombe sur le modèle proxy pour ces spans. Le premier échec est logué en niveau `warn` ; les échecs suivants en `debug` pour éviter le spam. La jauge Prometheus `perf_sentinel_scaphandre_last_scrape_age_seconds` permet aux opérateurs de détecter un scraper bloqué.

**Limites de précision (important).** Scaphandre améliore le coefficient énergétique **au niveau service** mais ne donne PAS d'attribution par finding. RAPL est au niveau processus, pas au niveau span : deux findings dans le même processus pendant la même fenêtre de scrape partagent le même coefficient. Voir [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-scaphandre) pour la discussion complète.

#### `[green.kepler]` (optionnel, opt-in)

Intégration opt-in avec [Kepler](https://github.com/sustainable-computing-io/kepler) (projet CNCF sandbox) pour la mesure d'énergie par conteneur ou par processus via eBPF. Contrairement à Scaphandre, Kepler fonctionne sur ARM64 (Graviton, Ampere, Apple Silicon, Cobalt 100) avec une précision dégradée mais un signal réel. Une fois configuré, le daemon `watch` scrape l'endpoint Prometheus `/metrics` de Kepler, calcule le delta de joules par service par rapport au scrape précédent, et publie un coefficient mesuré par opération taggué `kepler_ebpf`.

| Champ                  | Type   | Défaut        | Description                                                                                                                                                                      |
|------------------------|--------|---------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | chaîne | *(aucun)*     | URL complète de l'endpoint Prometheus `/metrics` de Kepler. Obligatoire quand la section est présente.                                                                           |
| `scrape_interval_secs` | entier | `5`           | Fréquence de scrape en secondes. Plage valide : 1-3600.                                                                                                                          |
| `metric_kind`          | chaîne | `"container"` | Compteur Kepler v2 à lire : `"container"` (`kepler_container_cpu_joules_total`, clé `container_name`) ou `"process"` (`kepler_process_cpu_joules_total`, clé `comm`).            |
| `service_mappings`     | table  | `{}`          | Mappe les noms de service perf-sentinel vers la valeur du label Kepler identifiant la même charge (nom de conteneur pour `container`, nom de commande processus pour `process`). |
| `auth_header`          | chaîne | *(aucun)*     | Header `"Nom: Valeur"` optionnel. Préférer la variable d'environnement `PERF_SENTINEL_KEPLER_AUTH_HEADER`.                                                                       |

```toml
[green.kepler]
endpoint = "http://kepler.kube-system.svc.cluster.local:9102/metrics"
scrape_interval_secs = 5
metric_kind = "container"

[green.kepler.service_mappings]
"order-svc" = "order-svc-deployment"
"chat-svc" = "chat"
```

**Ignoré en mode batch `analyze`.** Comme Scaphandre, seul `watch` lance le scraper.

**Les compteurs partageant une valeur de label sont sommés.** Un même nom de conteneur répété entre pods (ou un même `comm` partagé par plusieurs processus) produit plusieurs séries cumulatives sous une même valeur de mapping. Leurs compteurs sont sommés avant le calcul du delta par fenêtre, le coefficient les couvre donc ensemble.

**Précédence par rapport à Scaphandre.** Scaphandre RAPL surclasse Kepler eBPF sur x86_64 avec accès RAPL. L'intégration Kepler prend tout son sens sur ARM64 où Scaphandre est indisponible. Voir [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-kepler) pour les mises en garde sur la précision du modèle eBPF ARM (issue amont Kepler #1556).

**Forme de déploiement en production.** Kepler s'exécute en général comme `DaemonSet` Kubernetes, un pod par nœud. Le scraper actuel effectue un GET direct et la réponse doit exposer les séries Kepler elles-mêmes. L'endpoint `/metrics` d'un serveur Prometheus expose les métriques internes de Prometheus, pas les séries qu'il a scrapées. Pour un cluster multi-nœuds, exécutez un perf-sentinel par nœud ou fournissez un endpoint de fédération/proxy qui expose directement les séries Kepler agrégées au format d'exposition Prometheus. Le mode de requête PromQL natif est réservé à une version ultérieure.

#### `[green.alumet]` (optionnel, opt-in)

Intégration opt-in avec [Alumet](https://github.com/alumet-dev/alumet) (INRIA/LIG, EUPL-1.2) pour l'énergie mesurée. Alumet est un framework de mesure modulaire : un plugin source (`rapl`, `nvidia-nvml`, ...) produit des relevés, des plugins de transformation optionnels les attribuent aux charges de travail, et un plugin de sortie les expose. perf-sentinel scrape la sortie `prometheus-exporter`. Une fois configuré, le démon `watch` publie un coefficient mesuré par opération étiqueté `alumet_rapl`, qui **surclasse toutes les autres sources mesurées**, Scaphandre compris.

| Champ                  | Type   | Défaut    | Description                                                                                                                                                     |
|------------------------|--------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoint`             | string | *(aucun)* | URL complète de l'endpoint `/metrics` du `prometheus-exporter` d'Alumet (port amont par défaut 9091). Requis si la section est présente                         |
| `scrape_interval_secs` | int    | `5`       | Fréquence de scrape, en secondes. Plage valide : 1-3600                                                                                                         |
| `metric_name`          | string | *(aucun)* | Nom de la métrique Prometheus **exactement tel qu'il apparaît sur le fil**, préfixe et suffixe de l'exporteur inclus. Requis, sans défaut (voir plus bas)       |
| `label_key`            | string | *(aucun)* | Label Prometheus portant l'identité de la charge de travail. Requis, sans défaut. `name` pour la source `k8s` (nom du pod), `domain` pour une série RAPL brute  |
| `energy_interval_secs` | float  | `1.0`     | Durée en secondes que couvre la valeur en joules scrapée. **Doit correspondre au `poll_interval` de la source Alumet** qui alimente la métrique (voir plus bas) |
| `service_mappings`     | table  | `{}`      | Associe les noms de service perf-sentinel à la valeur de label Alumet identifiant la même charge de travail                                                     |
| `auth_header`          | string | *(aucun)* | En-tête `"Nom: Valeur"` optionnel. Préférer la variable d'environnement `PERF_SENTINEL_ALUMET_AUTH_HEADER`                                                      |

**Pourquoi `metric_name` et `label_key` n'ont pas de défaut.** L'exporteur d'Alumet ajoute un `prefix` en tête et un `suffix` en fin (par défaut `_alumet`) à chaque nom de métrique, et la série par service est produite par une formule `energy-attribution` dont *vous* choisissez le nom. Aucun défaut ne serait correct pour tous les déploiements, et une mauvaise supposition ne scraperait rien. Lisez les noms sur votre propre endpoint :

```bash
curl -s http://localhost:9091/metrics | grep -i energy
```

**Plusieurs lignes par service sont sommées.** Le `label_key` d'Alumet est couramment partagé : un pod porte une ligne par domaine RAPL (`package` + `dram`), et `label_key = "domain"` sur un hôte bi-socket porte une ligne `domain="package"` par socket. Toutes les lignes partageant une valeur de label sont sommées, ce qui est la lecture physiquement correcte puisque l'énergie est additive (les lignes NaN ou négatives sont ignorées). Deux conséquences à configurer. Choisissez `label_key` de sorte que les lignes partageant une valeur soient bien celles que vous voulez additionner. Et assurez-vous que ces lignes ne se recouvrent pas : les domaines RAPL s'emboîtent (`psys` contient `package`, `package` contient `pp0`/`pp1`), donc une formule qui émet un domaine parent et son enfant pour un même label compte deux fois la part partagée. `package` plus `dram` se somme correctement, `psys` plus `package` non.

**Pourquoi `energy_interval_secs` existe.** C'est le champ à ne pas rater. L'exporteur d'Alumet publie chaque mesure comme une **jauge Prometheus contenant la dernière valeur flushée**, et `rapl_consumed_energy` est un `CounterDiff` : les joules brûlés pendant un `poll_interval` de la source, ni un compteur cumulatif ni une puissance. perf-sentinel divise par cet intervalle pour retrouver des watts. L'intervalle n'apparaît nulle part sur le fil, il doit donc être déclaré ici et correspondre à la configuration côté Alumet. **Un écart met l'énergie et le carbone à une échelle linéairement fausse, en silence** : déclarer `1.0` alors qu'Alumet échantillonne à `5s` surestime l'énergie d'un facteur 5, sans aucun avertissement. Voir [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-alumet). Le démon affiche la valeur utilisée dans la ligne de log `Alumet scraper started`.

**Configuration Alumet correspondante.** L'attribution par service demande trois plugins Alumet qui travaillent ensemble, `rapl` seul ne mesure que la machine entière et `procfs` n'identifie les processus que par PID :

```toml
# alumet-config.toml
[plugins.rapl]
poll_interval = "1s"          # <- energy_interval_secs de perf-sentinel doit valoir ceci

[plugins.k8s]
# découverte des pods, fournit les attributs `name` / `namespace`

[plugins.energy-attribution.formulas.attributed_energy_cpu]
expr = "cpu_energy * cpu_usage / 100.0"
ref = "cpu_energy"

[plugins.energy-attribution.formulas.attributed_energy_cpu.per_resource]
cpu_energy = { metric = "rapl_consumed_energy", resource_kind = "local_machine", domain = "package_total" }

[plugins.energy-attribution.formulas.attributed_energy_cpu.per_consumer]
cpu_usage = { metric = "cpu_percent", kind = "total" }

[plugins.prometheus-exporter]
port = 9091
suffix = "_alumet"            # <- d'où le nom de métrique en _alumet ci-dessous
```

Le côté perf-sentinel correspondant :

```toml
[green.alumet]
endpoint = "http://localhost:9091/metrics"
scrape_interval_secs = 5
metric_name = "attributed_energy_cpu_alumet"
label_key = "name"
energy_interval_secs = 1.0

[green.alumet.service_mappings]
"order-svc" = "order-svc-pod"
"chat-svc" = "chat-svc-pod"
```

**Ignoré en mode batch `analyze`.** Comme tous les backends d'énergie mesurée, seul `watch` lance le scraper.

**Précédence.** `alumet_rapl` est en tête de la chaîne mesurée, devant `scaphandre_rapl`. Les deux lisent RAPL, mais l'échantillonnage d'Alumet est mesurablement moins erroné et il attribue par cgroup plutôt que par processus. Faire tourner les deux sur le même service est supporté, Alumet gagne.

**Pièges du packaging amont.** Le `.deb` amont installe `/etc/alumet/alumet-config.toml` (avec des sections csv, procfs, perf, ...) et son wrapper `alumet-agent` pointe `ALUMET_CONFIG` dessus si la variable n'est pas déjà définie. Activer prometheus-exporter via `--plugins` fonctionne même si ce fichier n'a pas de section `prometheus-exporter`. L'agent remplit la section absente à partir des défauts du plugin (`prefix ""`, `suffix "_alumet"`, port 9091). Pour une config contrôlée plutôt que celle livrée, pointez `ALUMET_CONFIG` vers un chemin vierge pour que l'agent régénère les défauts de votre jeu de plugins, ou lancez `config regen`. En conteneur, le binaire packagé porte des file capabilities (`cap_perfmon`, `cap_sys_nice`, `cap_sys_ptrace`) : un `docker run` nu échoue en EPERM sans les `--cap-add` correspondants.

**Le scraper seul ne met pas `alumet_rapl` dans le rapport.** Déclarer `[green.alumet]` démarre le scraper (visible sur `/api/energy`), mais le coefficient mesuré n'atteint `green_summary` que quand le scoring green résout une région pour les spans (`[green] default_region`, `[green.service_regions]`, ou un attribut de span `cloud.region`). Sans cela, `per_service_energy_model` continue d'afficher le tag proxy, ce qui se lit facilement comme une intégration Alumet cassée.

**Alumet est pré-1.0** (v0.9.5 au moment de l'écriture). Les noms de métriques et la configuration des plugins peuvent changer d'une version à l'autre. Si un scrape cesse de correspondre après une montée de version d'Alumet, le démon avertit avec `no samples matched the configured metric` après trois ticks consécutifs.

##### `[green.alumet.database]` (optionnel)

Déclare une charge de base de données mesurée par Alumet. Une base n'émet pas de spans, elle ne peut donc jamais apparaître dans `service_mappings` (zéro op, le chemin des coefficients par op l'ignore). À la place, son énergie sur chaque fenêtre de scoring est multipliée par le ratio de gaspillage SQL (`avoidable_sql_io_ops / total_sql_io_ops`) et publiée dans `green_summary.database_waste`, un chiffre autonome exclu de `energy_kwh`, de `co2` et de la divulgation publique. Voir `docs/FR/METHODOLOGY-FR.md` pour la formule et [Limites de précision Alumet](LIMITATIONS-FR.md#limites-de-précision-alumet) pour la raison d'une borne basse.

```toml
[green.alumet.database]
label_value = "postgres-pod"   # valeur portée par label_key pour le cgroup de la base, texto
region = "eu-west-3"           # optionnel, active la conversion gCO2 (déclarée, pas inférée)
```

`label_value` est obligatoire, apparié exactement comme une valeur de `service_mappings`. `region` est optionnel et utilise les mêmes identifiants de région que `[green.service_regions]` : sans lui le gaspillage est publié en kWh seulement. Une base par configuration, déclarez le cgroup qui sert votre trafic SQL.

##### `[green.alumet.broker]` (optionnel)

Le jumeau messaging de la section ci-dessus. Déclare un broker de messages mesuré par Alumet : un broker n'émet pas de spans propres non plus, donc son énergie de fenêtre est multipliée par le ratio de gaspillage messaging (`avoidable_messaging_io_ops / total_messaging_io_ops`) et rapportée en `green_summary.messaging_waste`.

```toml
[green.alumet.broker]
label_value = "kafka-pod"      # valeur portée par label_key pour le cgroup du broker, telle quelle
region = "eu-west-3"           # optionnel, active la conversion gCO2
```

Un même cgroup ne peut pas alimenter deux figures : un `label_value` qui apparaît aussi dans `service_mappings`, ou qui correspond à la déclaration de base de données, est rejeté au chargement de la config. Exige un agent sur l'hôte du broker, donc inapplicable à un broker managé. Pour ceux-là, voir `[green.broker_static]`.

#### `[green.broker_static]` (optionnel, opt-in)

Déclare un cluster de brokers **provisionné**, sans agent ni métrique. C'est la seule voie qui fonctionne pour un broker managé (Confluent Cloud, MSK, SQS, Pulsar managé), où aucun hôte n'est instrumentable.

```toml
[green.broker_static]
nodes = 3                      # nœuds de broker provisionnés, requis
instance_type = "m5.2xlarge"   # cherché dans la table SPECpower embarquée, requis
provider = "aws"               # optionnel : aws, gcp, azure, scaleway ou generic (défaut)
region = "eu-west-3"           # optionnel, active la conversion gCO2
```

L'énergie vaut `nodes × max_watts × durée de la fenêtre`, suivant `E(n) = n × P_max` : les nœuds provisionnés multipliés par leur plafond de puissance. Trois propriétés à accepter avant de s'appuyer dessus :

- **Le chiffre borne le calcul, pas la consommation murale.** `max_watts` est la puissance à 100 % de CPU, tirée d'une table SPECpower qui couvre le CPU et la carte mère et exclut le stockage, le réseau et les pertes d'alimentation. Or ce sont eux qui dominent sur un broker : c'est donc un plafond sur les vCPU déclarés, pas sur ce que le cluster tire réellement de la prise. Un nœud Kafka limité par le stockage peut consommer plus que ce que ce chiffre rapporte.
- **Il compte l'infrastructure provisionnée, pas consommée.** Un cluster de trois nœuds est immobilisé qu'il tourne à 10 % ou à 60 %. Dans l'autre sens, une période sans trafic facture au plus une heure, donc un cluster majoritairement inactif est sous-compté.
- **Le chiffre ne réagit à aucun changement applicatif.** Grouper vos publications ne le fera pas bouger, puisque rien dedans ne dépend du trafic. Si vous voulez un chiffre qui répond à une remédiation, il faut la voie mesurée.

Un `instance_type` inconnu émet un avertissement et retombe sur un défaut fournisseur plutôt que d'échouer : le chiffre devient simplement plus grossier, et l'avertissement le dit. Un `provider` non reconnu est rejeté d'emblée, car il se résoudrait silencieusement aux watts génériques on-premise. Daemon uniquement, comme toute voie mesurée, puisqu'une durée de fenêtre est nécessaire. Quand cette section et `[green.alumet.broker]` sont toutes deux configurées, **la mesure gagne**. La précédence suit la série Alumet du broker, pas l'endpoint de scrape : un scrape qui répond sans porter le `label_value` déclaré ne mesure rien, la déclaration prend donc le relais au lieu d'être supprimée par un endpoint qui fonctionne. L'énergie déjà mise de côté par la série est livrée d'abord, et le delta qui arrive à la reprise est jeté une fois, puisqu'il remonte sur du temps déjà facturé par la déclaration.

#### `[green.redfish]` (optionnel, opt-in)

Intégration opt-in avec le standard BMC [Redfish](https://www.dmtf.org/standards/redfish) pour les lectures de puissance murale sur bare-metal. Contrairement à Scaphandre et Kepler (qui mesurent uniquement CPU + DRAM), Redfish lit la sortie réelle de l'alimentation via le BMC, donc la périphérie (NIC, disques, ventilateurs, pertes PSU) est incluse. Bare-metal uniquement, pas de VMs cloud.

| Champ                  | Type   | Défaut                                 | Description                                                                                                                                                                                         |
|------------------------|--------|----------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `endpoints`            | table  | *(vide)*                               | Table `chassis_id` → table d'endpoint avec `url` + `schema`. Obligatoire pour activer le scraper.                                                                                                   |
| `scrape_interval_secs` | entier | `60`                                   | Fréquence de scrape par châssis. Plage valide : 15-3600 (protection contre la limitation de débit BMC, plusieurs BMCs limitent en dessous de 30 s).                                                 |
| `service_mappings`     | table  | `{}`                                   | Associe les noms de service perf-sentinel au châssis qui les héberge. Chaque service mappé au même châssis reçoit le même coefficient.                                                              |
| `ca_bundle_path`       | chaîne | *(aucun)*                              | **Réservé à une version ultérieure.** Définir ce champ aujourd'hui empêche le scraper de démarrer avec une erreur claire. Les certificats BMC auto-signés ne sont pas supportés dans cette release. |
| `auth_header`          | chaîne | *(aucun)*                              | Header Basic au format curl. Préférer `PERF_SENTINEL_REDFISH_AUTH_HEADER`. L'authentification Session-token (POST `/SessionService/Sessions`) n'est pas encore supportée.                           |

Chaque table d'endpoint a deux champs : `url` (chaîne, URL Redfish complète chemin inclus) et `schema` (chaîne, soit `"legacy_power"` soit `"environment_metrics"`). Le schema sélectionne le pointeur JSON canonique utilisé par le parser, sans pointeur tapé par l'opérateur :

| `schema`              | Chemin servi par le BMC                       | Pointeur JSON lu par le parser       |
|-----------------------|-----------------------------------------------|--------------------------------------|
| `legacy_power`        | `/redfish/v1/Chassis/{id}/Power`              | `/PowerControl/0/PowerConsumedWatts` |
| `environment_metrics` | `/redfish/v1/Chassis/{id}/EnvironmentMetrics` | `/PowerWatts/Reading`                |

```toml
[green.redfish]
scrape_interval_secs = 60

[green.redfish.endpoints."chassis-legacy-1"]
url = "https://bmc-rack-01.dc.example/redfish/v1/Chassis/1/Power"
schema = "legacy_power"

[green.redfish.endpoints."chassis-modern-1"]
url = "https://bmc-rack-02.dc.example/redfish/v1/Chassis/1/EnvironmentMetrics"
schema = "environment_metrics"

[green.redfish.service_mappings]
"order-svc"  = "chassis-legacy-1"
"chat-svc"   = "chassis-legacy-1"
"ledger-svc" = "chassis-modern-1"
```

**Quel schema choisir.** `/Power` (legacy_power) a été déprécié par DMTF Release 2020.4 mais reste obligatoire sur les firmwares BMC en 2026, tous les fournisseurs en production l'exposent. `/EnvironmentMetrics` (environment_metrics) est le remplacement moderne qui expose `PowerWatts.Reading` directement, présent en parallèle de `/Power` pendant la transition. Choisir `legacy_power` sauf si la documentation BMC recommande explicitement `EnvironmentMetrics`. Une flotte mixte se déclare en donnant à chaque châssis le schema que son firmware sert.

**Ignoré en mode batch `analyze`.** Comme Scaphandre et Kepler, seul `watch` intègre Redfish.

**Coefficient au niveau du nœud.** Chaque service mappé au même châssis reçoit le **même** coefficient. Deux services sur un même châssis n'auront jamais de coefficients mesurés distincts via Redfish. Voir [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-redfish-bmc) pour la discussion complète de ce compromis et de la variance JSON entre fournisseurs.

#### `[green.cloud]` (optionnel, opt-in)

Estimation d'énergie cloud-native via utilisation CPU% + interpolation SPECpower. Quand cette section est configurée, le daemon `watch` scrape les métriques CPU% depuis un endpoint Prometheus/VictoriaMetrics et utilise une table de lookup embarquée (watts idle/max par type d'instance cloud) pour estimer la consommation énergétique par service. Supporte AWS, GCP, Azure et le matériel on-premise avec surcharge manuelle des watts.

| Champ                   | Type   | Défaut    | Description                                                                                                                                   |
|-------------------------|--------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| `prometheus_endpoint`   | chaîne | *(aucun)* | URL de base de l'API HTTP Prometheus (ex. `http://prometheus:9090` ou `https://prometheus:9090`). TLS supporté via hyper-rustls. Obligatoire. |
| `scrape_interval_secs`  | entier | `15`      | Intervalle de polling en secondes (plage : 1-3600).                                                                                           |
| `default_provider`      | chaîne | *(aucun)* | Fournisseur cloud par défaut : `"aws"`, `"gcp"`, `"azure"`, `"scaleway"`. Les types d'instance Scaleway sont dérivés de son Product Catalog, voir [INSTANCE-TYPES-FR.md](INSTANCE-TYPES-FR.md). |
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
"account-svc" = { provider = "aws", instance_type = "m7i.4xlarge" }       # Sapphire Rapids
"api-asia" = { provider = "gcp", instance_type = "c4d-standard-8" }       # AMD Turin
"analytics" = { provider = "azure", instance_type = "Standard_D8s_v6" }   # Emerald Rapids
"ml-bench" = { provider = "aws", instance_type = "m8g.4xlarge" }          # Graviton 4
```

La liste complète des types couverts, avec leurs watts au repos et maximum, est dans [`INSTANCE-TYPES-FR.md`](./INSTANCE-TYPES-FR.md). Familles d'instances modernes couvertes : AWS m7i/c7i/r7i, m7a/c7a, m6a/c6a, m7g/c7g, m8g/c8g, GCP c3, c3d, c4, c4d, n2d, t2a, Azure Standard_Dv6, Standard_Dadsv6, Standard_Dpsv6 (Cobalt 100), Standard_Ev6. Une entrée CPU-named bare-metal pour Sierra Forest (`xeon-6780e`, watts au niveau système, suppose pleine possession de la puce).

**Watts manuels (on-premise ou matériel custom) :**

```toml
[green.cloud.services]
"my-service" = { idle_watts = 45, max_watts = 120 }
```

**Ignoré en mode `analyze` batch.** Seul le daemon `watch` lance le scraper Prometheus.

**Comportement de repli.** Si le endpoint Prometheus est inaccessible, le daemon utilise le modèle proxy pour tous les services configurés cloud. Les types d'instance inconnus retombent sur un défaut au niveau du fournisseur.

**Limites de précision.** Le modèle d'interpolation SPECpower a une précision d'environ +/-30%, meilleure que le modèle proxy mais moins précise que Scaphandre RAPL. Voir [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-du-cloud-specpower) pour les détails.

#### `[green.electricity_maps]` (optionnel, opt-in)

Intensité carbone en temps réel via l'API Electricity Maps. Mode daemon uniquement.

| Champ                  | Type    | Défaut                               | Description                                                                                                                                                                                                                      |
|------------------------|---------|--------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `api_key`              | string  | aucun                                | Token API. Préférez la variable `PERF_SENTINEL_EMAPS_TOKEN`                                                                                                                                                                      |
| `endpoint`             | string  | `https://api.electricitymaps.com/v4` | URL de base (`http://` ou `https://`). v3 reste accepté mais émet un avertissement de dépréciation au démarrage                                                                                                                  |
| `poll_interval_secs`   | integer | `300`                                | Intervalle de sondage en secondes (plage : 60-86400)                                                                                                                                                                             |
| `emission_factor_type` | string  | `lifecycle`                          | Modèle de facteur d'émission. `lifecycle` (défaut) inclut les émissions amont (fabrication, transport). `direct` inclut uniquement la combustion. Certains référentiels Scope 2 préfèrent `direct` pour une comptabilité stricte |
| `temporal_granularity` | string  | `hourly`                             | Agrégation temporelle de la réponse API. `hourly` (défaut), `5_minutes` ou `15_minutes`. Les valeurs sub-horaires nécessitent un plan payant qui les expose, sinon l'API agrège silencieusement en horaire                       |

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

**Version d'API :** l'endpoint par défaut cible v4 depuis perf-sentinel 0.5.11. v3 reste accepté (le schéma de réponse est identique sur `carbon-intensity/latest`), mais un avertissement de dépréciation est loggué une fois au démarrage du daemon. Pour le faire taire, mettez `endpoint = "https://api.electricitymaps.com/v4"` explicitement. Pour rester délibérément sur v3 (par exemple pour valider A/B contre v4), laissez `endpoint = "https://api.electricitymaps.com/v3"` et acceptez l'avertissement.

**Valeurs inconnues pour `emission_factor_type` et `temporal_granularity` :** ces deux knobs utilisent un parser fail-graceful. Une faute de frappe ou une valeur non supportée (par exemple `temporal_granularity = "5min"` au lieu de `"5_minutes"`) ne rejette pas la config au chargement. La valeur est sanitisée, un `tracing::warn!` est émis, et le daemon retombe sur le défaut. Surveillez les logs du daemon au démarrage si vous suspectez une faute de frappe, la ligne warn nommera le champ et la valeur fautifs.

**Visibilité dans les rapports (depuis perf-sentinel 0.5.12) :** la configuration de scoring active (version d'API, modèle de facteur d'émission, granularité temporelle) est exposée à trois endroits pour qu'un auditeur Scope 2 puisse vérifier quel modèle carbone a produit les chiffres sans lire la TOML de l'opérateur.

- Le rapport JSON porte toujours `green_summary.scoring_config` quand le scoring GreenOps est actif : il enregistre les coefficients appliqués et un flag `electricity_maps`. Les champs propres à l'API ne sont significatifs que lorsque ce flag vaut `true`.
- Le dashboard HTML rend un bandeau de chips au-dessus de la table green-regions. Les valeurs par défaut (`v4`, `lifecycle`, `hourly`) apparaissent en chips neutres, les opt-ins (`direct`, `5_minutes`, `15_minutes`) en chips accent, l'endpoint legacy `v3` en chip warning miroir de l'avertissement de dépréciation. Les tooltips natifs du navigateur expliquent chaque valeur.
- La sortie terminale `print_green_summary` ajoute une ligne `Carbon scoring: Electricity Maps v4, lifecycle, hourly` avant le détail par région.

Le bandeau et la ligne terminal sont masqués quand `[green.electricity_maps]` n'est pas configuré.

#### `[green] calibration_file` (optionnel)

Chemin vers un fichier TOML de calibration généré par `perf-sentinel calibrate`. Les facteurs par service multiplient l'énergie proxy par opération.

```toml
[green]
calibration_file = ".perf-sentinel-calibration.toml"
```

**Limites de taille d'entrée pour `perf-sentinel calibrate`.** Les deux entrées sont plafonnées pour éviter une consommation mémoire incontrôlée : le fichier `--traces` est plafonné à 1 Gio (plafond batch fixe depuis 0.8.7, identique à `analyze`) et le CSV `--measured-energy` est plafonné à 64 MiB. Calibrate termine avec une erreur claire si l'un des fichiers dépasse sa limite. 64 MiB est généreux pour des milliers d'échantillons RAPL par minute, si vous avez besoin de plus, augmentez `max_payload_size` et ouvrez une issue décrivant la charge de travail.

#### `perf-sentinel tempo` (pas de section de config)

La sous-commande `tempo` s'exécute en **mode batch** (pas daemon), récupère les traces depuis l'API HTTP d'un Grafana Tempo et les passe dans le pipeline d'analyse standard. Ses réglages propres sont des flags CLI uniquement, il n'existe pas de section `[tempo]` : `--endpoint` est obligatoire, `--max-traces` vaut `100` par défaut et est borné à 1..=10000 (le plafond de lecture du client, pas celui de Tempo), aux côtés de `--trace-id`, `--service`, `--lookback`, `--from`/`--to`, `--sort` et `--auth-header`. Lancez `perf-sentinel tempo --help` pour la liste à jour. Une table `[tempo]` écrite dans le fichier de config fait échouer le chargement depuis la 0.12.0, comme toute table racine inconnue. Le fichier `--config` s'applique quand même pour le reste, les seuils et la détection en particulier, puisque les traces récupérées passent par le même pipeline.

### `[daemon]`

Paramètres du mode streaming (`perf-sentinel watch`).

| Champ                     | Type     | Défaut                      | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
|---------------------------|----------|-----------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `listen_address`          | chaîne   | `"127.0.0.1"`               | Adresse IP de liaison pour les endpoints OTLP et métriques. Utilisez `127.0.0.1` pour un accès local uniquement. **Attention :** définir une adresse non-loopback expose des endpoints non authentifiés sur le réseau, utilisez un reverse proxy ou une politique réseau                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `listen_port_http`        | entier   | `4318`                      | Port pour le récepteur OTLP HTTP et l'endpoint Prometheus `/metrics` (plage : 1-65535)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `listen_port_grpc`        | entier   | `4317`                      | Port pour le récepteur OTLP gRPC (plage : 1-65535)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `json_socket`             | chaîne   | `"/tmp/perf-sentinel.sock"` | Chemin du socket Unix pour l'ingestion d'événements JSON                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `max_active_traces`       | entier   | `10000`                     | Nombre maximum de traces conservées en mémoire. En cas de dépassement, la trace la plus ancienne est évincée (LRU). Plage : 1 à 1 000 000                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `trace_ttl_ms`            | entier   | `30000`                     | Durée de vie des traces en millisecondes. Une trace devient périmée quand plus aucun span n'est arrivé depuis cette durée, et le balayage qui l'évince et l'analyse tourne sur un tic à la moitié de cette valeur, donc l'échéance effective vaut cette durée plus un tic au plus. Un span qui arrive dans cet intervalle rejoint la trace périmée et la rafraîchit au lieu d'en ouvrir une neuve, ce qui compte pour qui rejoue un identifiant de trace. Plage : 100 à 3 600 000                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `sampling_rate`           | flottant | `1.0`                       | Fraction des traces à analyser (0.0 à 1.0). Réduire en dessous de 1.0 pour diminuer la charge dans les environnements à fort trafic. Les traces sont gardées ou jetées entières sur un hachage du trace id, donc les détecteurs par trace restent corrects sur ce qui reste et les ratios comme le ratio de gaspillage I/O échantillonnent numérateur et dénominateur de la même façon, mais les comptes absolus (findings, occurrences, totaux `perf_sentinel_*`) décrivent alors cette fraction du trafic, et un pattern présent sur une petite part du trafic peut être entièrement écarté. Sous 1.0, le daemon émet une entrée `tuning` dans `Report.warning_details` qui le signale, et `0.0`, que cette plage accepte, reçoit son propre message puisque plus aucune trace n'est analysée. Un sampling fait par un collector en amont a le même effet et ne peut pas être détecté, voir [HELM-DEPLOYMENT-FR.md](HELM-DEPLOYMENT-FR.md#sampling-du-collector-et-ce-qui-atteint-le-daemon)                                                                                                                                                                                                                   |
| `max_events_per_trace`    | entier   | `1000`                      | Plafond par trace appliqué séparément aux événements stockés (buffer circulaire), aux contextes d'endpoint entrant (endpoint et lien parent facultatif) et aux entrées d'ascendance de spans (lien parent intermédiaire et endpoint résolu facultatif). Une trace peut contenir ces trois collections bornées ; les événements et entrées d'ascendance sont alloués progressivement et suivent leur politique de rotation respective. Plage : 1 à 100 000                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `max_payload_size`        | entier   | `16777216`                  | Taille maximale en octets d'un payload JSON unique (défaut : 16 Mio depuis 0.5.13, monté depuis 1 Mio parce qu'un snapshot daemon de `/api/export/report` dépasse déjà 1 Mio sur un cluster modeste). Plage : 1 024 à 104 857 600 (100 Mo). Le défaut sit à la borne supérieure inclusive de la zone de confort par design. Depuis 0.8.7 ce plafond ne borne que les payloads réseau du daemon : les sous-commandes batch (`analyze`, `diff`, `report`, `explain`, `calibrate`, `pg-stat`, `bench`) lisent les fichiers locaux sous un plafond fixe de 1 Gio                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `environment`             | chaîne   | `"staging"`                 | Label d'environnement de déploiement. Valeurs acceptées : `"staging"` (défaut, confiance moyenne) ou `"production"` (confiance élevée). Tague chaque finding avec le champ `confidence` correspondant pour les consommateurs en aval (perf-lint planifié). Insensible à la casse ; toute autre valeur est rejetée au chargement de la config                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `tls_cert_path`           | chaîne   | *(absent)*                  | Chemin vers une chaîne de certificats TLS au format PEM pour les récepteurs OTLP. Quand renseigné avec `tls_key_path`, les listeners gRPC et HTTP utilisent TLS. Quand absent, les listeners utilisent TCP en clair. Chaque listener TLS borne à 128 les handshakes en vol simultanés (non configurable) et coupe les pairs qui ne terminent pas le handshake en 10 secondes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `tls_key_path`            | chaîne   | *(absent)*                  | Chemin vers la clé privée TLS au format PEM. Doit être renseigné avec `tls_cert_path` (les deux ou aucun). Sous Unix, le daemon avertit si le fichier de clé est lisible par le groupe ou les autres                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `api_enabled`             | booléen  | `true`                      | Active les endpoints de l'API de requêtage du daemon (`/api/findings`, `/api/explain/{trace_id}`, `/api/correlations`, `/api/status`). Mettre à `false` pour désactiver l'API tout en conservant l'ingestion OTLP et `/metrics`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `read_api_key`            | chaîne   | *(absent)*                  | Clé en lecture seule pour les deux GET qu'une clé d'écriture garde, `GET /api/acks` et `GET /api/incidents` (depuis 0.20.0). Ne satisfait jamais un `POST` ni un `DELETE`, n'ajoute jamais de porte là où aucune clé d'écriture n'est posée, et doit différer de `[daemon.ack] api_key` et de `[daemon.incidents] api_key`, puisqu'une clé de lecture égale à une clé d'écriture est cette clé. Donnez-la à Grafana et au Hub pour que rien de ce qui ne fait que lire ne détienne une clé capable d'acquitter ou de fabriquer un incident. 12 caractères minimum. `PERF_SENTINEL_READ_API_KEY` prime sur cette valeur                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `max_retained_findings`   | entier   | `10000`                     | Nombre maximum de findings récents conservés dans le buffer circulaire du daemon pour l'API de requêtage. Les findings les plus anciens sont évincés quand la limite est atteinte. Plage : 0 à 10 000 000, où `0` désactive complètement le store et libère sa mémoire (recommandé quand `api_enabled = false`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `max_export_findings`     | entier   | `1000`                      | Nombre maximum de findings portés par un snapshot `/api/export/report`. Distinct du plafond de `/api/findings`, qui pagine une API de consultation, là où celui-ci dimensionne un export délibéré : un store contenant des dizaines de milliers de findings n'en expédie qu'une tranche, la plus récente, et le snapshot l'annonce dans `warning_details`. L'augmenter alourdit le corps de la réponse, et le HTML qui en est rendu, de quelques Ko par finding. Au-delà d'environ 2000 le snapshot dépasse la limite de corps de 8 Mio avec laquelle `query inspect` et `query monitor` le récupèrent, et le daemon émet un avertissement. Plage : 0 à 100 000, où `0` n'exporte que l'enveloppe (chiffres green, aucun finding). Comme la `quality_gate` exportée compte les findings de cette tranche, à `0` ses trois règles de comptage passent quoi qu'ait détecté le daemon. La quatrième, `io_waste_ratio_max`, lit `green_summary`, qu'aucun plafond ne vide : le verdict ne passe donc pas systématiquement, il cesse simplement de refléter les findings. Cela suffit à réserver `0` à une sonde de liveness, pas à une sonde d'alerte. Surchargeable au lancement avec `watch --max-export-findings` |
| `max_retained_traces`     | entier   | `50`                        | Nombre maximum de traces dont les spans masqués sont conservés pour que `/api/export/report` porte un arbre de spans que le dashboard HTML sait dessiner. Sans cela, la fenêtre de corrélation lâche les spans d'une trace quelques secondes après sa fin et un rapport exporté a des findings sans rien à montrer autour. Coûte de la mémoire proportionnellement à `max_events_per_trace`, d'où un plafond bien plus petit que `max_retained_findings`. `0` n'en conserve aucune                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `ingest_queue_capacity`   | entier   | `1024`                      | Capacité du canal d'ingestion : lots d'événements de span tamponnés entre les listeners et la boucle d'événements. Une fois plein, l'ingestion applique une contre-pression aux producteurs. Augmentez-la pour absorber un trafic plus en rafales, au prix de la mémoire. Plage : 1 à 1 048 576                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `analysis_queue_capacity` | entier   | `1024`                      | Capacité de la file du worker d'analyse : lots évincés et expirés en attente de detect+score. Une fois pleine, des lots entiers sont délestés et comptés sur `perf_sentinel_analysis_shed_batches_total`. Augmentez-la pour tolérer des rafales d'analyse plus longues avant délestage. Plage : 1 à 1 048 576                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `per_service_labels`      | booléen  | `true`                      | Si `perf_sentinel_findings_total` et `perf_sentinel_slow_duration_seconds` portent un label `service` (depuis 0.18.0). La cardinalité est plafonnée par run du daemon (128 services sur les findings, 64 sur l'histogramme), les services au-delà d'un plafond se replient dans `service="_other"` pour que les totaux restent exacts. `false` rend le label vide sur chaque série, restaurant la forme d'avant 0.18, et depuis 0.19.0 la série sans label de l'histogramme n'est pré-chauffée au démarrage que si `per_grouping_labels` est aussi désactivé. Un label vide n'est pas un label : un scrape honor labels (le ServiceMonitor du chart) le supprime et un scrape ordinaire l'écrase avec le nom de la cible, donc avec le réglage désactivé le filtre `Service` du tableau de bord livré liste toujours les services (depuis les counters I/O par service) mais en choisir un affiche 0 finding et aucune latence de span lent, gardez `All` ou laissez le réglage actif. Sans effet sur les counters I/O par service (`service_io_ops_total`, `service_avoidable_io_ops_total`, `service_analyzed_io_ops_total`), par service par construction                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `per_grouping_labels`     | booléen  | `true`                      | Si `perf_sentinel_findings_total`, `perf_sentinel_slow_duration_seconds`, `perf_sentinel_service_io_ops_total`, `perf_sentinel_service_avoidable_io_ops_total` et `perf_sentinel_service_analyzed_io_ops_total` portent un label `grouping` à côté de `service` (depuis 0.19.0), contenant le premier attribut présent parmi `[detection] grouping_attributes` (vide quand le span n'en portait aucun), la valeur seule sans sa clé d'attribut, donc ne configurez qu'une clé quand deux attributs peuvent partager une valeur. La cardinalité est plafonnée par run du daemon sur les paires (service, grouping) admises, après les plafonds de services (512 côté analyse, 256 sur l'histogramme, 4096 à l'ingestion). Une paire au-delà garde son service et replie son regroupement dans `grouping="_other"`, pour que `sum by (service)` égale toujours la série de la 0.18.0. Les plafonds comptent des paires, donc ce qui se replie suit le produit services × regroupements (11 services dans 20 namespaces font 220 paires, 100 dans 10 en font 1000). `false` rend le label vide sur chaque série, restaurant la forme de la 0.18.0. Un label vide n'est pas un label, et comme rien n'attache de label de cible `grouping`, la série n'en a simplement aucun : le filtre `Grouping` du tableau de bord livré ne l'atteint que sous `All`. Contrairement à `per_service_labels`, ce réglage gouverne aussi les trois counters I/O par service. Il ne s'appelle pas `namespace` parce que Prometheus Operator attache un label de cible `namespace` et que le `honorLabels: true` du chart ferait gagner celui du daemon |
| `memory_high_water_pct`   | entier   | `0`                         | Contrôle d'admission par pression mémoire, en pourcentage de la limite mémoire cgroup v2. Quand le ratio de working set (`memory.current` moins le page cache récupérable `inactive_file`, sur `memory.max`) franchit ce seuil, l'ingest est rejeté avec un statut retryable (compté sur `perf_sentinel_otlp_rejected_total{reason="memory_pressure"}`, état sur la gauge `perf_sentinel_ingest_memory_pressure`) et reprend une fois l'usage retombé 5 points de pourcentage sous le seuil (hystérésis, pour que l'ingest n'oscille pas autour de la frontière), bornant la RSS indépendamment de la profondeur de queue. `0` désactive le garde-fou (défaut). Linux/cgroup-v2 uniquement, inerte ailleurs, et fail-open si le cgroup devient illisible. Réglez 80-85 pour garder de la marge au-dessus du régime permanent. Le garde-fou échantillonne à cadence fixe, donc dimensionnez le seuil pour que la marge `limite - seuil` dépasse le pic en vol (un flood soutenu peut dépasser une marge trop fine, voir [RUNBOOK-FR](RUNBOOK-FR.md#pression-mémoire-ou-oom-du-daemon)). Plage : 0, ou 6 à 100 (1-5 mettrait la borne basse d'hystérésis à zéro ou en dessous)                                     |

##### Zones de confort et avertissements au démarrage

Les limites du daemon acceptent toute valeur à l'intérieur de leurs bornes dures (rejetées au chargement de la config), mais `perf-sentinel watch` émet un log `WARN` unique au démarrage quand une valeur sort de la zone de confort recommandée. L'avertissement est informatif : le daemon continue de tourner. Sert de garde-fou pour vérifier qu'une valeur inhabituelle est bien volontaire.

| Champ                   | Zone de confort        | Pourquoi une valeur hors zone est inhabituelle                                                                                                                                                                                                                                                                                              |
|-------------------------|------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `max_payload_size`      | 256 Kio à 16 Mio       | Plus petit risque de rejeter des batches OTLP légitimes ; plus grand augmente la latence d'ingestion et le RSS                                                                                                                                                                                                                              |
| `max_active_traces`     | 1 000 à 100 000        | Plus petit déclenche une éviction LRU agressive ; plus grand fait croître la mémoire à peu près linéairement                                                                                                                                                                                                                                |
| `max_events_per_trace`  | 100 à 10 000           | Plus petit tronque les traces complexes ; plus grand n'améliore que rarement la qualité de détection                                                                                                                                                                                                                                        |
| `max_retained_findings` | 100 à 100 000 (ou `0`) | Plus petit évince les findings avant que `/api/findings` ne puisse les servir ; plus grand garde un backlog en mémoire. `0` désactive le store et reste silencieux                                                                                                                                                                          |
| `trace_ttl_ms`          | 1 000 à 600 000        | Sous 1s, les traces sont vidées avant que les spans lents n'arrivent ; au-dessus de 10min, des traces presque mortes restent en mémoire                                                                                                                                                                                                     |
| `max_fanout`            | 5 à 1 000              | Plus petit inonde le store de findings de bruit ; plus grand supprime la plupart des détections de fanout                                                                                                                                                                                                                                   |

Les zones de confort jugent la valeur statique au démarrage. Au
runtime, le daemon les complète avec un conseiller de réglages : quand
les compteurs lifetime montrent un réglage sous-dimensionné pour la
charge observée (sheds de file, rejets d'ingestion, fenêtre de traces
presque pleine...), `/api/export/report` émet des entrées `tuning`
dans `Report.warning_details` nommant le réglage, sa valeur actuelle
et l'ajustement suggéré. Voir [METRICS-FR.md](METRICS-FR.md) section
"Kinds de warning : transitoire vs collant" pour la table des règles.

#### `[daemon.correlation]` (optionnel)

Corrélation temporelle cross-trace en mode daemon. Quand activé, le daemon détecte les co-occurrences récurrentes entre findings de services ou traces différents (ex. "chaque fois que le N+1 dans order-svc se déclenche, une saturation du pool apparaît dans payment-svc dans les 2 secondes").

| Champ                | Type     | Défaut  | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
|----------------------|----------|---------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`            | booléen  | `false` | Active la corrélation cross-trace. Nécessite le mode daemon `watch` avec un trafic soutenu pour des résultats utiles                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `window_minutes`     | entier   | `10`    | Fenêtre glissante en minutes sur laquelle les co-occurrences sont suivies                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `lag_threshold_ms`   | entier   | `2000`  | Décalage temporel maximum en millisecondes entre deux findings pour les considérer co-occurrents                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `min_co_occurrences` | entier   | `3`     | Nombre minimum de co-occurrences avant de remonter une corrélation                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `min_confidence`     | flottant | `0.5`   | Score de confiance minimum (0.0 à 1.0) pour remonter une corrélation. Calculé comme `co_occurrence_count / total_occurrences_of_A`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `max_tracked_pairs`  | entier   | `10000` | Nombre maximum de paires de findings retenues simultanément. Le plafond borne ce que le corrélateur garde, pas ce qu'un batch parcourt : une topologie large balaie le produit croisé des findings entrants et de la fenêtre de décalage quelle que soit cette valeur, donc l'abaisser fait refuser davantage plutôt qu'allouer moins. Les paires croissent comme le nombre de types de findings multiplié par le nombre de services : une poignée de services peut dépasser le défaut, et au-delà du plafond `/api/correlations` renvoie un sous-ensemble arbitraire sans que la sortie le signale. `perf_sentinel_correlator_pairs_evicted_total` est le signal, et le daemon loggue un avertissement à la première éviction. Pas de contrôle de zone de confort au démarrage |

```toml
[daemon.correlation]
enabled = true
window_minutes = 10
lag_threshold_ms = 2000
min_co_occurrences = 3
min_confidence = 0.5
```

Les corrélations sont exposées via `GET /api/correlations` (quand `api_enabled = true`) et émises en NDJSON sur le flux stdout du daemon.

#### `[daemon.ack]` (optionnel, depuis 0.5.20)

Store d'acks runtime côté daemon. Complète les acks TOML CI (voir
`ACKNOWLEDGMENTS-FR.md`) avec un fichier JSONL append-only muté via les
endpoints HTTP `POST` / `DELETE` `/api/findings/{signature}/ack`.

| Champ          | Type    | Défaut                                                  | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
|----------------|---------|---------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`      | booléen | `true`                                                  | Active les endpoints ack du daemon. Quand `false`, `POST` / `DELETE` / `GET /api/acks` retournent 503 Service Unavailable, et `GET /api/findings` saute le filtre ack                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `storage_path` | chaîne  | `<data_local_dir>/perf-sentinel/acks.jsonl`             | Override pour l'emplacement du fichier JSONL. Résolu au runtime via `dirs::data_local_dir()` (XDG sur Linux, Library/Application Support sur macOS) en absence d'override. La politique d'erreur dépend de la source : un override explicite qui échoue à s'ouvrir est fatal au démarrage, un chemin par défaut qui ne peut pas être résolu ou ouvert ne produit qu'un WARN et laisse les deux routes d'écriture d'ack en 503 (`GET /api/acks` n'est gardé que par l'authentification et répond toujours 200 avec une liste vide). Pas de fallback sur `/tmp` car le fichier contient des données d'audit qui doivent survivre à un reboot. Les conteneurs minimaux sans `HOME` (dont l'image publiée `FROM scratch`) tombent dans le second cas, définissez donc ce champ explicitement là-bas |
| `api_key`      | chaîne  | *(absent)*                                              | Secret optionnel gardant l'accès aux acks. Quand défini, `POST` et `DELETE` sur `/api/findings/{signature}/ack` **et** `GET /api/acks` exigent que le header `X-API-Key` matche (comparaison constant-time via `subtle`, ce `GET` acceptant aussi `[daemon] read_api_key`) ; `GET /api/findings` reste non authentifié. Définissez la variable d'environnement `PERF_SENTINEL_ACK_API_KEY` pour surcharger cette valeur et garder la clé hors de la config committée ; la variable d'env a la priorité quand elle est présente, même convention que `PERF_SENTINEL_EMAPS_TOKEN`. Une chaîne vide (ou une variable d'env définie à vide) est rejetée au load de config                                                                                                                                                                             |
| `toml_path`    | chaîne  | `".perf-sentinel-acknowledgments.toml"` (relatif à CWD) | Override pour le fichier TOML d'acks CI. Lu au démarrage, puis relu chaque minute pour qu'une modification s'applique sans redémarrage, ce qui compte quand le fichier est une ConfigMap montée. Une relecture en échec conserve les acks précédents et loggue un avertissement, tout comme un fichier disparu : ni un fichier à moitié écrit ni un volume démonté ne désacquitte quoi que ce soit. Seul un chemin configuré explicitement est relu, jamais le défaut relatif à CWD, et `enabled = false` arrête la relecture avec le reste. À régler en chemin absolu pour les déploiements systemd ou container où CWD n'est pas la racine du repo                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |

```toml
[daemon.ack]
enabled = true
storage_path = "/var/lib/perf-sentinel/acks.jsonl"
# api_key = "<à-rotater>"
toml_path = "/etc/perf-sentinel/acknowledgments.toml"
```

Le fichier JSONL est rejoué et atomiquement réécrit (via tmp + rename)
à chaque redémarrage du daemon, donc des cycles `ack` / `unack`
répétés ne peuvent pas s'accumuler au-delà de leur état actif net. Sur
Unix, le fichier est créé avec le mode `0600` (lecture-écriture
propriétaire uniquement).

#### `[daemon.hub_export]` (optionnel)

Export asynchrone et borné des findings live vers PerfSentinelHub. Le daemon
fusionne les findings répétés par signature et ne conserve que leur dernière
valeur : un problème très fréquent ne déclenche donc pas une requête réseau
par détection.

| Champ                 | Type    | Défaut     | Description                                                                                                    |
|-----------------------|---------|------------|----------------------------------------------------------------------------------------------------------------|
| `enabled`             | booléen | `false`    | Active l'export Hub. La détection reste non bloquante si le Hub est indisponible                               |
| `endpoint`            | chaîne  | *(absent)* | URL du Hub terminant par `/api/import/findings`                                                                |
| `source_id`           | chaîne  | *(absent)* | Identifiant de source Hub : 1 à 64 lettres ASCII, chiffres, `.`, `_` ou `-`                                    |
| `api_key_file`        | chaîne  | *(absent)* | Fichier contenant la clé d'import de la source. Requis si activé ; la clé doit contenir au moins 32 caractères |
| `batch_size`          | entier  | `100`      | Findings par requête, de 1 à 100                                                                               |
| `flush_interval_secs` | entier  | `5`        | Délai normal maximal de regroupement, de 1 à 300 secondes                                                      |
| `max_pending`         | entier  | `10000`    | Nombre maximal de signatures distinctes en attente, de 1 à 1 000 000                                           |

```toml
[daemon.hub_export]
enabled = true
endpoint = "https://hub.example.com/api/import/findings"
source_id = "production-a"
api_key_file = "/run/secrets/perf-sentinel-hub-api-key"
batch_size = 100
flush_interval_secs = 5
max_pending = 10000
```

La structure en attente est une table de dernières valeurs, pas une file
illimitée. Une signature est envoyée dès sa première découverte ou si sa
sévérité empire, puis rafraîchie au plus une fois par heure tant qu'elle se
répète. La table en attente et le cache des succès récents sont chacun bornés
à `max_pending` ; évincer du cache des succès récents n'est pas une perte et
n'est pas compté, alors qu'une éviction de la table en attente, un finding trop
gros et un lot rejeté par le Hub avec un 4xx non rejouable augmentent tous
`perf_sentinel_hub_export_dropped_total`. Un échec HTTP conserve le lot
fusionné et déclenche un retry avec backoff exponentiel et jitter ; un
redémarrage efface les deux caches. Requêtes et corps JSON sont bornés à 100
findings et 2 Mio. Utilisez HTTPS hors réseau privé de confiance.

À l'arrêt propre (SIGTERM, `helm upgrade`, redémarrage progressif), l'exporteur
vide ce qu'il détient encore avant que le processus ne se termine, pendant au
plus 10 secondes. Ce budget est délibéré : un Hub injoignable ne doit pas
retenir le daemon au-delà du délai de grâce de l'orchestrateur, où le signal
suivant est SIGKILL et où plus rien n'est transmis. Quand le budget expire, les
findings encore en attente sont perdus et un `WARN` en donne le nombre.
Dimensionnez `terminationGracePeriodSeconds` au-dessus de ce budget pour que le
drain ait la place de s'exécuter, sinon le pod est tué en plein envoi.

Montez la clé API depuis un fichier adossé à un Secret. Ne la mettez ni dans
une ConfigMap ni dans `.perf-sentinel.toml`. Avec le chart Helm, utilisez
`extraVolumes` et `extraVolumeMounts` pour exposer la clé au chemin
`api_key_file` configuré.

#### `[daemon.incidents]` (optionnel, depuis 0.20.0)

Webhooks d'incident entrants, pour qu'une alerte donne au daemon le
moment où un service est tombé ou a saturé, et que le daemon fige les
findings de la fenêtre avant que le ring ne les évince. Voir
[QUERY-API-FR.md](QUERY-API-FR.md) pour les endpoints. Daemon uniquement,
désactivé par défaut, et à ne pas confondre avec
`[daemon] memory_high_water_pct`, qui désigne le cgroup du daemon
lui-même.

| Champ           | Type    | Défaut               | Description                                                                                                                                                                                    |
|-----------------|---------|----------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`       | boolean | `false`              | Expose `POST` et `GET /api/incidents`. Une surface d'écriture entrante est opt-in. Les deux routes répondent 503 sans elle                                                                      |
| `api_key`       | string  | *(absent)*           | **Obligatoire quand activé**, sinon erreur de configuration, parce que la route écrit. Comparé en temps constant à `X-API-Key`, 12 caractères minimum, et le `GET` accepte aussi `[daemon] read_api_key`. `PERF_SENTINEL_INCIDENTS_API_KEY` prime sur cette valeur |
| `lookback_ms`   | integer | `300000`             | Profondeur de findings qu'un incident posté fige, de 1000 à 86400000                                                                                                                            |
| `max_retained`  | integer | `200`                | Incidents gardés dans le ring en mémoire, de 1 à 1000, chacun portant jusqu'à 1000 findings figés. Le ring meurt avec le daemon                                                                                                            |
| `service_label` | string  | `service`            | Libellé d'alerte portant le nom de service perf-sentinel. Une alerte qui ne le porte pas est refusée, c'est la clé de jointure avec les findings                                               |
| `kind_label`    | string  | `perf_sentinel_kind` | Libellé d'alerte portant le genre : `oom_kill`, `memory_saturation`, `restart`, `deploy` ou `other`. Tout le reste vaut `other`, jamais deviné depuis `alertname`                              |
| `namespace_label` | string | `namespace`         | Libellé d'alerte portant le namespace, celui que les alertes kube-prometheus portent déjà. Optionnel, jamais un motif de refus : sa valeur est portée sur l'incident comme `namespace` et le paramètre `namespace` de `GET /api/incidents` filtre dessus |
| `archive_path`  | string  | *(absent)*           | Ajoute chaque nouvel incident, fermeture et consolidation à ce fichier JSON par lignes, ouvert au démarrage pour qu'un mauvais chemin fasse échouer le daemon. Absent signifie que le ring en mémoire est le seul enregistrement, et un événement mémoire au niveau du nœud qui tue le service observé emporte souvent un daemon colocalisé. Append-only, le dernier enregistrement d'un id fait foi, créé en `0600` et ramené à ce mode quand un montage de volume l'affaiblit, refusé quand le fichier n'appartient pas au daemon, sans rotation |

```toml
[daemon.incidents]
enabled = true
# api_key = "<rotate-this>"      # ou PERF_SENTINEL_INCIDENTS_API_KEY
lookback_ms = 300000
```

La fenêtre qu'un incident posté fige est
`[at_ms - lookback_ms, at_ms + 2 * trace_ttl_ms]`, résolue avec les deux
bornes, donc `seen_count` et `first_seen_ms` des findings figés décrivent
la fenêtre plutôt que l'historique retenu entier. Voir
[QUERY-API-FR.md](QUERY-API-FR.md) pour la raison de cette fermeture
après l'incident et pour la passe de consolidation qui en remplit la
queue.

#### `[daemon.cors]` (optionnel, depuis 0.5.23)

Cross-origin resource sharing pour les endpoints `/api/*` du daemon.
Désactivé par défaut (aucun en-tête `Access-Control-Allow-Origin`
n'est émis, la posture loopback-only est préservée). À activer quand
un client navigateur doit appeler le daemon, typiquement le rapport
HTML en mode live (`perf-sentinel report --daemon-url <URL>`, voir
`HTML-REPORT-FR.md`).

**Scope** : le layer CORS est branché uniquement sur le sous-router
`/api/*`. Le chemin d'ingestion OTLP (`/v1/traces`), l'exposition
Prometheus (`/metrics`) et le liveness probe (`/health`) ne sont PAS
exposés en cross-origin, même en mode wildcard. Les pages navigateur
ne peuvent pas poster des traces, scraper `/metrics` ou frapper
`/health` quel que soit `allowed_origins`. Ce confinement est
intentionnel, les clients navigateur n'ont aucun usage légitime pour
ces surfaces.

**Exposition des read endpoints** : chaque GET `/api/*`
(`/api/findings`, `/api/acks`, `/api/status`, `/api/correlations`,
`/api/explain/*`, `/api/export/report`) est non authentifié par
design, en cohérence avec la posture loopback-only pré-0.5.23. Une
fois qu'une origine est whitelistée, tout onglet de navigateur sur
cette origine peut lire chaque signature de finding, métadonnée d'ack
et export de trace que le daemon retient. **Whiteliste seulement les
origines auxquelles vous faites confiance pour voir l'ensemble des
données du daemon.** Mélanger des origines non fiables avec le mode
wildcard (`["*", "https://x"]`) est rejeté au load de la config.

| Champ             | Type          | Défaut | Description                                                                                                                                                                                                                                                                                               |
|-------------------|---------------|--------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `allowed_origins` | array<string> | `[]`   | Liste des origines autorisées à appeler la surface `/api/*` du daemon. `["*"]` est le mode wildcard (développement uniquement, sans credentials). Une liste non-wildcard whiteliste les origines exactes. Chaque entrée doit être une origine complète (scheme + hôte + port optionnel), sans slash final |

Exemple wildcard (développement) :

```toml
[daemon.cors]
allowed_origins = ["*"]
```

Exemple production (whitelist) :

```toml
[daemon.cors]
allowed_origins = [
    "https://reports.example.com",
    "https://gitlab.example.com",
]
```

Méthodes autorisées : `GET`, `POST`, `DELETE`, `OPTIONS`.
En-têtes autorisés : `Content-Type`, `X-API-Key`. (`X-User-Id` n'est
pas annoncé parce que le daemon ne l'enforce pas côté serveur, le
champ `by` sur le body d'un ack POST est attesté par l'opérateur
uniquement.)
Préflight `Access-Control-Max-Age` : 120 secondes. Assez long pour
amortir l'aller-retour OPTIONS sur une interaction typique, assez
court pour qu'un whitelist resserré prenne effet au prochain
préflight navigateur sans refresh forcé.

Le layer CORS ne positionne pas `Access-Control-Allow-Credentials: true`,
incompatible avec `["*"]` et inutile car le daemon authentifie via
l'en-tête `X-API-Key` et non via des cookies. Les navigateurs sur une
origine non-whitelistée reçoivent une réponse sans en-tête
`Access-Control-Allow-Origin` et la requête est bloquée côté client,
sans rejet côté daemon.

Les origines qui ne se parsent pas comme une valeur d'en-tête HTTP
valide (typiquement un copier-coller avec des caractères de contrôle)
sont écartées au démarrage avec un log `warn!` et le reste de la
liste est honoré. Si toutes les entrées sont invalides, le layer est
désactivé entièrement. Si `daemon_api_enabled = false`, le layer
CORS est skippé (le sous-router `/api/*` n'est pas monté de toute
façon) et un `warn!` signale la config inutilisée.

Depuis 0.5.27, combiner
`allowed_origins = ["*"]` avec `[daemon.ack] api_key` émet aussi un
`warn!` au démarrage. Le mode CORS wildcard combiné à une auth
`X-API-Key` autorise n'importe quelle origine navigateur à rejouer
une clé capturée à travers le daemon, même sans cookie ni mode
`Allow-Credentials`. Whitelistez des origines explicites pour les
déploiements de production qui configurent la clé API.

### `[reporting]`

Paramètres de divulgation publique consommés par `disclose`, `hash-bake` et `verify-hash`. La section entière est optionnelle. Une section absente signifie que l'opérateur n'a jamais demandé de divulgation périodique. Guide complet dans `docs/FR/REPORTING-FR.md`, référence des champs dans `docs/FR/SCHEMA-FR.md`.

| Champ                   | Type   | Défaut         | Description                                                                                                                                                                                 |
|-------------------------|--------|----------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `intent`                | string | *(non défini)* | `internal`, `official` ou `audited`. Lu au démarrage du daemon uniquement : `audited` le fait REFUSER de démarrer (pas implémenté), `official` exige `org_config_path` et valide ce fichier |
| `org_config_path`       | string | *(non défini)* | Chemin vers le TOML organisation/scope/méthodologie, exigé quand `intent = "official"`                                                                                                      |
| `confidentiality_level` | string | *(non défini)* | `internal` ou `public`. **Réservé**, validé puis inutilisé : la valeur publiée vient de `disclose --confidentiality`                                                                        |
| `disclose_output_path`  | string | *(non défini)* | **Réservé**, sans effet aujourd'hui, c'est `disclose --output` qui écrit le rapport                                                                                                         |
| `disclose_period`       | string | *(non défini)* | `calendar-quarter`, `calendar-month`, `calendar-year` ou `custom`. **Réservé**, inutilisé, voir `disclose --period-type`                                                                    |

`disclose` ne lit aucune de ces clés : il prend `--intent`, `--confidentiality`, `--period-type`, `--org-config` et `--output` sur la ligne de commande. Ce que cette section fait encore, c'est conditionner le démarrage du daemon via `intent` et `org_config_path`.

La sous-section `[reporting.sigstore]` porte les endpoints Sigstore, Rekor étant le journal de transparence et Fulcio l'autorité de certification. **Les deux sont réservés : ils sont parsés puis inutilisés.** `verify-hash` délègue la vérification de signature au binaire `cosign` et appelle `cosign verify-blob` sans `--rekor-url` ni `--fulcio-url`, cosign suit donc sa propre configuration et renseigner l'une de ces clés ici n'a aucun effet. Pointez une instance Sigstore privée sur cosign lui-même en attendant que ce soit câblé.

| Champ        | Type   | Défaut                        | Description                                              |
|--------------|--------|-------------------------------|----------------------------------------------------------|
| `rekor_url`  | string | `https://rekor.sigstore.dev`  | Endpoint du journal de transparence Rekor. Réservé.      |
| `fulcio_url` | string | `https://fulcio.sigstore.dev` | Endpoint de l'autorité de certification Fulcio. Réservé. |

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
n_plus_one_messaging_warning_max = 3
io_waste_ratio_max = 0.30
# min_usable_span_ratio = 0.9   # absent = désactivé, voir le tableau thresholds

[detection]
n_plus_one_min_occurrences = 5
window_duration_ms = 500
slow_query_threshold_ms = 500
slow_query_min_occurrences = 3
max_fanout = 20
chatty_service_min_calls = 15
pool_saturation_concurrent_threshold = 10
serialized_min_sequential = 3
# Heuristique de récupération pour le SQL déjà paramétré : "auto", "strict",
# "always", "never". La barre de variance ci-dessous est ce qui sépare un vrai
# N+1 d'une répétition servie par le cache. La relever sur un runtime instable
# comme PHP-FPM, où les répétitions d'une même requête en cache dépassent 0,5
# et sont lues comme des N+1.
sanitizer_aware_classification = "auto"
sanitizer_aware_min_cv = 0.5

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
max_payload_size = 16777216
# Optionnel : activer le TLS sur les listeners gRPC et HTTP.
# Les deux champs doivent être renseignés ensemble (ou les deux absents pour TCP en clair).
# tls_cert_path = "/etc/tls/server-cert.pem"
# tls_key_path = "/etc/tls/server-key.pem"
api_enabled = true
max_retained_findings = 10000
max_retained_traces = 50
# Optionnel : régler les files bornées (valeurs par défaut affichées).
# Augmentez sous charge en rafales pour réduire la contre-pression
# d'ingestion / le délestage d'analyse.
ingest_queue_capacity = 1024
analysis_queue_capacity = 1024
# Optionnel : label `service` sur les findings et l'histogramme des spans
# lents (borné par les plafonds de cardinalité du daemon). false restaure
# la forme d'avant 0.18.
per_service_labels = true
# Optionnel : label `grouping` à côté de `service` sur les mêmes séries et
# sur les counters I/O par service (plafonné sur les paires (service,
# grouping) admises). false restaure la forme 0.18.
per_grouping_labels = true

# Optionnel : rejeter l'ingest OTLP quand la mémoire cgroup franchit ce
# pourcentage de la limite du conteneur, bornant la RSS contre l'OOM. 0
# désactive (défaut). Linux/cgroup-v2 uniquement. Réglez 80-85 pour garder
# de la marge au-dessus du régime permanent.
memory_high_water_pct = 0

# Optionnel : corrélation cross-trace (mode daemon uniquement)
# [daemon.correlation]
# enabled = true
# window_minutes = 10
# lag_threshold_ms = 2000
```

## Migration depuis 0.5.x

Huit clés top-level legacy ont été dépréciées en 0.5.26 et retirées en 0.6.0. Une configuration 0.5.x qui en utilise encore une échoue désormais au chargement avec un message de migration explicite, plutôt que de retomber silencieusement sur la valeur par défaut. Migrez vers la forme sectionnée ci-dessous avant la mise à jour.

| Retirée (top-level)    | Utiliser à la place          | Section       |
|------------------------|------------------------------|---------------|
| `n_plus_one_threshold` | `n_plus_one_min_occurrences` | `[detection]` |
| `window_duration_ms`   | `window_duration_ms`         | `[detection]` |
| `listen_addr`          | `listen_address`             | `[daemon]`    |
| `listen_port`          | `listen_port_http`           | `[daemon]`    |
| `max_active_traces`    | `max_active_traces`          | `[daemon]`    |
| `trace_ttl_ms`         | `trace_ttl_ms`               | `[daemon]`    |
| `max_events_per_trace` | `max_events_per_trace`       | `[daemon]`    |
| `max_payload_size`     | `max_payload_size`           | `[daemon]`    |

Exemple de migration. Avant (0.5.x) :

```toml
n_plus_one_threshold = 5
listen_port = 4318
max_payload_size = 2097152
```

Après (0.6.0+) :

```toml
[detection]
n_plus_one_min_occurrences = 5

[daemon]
listen_port_http = 4318
max_payload_size = 2097152
```

Le chargement d'un fichier 0.5.x sur 0.6.0 retourne une `ConfigError::Validation` dont le message nomme à la fois la clé retirée et son remplacement, donc un simple tail du flux d'erreur indique exactement quoi modifier.

## Variables d'environnement

Les fichiers de configuration ne doivent jamais contenir de secrets. Pour les valeurs sensibles (clés API, tokens), utilisez des variables d'environnement dans vos outils de déploiement. perf-sentinel en lit une liste fixe, chacune surchargeant le champ de config correspondant quand elle est définie et nettoyée d'un saut de ligne final, donc un Secret Kubernetes peut l'alimenter directement :

| Variable | Surcharge | Lue par |
|----------|-----------|---------|
| `PERF_SENTINEL_EMAPS_TOKEN` | l'`api_key` Electricity Maps | daemon et batch |
| `PERF_SENTINEL_ACK_API_KEY` | `[daemon.ack] api_key` | daemon et batch |
| `PERF_SENTINEL_INCIDENTS_API_KEY` | `[daemon.incidents] api_key` | daemon et batch |
| `PERF_SENTINEL_READ_API_KEY` | `[daemon] read_api_key` | daemon et batch |
| `PERF_SENTINEL_DAEMON_API_KEY` | le `--api-key-file` de `ack`, `query inspect`, `query monitor` et `query incidents` | CLI, envoyée en `X-API-Key` |
| `PERF_SENTINEL_DAEMON_URL` | l'URL `--daemon` des commandes `ack` et `query` | CLI |

Les surcharges s'appliquent à chaque exécution, commandes batch et exécution sans aucun fichier de config comprises, donc un job qui hérite du Secret du daemon doit lui aussi porter des valeurs valides (la clé d'incidents ne compte qu'une fois `[daemon.incidents] enabled` posé). Une variable définie à la chaîne vide compte comme définie : une clé vide est rejetée au chargement de la config plutôt qu'ignorée en silence, donc un Secret monté vide fait échouer toute commande qui la charge, daemon compris, au lieu d'ouvrir la route.

## Fichier d'acknowledgments

`.perf-sentinel-acknowledgments.toml` est un fichier séparé de `.perf-sentinel.toml`. Il vit à la racine du repo applicatif et liste les findings que l'équipe a acceptés comme connus. Les findings acquittés sont retirés de la sortie CLI (`analyze`, `report`, `inspect`, `diff`) et exclus de la quality gate.

Règles de chargement :

- Le chemin par défaut est `./.perf-sentinel-acknowledgments.toml` dans le répertoire courant. Override avec `--acknowledgments <chemin>`.
- Si le fichier n'existe pas, le run est un no-op (pas d'erreur, pas de bruit en sortie).
- `--no-acknowledgments` ignore le fichier complètement (vue d'audit).
- Une coquille dans `signature`, un champ requis manquant, ou un `expires_at` mal formé fait échouer le run de façon visible plutôt que d'élargir silencieusement l'ensemble acquitté.

Entry minimale :

```toml
[[acknowledged]]
signature = "redundant_sql:order-service:POST__api_orders:cafebabecafebabecafebabecafebabe"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-05-02"
reason = "Pattern d'invalidation de cache, intentionnel. Voir ADR-0042."
```

Le champ `expires_at = "YYYY-MM-DD"` est optionnel. L'omettre rend l'ack permanent. Le définir permet d'imposer une réévaluation périodique : quand la date passe, l'ack cesse de s'appliquer et le finding réapparaît au prochain run CI.

Pas de support glob ou wildcard, chaque entry est matchée contre une signature exacte. Les signatures sont émises sur chaque finding dans la sortie JSON, copiez-les dans le fichier plutôt que de recalculer le préfixe SHA-256 à la main.

Pour le workflow complet et la FAQ, voir [`ACKNOWLEDGMENTS-FR.md`](ACKNOWLEDGMENTS-FR.md).
