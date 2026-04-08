# Limitations connues et compromis

## Fiabilité de la capture OTLP

perf-sentinel est un **écouteur passif** : il reçoit les traces transmises par les SDKs ou collecteurs OpenTelemetry. Contrairement à un agent in-process (ex. Hypersistence Utils), il ne peut pas garantir la capture de chaque span. Des spans peuvent être perdus à cause de :

- Problèmes réseau entre l'application et perf-sentinel
- Échantillonnage configuré au niveau du SDK ou du collecteur
- Plantages de l'application avant le flush des spans

**Atténuation :** Pour les pipelines CI critiques, utilisez le mode batch (`perf-sentinel analyze`) avec des fichiers de traces pré-collectés plutôt que de dépendre de la capture en direct.

## Tokenizer SQL

Le normaliseur SQL utilise un tokenizer maison basé sur les regex plutôt qu'un parseur SQL complet. C'est intentionnel : cela maintient le binaire petit, évite les dépendances lourdes et fonctionne avec tous les dialectes SQL. Cependant, il a des limitations :

- **Pas d'analyse sémantique :** le tokenizer remplace les littéraux et UUIDs de manière positionnelle. Il ne construit pas d'AST et ne peut pas raisonner sur la structure de la requête.
- **Limite de longueur de requête :** les requêtes SQL dépassant 64 Ko sont tronquées à une frontière de caractère avant la normalisation. Cela empêche les allocations mémoire illimitées depuis des entrées adverses ou pathologiques.
- **CTEs :** les Common Table Expressions (`WITH ... AS (...)`) sont supportées -- le tokenizer normalise correctement les littéraux dans les CTEs, y compris les CTEs imbriquées.
- **Identifiants double-quoted :** les identifiants SQL standard entre guillemets doubles (`"MyTable"`, `"Column"`) sont préservés tels quels. Les chiffres dans les guillemets doubles ne sont pas confondus avec des littéraux numériques.
- **Chaînes dollar-quoted :** les chaînes dollar-quoted PostgreSQL (`$$body$$`, `$tag$body$tag$`) sont remplacées par des placeholders `?`, y compris dans les corps de fonctions.
- **Instructions `CALL` :** les paramètres littéraux dans `CALL` sont normalisés (`CALL process(42, 'rush')` devient `CALL process(?, ?)`). Les expressions SQL comme `NOW()`, `INTERVAL '...'` sont gérées (la chaîne dans `INTERVAL` est remplacée, l'appel de fonction est préservé).
- **Identifiants backtick :** les identifiants MySQL entre backticks (`` `table` ``) ne sont pas gérés spécifiquement. Ils passent tels quels sans provoquer d'erreur, mais les backticks restent dans le template.

Si vous rencontrez une requête mal normalisée, veuillez ouvrir une issue avec le SQL brut (anonymisé).

**Complémentarité avec pg_stat_statements :** perf-sentinel détecte les patterns par trace (N+1, appels redondants) que pg_stat_statements ne peut pas voir. Inversement, pg_stat_statements fournit des statistiques agrégées côté serveur (total d'appels, temps moyen) que perf-sentinel ne suit pas. Ils se complètent, utilisez les deux pour une visibilité complète.

## Paramètres bindés des ORM et classification N+1 vs redundant

Les ORM qui utilisent des paramètres nommés (Entity Framework Core avec `@__param_0`, Hibernate avec `?1`) produisent des spans SQL ou les valeurs des paramètres ne sont pas visibles dans l'attribut `db.statement`/`db.query.text`. perf-sentinel voit le template avec les placeholders mais pas les valeurs réelles.

Cela signifie que les patterns N+1 (même requête, valeurs différentes) peuvent être classifiés comme `redundant_sql` (même requête, mêmes params visibles) au lieu de `n_plus_one_sql` (même requête, params différents). Les deux findings identifient correctement le pattern de requêtes répétées et la suggestion de batcher ou cacher reste valide.

Les ORM qui injectent les valeurs littérales (SeaORM avec des requêtes brutes, JDBC sans prepared statements) produisent des spans avec des valeurs de paramètres visibles, permettant une classification précise N+1 vs redundant.

## Findings lents et ratio de gaspillage

Les findings lents (`slow_sql`, `slow_http`) représentent des opérations qui sont **nécessaires mais lentes** : ce ne sont pas des I/O évitables. Par conséquent, les findings lents ne contribuent **pas** au ratio de gaspillage I/O ni au compteur `avoidable_io_ops` dans le résumé GreenOps. Ils apparaissent tout de même dans la liste des findings avec `green_impact.estimated_extra_io_ops: 0`.

C'est un choix de conception : le ratio de gaspillage mesure combien d'I/O pourraient être éliminées (N+1, redondant), tandis que les findings lents mettent en évidence des opérations nécessitant une optimisation (indexation, cache) plutôt qu'une élimination.

## La détection de fanout nécessite `parent_span_id`

La détection de fanout (`excessive_fanout`) repose sur le champ `parent_span_id` pour construire les relations parent-enfant entre les spans. Si l'instrumentation de tracing ne propage pas les IDs de span parent (certains anciens SDKs OTel ou instrumentations personnalisées), la détection de fanout ne produira pas de findings.

Les findings de fanout, comme les findings lents, ne sont **pas** comptés comme des I/O évitables dans le ratio de gaspillage. Ils représentent un problème structurel (trop d'opérations enfants par parent) plutôt que des I/O éliminables.

## `rss_peak_bytes` sous Windows

La commande `perf-sentinel bench` rapporte le RSS pic (Resident Set Size) en utilisant des APIs spécifiques à la plateforme. Sous Windows, cette métrique est rapportée comme `null` car l'implémentation actuelle utilise `getrusage()` qui est spécifique à Unix. Les métriques de débit et de latence fonctionnent sur toutes les plateformes.

## Échantillonnage en mode daemon

Lorsque `sampling_rate` est défini en dessous de 1.0 dans la configuration `[daemon]`, perf-sentinel supprime aléatoirement des traces pour réduire l'utilisation des ressources. Cela signifie :

- Certains patterns N+1 ou redondants peuvent ne pas être détectés
- Le ratio de gaspillage est calculé uniquement sur les traces échantillonnées et peut ne pas représenter l'ensemble du trafic
- Les métriques Prometheus (`perf_sentinel_traces_analyzed_total`) ne reflètent que les traces échantillonnées

Pour une détection précise, utilisez `sampling_rate = 1.0` (le défaut) ou échantillonnez au niveau du collecteur où vous avez plus de contrôle.

## Nombre maximum d'événements par trace

En mode streaming, chaque trace contient au maximum `max_events_per_trace` événements (défaut : 1000) dans un buffer circulaire. Si une trace génère plus d'événements, les plus anciens sont supprimés. Cela peut causer :

- Des patterns N+1 manqués si les opérations répétées tombent en dehors de la fenêtre conservée
- Un sous-comptage des occurrences dans les findings

Pour les traces avec un très grand nombre d'événements, augmentez `max_events_per_trace` ou investiguer pourquoi une seule trace génère autant d'opérations.

## Taille du binaire

Le binaire release cible < 10 Mo avec `lto = "thin"`, `strip = true` et `panic = "abort"`. La table d'intensité carbone embarquée et le support protobuf OTLP contribuent à la taille du binaire. Si vous avez besoin d'un binaire plus petit et n'utilisez pas l'ingestion OTLP, la compilation avec des feature flags (travail futur) pourrait réduire la taille.

## Pas d'authentification ni de TLS

perf-sentinel n'implémente **pas** d'authentification ni de TLS sur ses endpoints d'ingestion (OTLP gRPC, OTLP HTTP, socket unix JSON, Prometheus `/metrics`). Par défaut, le daemon écoute sur `127.0.0.1` (loopback uniquement), ce qui est sûr pour les déploiements sur une seule machine.

Si vous exposez perf-sentinel sur un réseau :

- Placez-le derrière un reverse proxy qui gère le TLS et l'authentification
- Utilisez des politiques réseau (Kubernetes `NetworkPolicy`, isolation réseau Docker, règles de pare-feu) pour restreindre l'accès
- Acheminez les traces via un OpenTelemetry Collector avec ses propres extensions d'authentification et transmettez à perf-sentinel sur un réseau interne de confiance

N'exposez jamais perf-sentinel directement sur des réseaux non fiables sans couche de sécurité en amont.

## Précision des estimations carbone

perf-sentinel utilise un **modèle proxy I/O → énergie → CO₂** pour estimer l'empreinte carbone des charges de travail analysées. La chaîne comporte trois étapes, chacune introduisant une marge d'erreur :

1. **Opérations I/O → énergie** : chaque opération I/O détectée (requête SQL, appel HTTP) est multipliée par une constante fixe `ENERGY_PER_IO_OP_KWH` de `0,0000001 kWh` (~0,1 µWh). Cette valeur n'est **pas mesurée**, c'est une approximation d'ordre de grandeur.
2. **Énergie → CO₂** : l'énergie est multipliée par une intensité carbone réseau par région (gCO₂eq/kWh) issue d'Electricity Maps et Cloud Carbon Footprint (moyennes annuelles 2023-2024), avec un PUE par fournisseur (AWS 1,135, GCP 1,10, Azure 1,185, Generic 1,2).
3. **Carbone embodié (`M` dans SCI v1.0)** : émissions de fabrication matérielle amorties à un défaut configurable de `0,001 gCO₂/requête`. Indépendant de la région.

### Incertitude : multiplicative 2×, pas ±50%

Chaque estimation CO₂ est rapportée comme `{ low, mid, high }` où :

```
low  = mid × 0,5   (moitié du midpoint)
high = mid × 2,0   (double du midpoint)
```

C'est un **intervalle multiplicatif log-symétrique**, pas une fenêtre arithmétique ±50%. La moyenne géométrique de `low` et `high` est égale à `mid` ; la moyenne arithmétique ne l'est pas. Ce cadrage 2× est délibéré : le modèle proxy I/O a une incertitude d'ordre de grandeur (ENERGY_PER_IO_OP_KWH est plus approximatif que la moitié), donc une fenêtre ±50% symétrique sous-estimerait l'incertitude réelle du modèle. Interprétez les bornes comme "la valeur réelle est dans un facteur 2 de `mid`, dans un sens ou l'autre".

Les bornes reflètent l'incertitude agrégée du modèle, pas la variance par endpoint.

**Cet intervalle est un indicateur directionnel d'incertitude modèle, pas un intervalle de confiance statistique.** La valeur réelle sur des charges I/O atypiques (mix SQL + HTTP, lourds caches, moteurs de stockage custom) peut sortir de `[low, high]`. Utilisez la plage pour jauger la *plausibilité d'ordre de grandeur*, pas comme borne probabiliste.

### Sémantique SCI v1.0 : numérateur vs intensité

Le champ `co2.total` contient le **numérateur SCI v1.0** `(E × I) + M`, sommé sur toutes les traces analysées. Ce n'est **pas** le score d'intensité par requête que la spécification SCI définit comme "SCI". Pour obtenir l'intensité par requête, les consommateurs calculent :

```
sci_par_trace = co2.total.mid / analysis.traces_analyzed
```

Cette distinction compte : perf-sentinel rapporte une **empreinte** (émissions absolues), pas une **intensité** (émissions par unité fonctionnelle). Le champ `methodology` de chaque `CarbonEstimate` tague la sémantique :

- `co2.total.methodology = "sci_v1_numerator"` : l'empreinte `(E × I) + M` sur les traces analysées.
- `co2.avoidable.methodology = "sci_v1_operational_ratio"` : `operational × (avoidable_io_ops / accounted_io_ops)`, un ratio global aveugle à la région qui exclut le carbone embodié par design.

### Positionnement : compteur de gaspillage directionnel

perf-sentinel est un **compteur de gaspillage directionnel** conçu pour :

- **Détecter les anti-patterns de performance** (N+1, requêtes redondantes, fanout) et quantifier leur impact carbone relatif.
- **Comparer les exécutions** avant/après optimisation pour valider qu'un correctif réduit effectivement les I/O.
- **Détecter les régressions carbone** en CI comme garde-fou.

Ce n'est **PAS un outil de comptabilité carbone réglementaire**. **N'utilisez PAS** perf-sentinel pour :

- Le reporting CSRD (Corporate Sustainability Reporting Directive).
- Les déclarations GHG Protocol Scope 3.
- Des documents de conformité à valeur d'audit.
- Comparer des valeurs CO₂ absolues entre infrastructures différentes (le modèle suppose un profil serveur uniforme et moyen).
- Remplacer des données d'énergie réellement mesurées (RAPL, Scaphandre, wattmètres in-process).

### Ce qui fonctionne

| Cas d'usage                                      | Fiabilité                               |
|--------------------------------------------------|-----------------------------------------|
| Détecter le gaspillage (N+1, fanout, redondant)  | ✅ comptage déterministe                 |
| Comparer les exécutions (baseline vs. correctif) | ✅ deltas relatifs significatifs         |
| Classer les endpoints par impact relatif         | ✅ au sein d'un déploiement unique       |
| Garde-fous de régression carbone en CI           | ✅ via `[thresholds] io_waste_ratio_max` |
| CO₂ absolu pour rapports de conformité           | ❌ incertitude multiplicative 2×         |
| Comparaison cross-infrastructure                 | ❌ profil serveur uniforme supposé       |
| Remplacer l'énergie mesurée                      | ❌ proxy uniquement                      |

### Scoring multi-région

Quand les spans OTel portent l'attribut de ressource `cloud.region`, perf-sentinel répartit automatiquement les ops I/O par région et applique le bon coefficient d'intensité réseau. La chaîne de fallback est :

1. `event.cloud_region` depuis l'attribut OTel.
2. `[green.service_regions]` mapping config par service.
3. `[green] default_region`.

Les ops I/O sans région résolvable atterrissent dans un bucket synthétique `"unknown"` et contribuent à zéro CO₂ opérationnel (un `tracing::warn!` est émis). Le carbone embodié est tout de même émis car les émissions matérielles sont indépendantes de la région.

Voir `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md` pour la méthodologie complète, la formule et les notes d'alignement SCI v1.0.

### Profils carbone horaires

Des profils UTC 24h embarqués sont disponibles pour quatre régions disposant de patterns de réseau diurnes bien documentés :

- **France (`eu-west-3`, `fr`)** — baseload nucléaire, profil relativement plat avec un léger pic 17h-20h UTC.
- **Allemagne (`eu-central-1`, `de`)** — charbon + gaz + renouvelables variables, pics prononcés le matin (06h-10h UTC) et le soir (17h-20h UTC).
- **Royaume-Uni (`eu-west-2`, `gb`)** — éolien + gaz, pics jumeaux modérés similaires à l'Allemagne mais plus petits.
- **US-East (`us-east-1`)** — gaz + charbon, plateau diurne (13h-18h UTC = 9h-14h heure Est, heures de bureau).

Quand `[green] use_hourly_profiles = true` (le défaut), l'étape de scoring utilise l'intensité spécifique à l'heure pour chaque span basée sur son timestamp UTC. Les régions **non** listées ci-dessus utilisent toujours la valeur annuelle plate de la table carbone principale quel que soit le toggle. Les rapports où au moins une région a utilisé un profil horaire sont tagués `model = "io_proxy_v2"` (monté depuis `"io_proxy_v1"`), et chaque ligne de breakdown par région porte un champ `intensity_source` (`"annual"` ou `"hourly"`) pour que les consommateurs aval puissent auditer quelles régions ont bénéficié des données plus précises.

**Ce que ça fait et ne fait pas.** Le chemin horaire capture la variance au fil de la journée (un N+1 à 3h du matin en France coûte moins qu'un N+1 à 19h). Il ne capture PAS :

- **La variance saisonnière** — seul l'horaire est embarqué, pas mensuel×horaire. Les différences hiver/été sont moyennées dans le profil 24 valeurs.
- **Les fluctuations liées à la météo** — les valeurs embarquées sont des moyennes typiques, pas des données temps-réel.
- **Les régions hors du set de 4** — toutes les autres régions AWS/GCP/Azure et codes pays ISO retombent sur la valeur annuelle plate.

**Exigences de timestamp.** perf-sentinel parse les timestamps en UTC et exige la forme canonique ISO 8601 `YYYY-MM-DDTHH:MM:SS[.fff]Z` (Z final) ou la variante avec espace. Les chaînes avec offset non-UTC (`+02:00`, `-05:00`) sont rejetées plutôt que silencieusement décalées — la table carbone est ancrée UTC, donc un traitement naïf des offsets fausserait systématiquement l'estimation. Les spans avec timestamps non-parsables retombent sur l'intensité annuelle plate.

**Amélioration de précision (approximative).** Par rapport au modèle plat-annuel, les profils horaires réduisent la composante temps-de-jour du budget d'incertitude de ~±50% à ~±20% **pour les 4 régions listées uniquement**. L'intervalle d'incertitude multiplicative 2× global sur l'estimation CO₂ est inchangé, car la constante proxy énergie-par-op reste la source d'erreur dominante.

Pour figer les rapports sur le modèle annuel plat (ex. pour comparer des runs historiques sans le décalage horaire), mettre `[green] use_hourly_profiles = false` dans la config.

#### ⚠️ Le profil horaire Allemagne (`eu-central-1`) diverge de l'annuel plat

Contrairement à la France, au Royaume-Uni et aux US-East — dont les profils horaires restent dans les ±5% de leur valeur annuelle plate correspondante dans la table carbone principale — le profil horaire Allemagne a une **moyenne arithmétique de ~442 g/kWh**, alors que la valeur annuelle plate embarquée dans `CARBON_TABLE[eu-central-1]` est de **338 g/kWh** (écart d'environ 31%). Cela reflète les données ENTSO-E récentes (2023-2024) sur le réseau allemand, dominé par le charbon et les renouvelables variables avec des pics prononcés ; la valeur annuelle plate embarquée précède ce décalage et est optimiste par comparaison.

**Ce que ça signifie pour vos rapports :**

- Si vous lancez des rapports avec `default_region = "eu-central-1"` (ou n'importe quel span portant `cloud.region = eu-central-1`) et le défaut `use_hourly_profiles = true`, vous verrez des **chiffres CO₂ environ 31% plus élevés** qu'avant l'arrivée des profils horaires.
- Les nouveaux chiffres sont plus proches de la réalité que les anciens chiffres plats-annuels. **Nous ne recommandons PAS de figer les anciens chiffres**, sauf pour des raisons de rétrocompatibilité (ex. comparer un nouveau run à une baseline capturée avant les profils horaires).
- Si vous avez besoin de l'ancien comportement, mettez `[green] use_hourly_profiles = false` dans votre config. Ça désactive l'horaire pour toutes les régions, pas seulement l'Allemagne.
- Si vos quality gates CI (`[thresholds] io_waste_ratio_max` etc.) sont calibrés sur les anciens chiffres DE, vous devrez les recalibrer après l'upgrade.

La divergence est documentée inline dans `score/carbon.rs` pour que les futurs refreshs de données restent honnêtes sur le décalage. Un test de régression (`de_flat_annual_numerical_regression`) épingle la valeur annuelle plate pour qu'une édition accidentelle du profil DE ne puisse pas la corrompre silencieusement.

### Limites de précision Scaphandre

perf-sentinel embarque une intégration opt-in avec [Scaphandre](https://github.com/hubblo-org/scaphandre) pour la mesure énergétique par processus via les compteurs Intel RAPL. Quand `[green.scaphandre]` est configuré, le daemon `watch` scrape l'endpoint Prometheus Scaphandre toutes les quelques secondes et utilise les lectures de puissance mesurées pour remplacer la constante proxy `ENERGY_PER_IO_OP_KWH` fixe pour chaque service mappé.

**Exigences plateforme.** Scaphandre fonctionne sur :

- **Linux uniquement** (pas Windows, pas macOS, pas BSD).
- **CPU x86_64 Intel ou AMD avec support RAPL** — la plupart des puces serveur et desktop récentes, mais notamment **PAS ARM64**. Apple Silicon, Ampere, Graviton et instances cloud ARM similaires ne peuvent pas utiliser cette intégration.
- **Bare metal ou VMs avec passthrough RAPL.** La plupart des VMs cloud (AWS EC2, GCP GCE, Azure VMs) n'exposent **pas** les compteurs RAPL aux OS invités. Les pods Kubernetes s'exécutant sur des nœuds bare-metal peuvent accéder à RAPL si l'hôte expose `/sys/class/powercap/intel-rapl/` dans le conteneur (nécessite accès privilégié ou mount explicite).

Sur les plateformes non supportées, la section `[green.scaphandre]` est parsée et le scraper est lancé, mais il échouera à trouver l'endpoint et retombera silencieusement sur le modèle proxy. Une seule ligne de log au niveau warn est émise au premier échec pour que les opérateurs remarquent la mauvaise configuration.

**Ce que Scaphandre améliore.** L'intégration remplace le coefficient proxy fixe (0,1 µWh par op I/O) par une **valeur mesurée au niveau service** dérivée de la consommation réelle du processus mappé sur la fenêtre de scrape. Formule :

```
energy_per_op_kwh = (process_power_watts × scrape_interval_secs) / ops_in_window / 3_600_000
```

Ce qui capture :

- **La puissance processus réelle** (pas une approximation moyenne).
- **Les différences entre services** — Java vs .NET vs Node vs Go auront des empreintes énergétiques différentes même pour des charges I/O similaires.
- **La variance de charge dans le temps** — un service idle et un service en charge obtiennent des coefficients différents pendant que le daemon tourne.

Les rapports où au moins un service a utilisé un coefficient mesuré sont tagués `model = "scaphandre_rapl"` (priorité sur `"io_proxy_v2"` et `"io_proxy_v1"`).

**Ce que Scaphandre ne fait PAS.** C'est la limitation critique : **Scaphandre donne des coefficients par-service, pas d'attribution par-finding**. Spécifiquement :

1. **RAPL est au niveau processus, pas au niveau span.** La métrique `scaph_process_power_consumption_microwatts{exe="java"}` rapporte la consommation totale du processus `java`. Elle ne peut pas distinguer deux findings N+1 concurrents tournant dans le même processus au même moment — ils partagent le coefficient par construction.
2. **L'intervalle de scrape n'est PAS le goulot de précision.** Une fenêtre de 5 secondes moyenne la puissance sur 5 secondes. Passer à 1 seconde ne donnerait pas de précision par-finding parce que RAPL lui-même moyenne à la granularité du pas Scaphandre (~2s). Le plancher de précision réel est "un coefficient par (service, fenêtre_scrape)".
3. **Les services concurrents dans le même processus ne partagent rien.** Si votre architecture fait tourner plusieurs services logiques dans la même JVM, la lecture `exe="java"` de Scaphandre couvre tous ensemble. perf-sentinel attribue l'énergie mesurée au nom de service que vous avez mappé, ce qui est une simplification.
4. **Bruit de l'ordonnanceur OS.** L'attribution de puissance par processus via `process_cpu_time / total_cpu_time` est intrinsèquement bruitée sous charges mixtes.

**Modèle mental correct.** Scaphandre vous donne un **coefficient dynamique mesuré par service** au lieu d'une **constante proxy fixe et globale**. C'est une amélioration significative dans la couche d'attribution énergétique de la pile d'estimation carbone, mais cela ne transforme pas perf-sentinel en outil de comptabilité carbone grade-réglementaire. L'intervalle d'incertitude multiplicatif 2× s'applique toujours.

**Gestion de la fraîcheur.** Le daemon jette les entrées plus anciennes que 3× l'intervalle de scrape lors de la construction du snapshot par tick. Un scraper bloqué ou un service qui cesse d'émettre des événements retombera silencieusement sur le modèle proxy après ~3 intervalles de scrape. La jauge Prometheus `perf_sentinel_scaphandre_last_scrape_age_seconds` permet aux opérateurs de configurer des alertes Grafana sur la santé du scraper.

**Mode batch.** Le mode batch `analyze` ne lance jamais le scraper et n'utilise jamais les données Scaphandre. Même si `[green.scaphandre]` est présent dans la config, la commande `analyze` l'ignore entièrement et utilise toujours le modèle proxy. Seul le daemon `watch` intègre Scaphandre.

## Constante énergétique gCO2eq (section legacy, conservée pour les références croisées)

L'estimation carbone utilise une constante énergétique fixe (`0,1 uWh par opération I/O`) comme approximation d'ordre de grandeur. Voir **Précision des estimations carbone** ci-dessus pour la méthodologie complète et le disclaimer.

## Ingestion pg_stat_statements

- **Pas de corrélation par trace.** Les données `pg_stat_statements` n'ont pas de `trace_id` ni de `span_id`. Elles ne peuvent pas servir à la détection d'anti-patterns par trace (N+1, redondant). Elles fournissent une analyse complémentaire de hotspots et une référence croisée avec les findings basés sur les traces.
- **Parsing CSV.** Le parseur CSV gère le quoting RFC 4180 (champs entre guillemets doubles, `""` échappé), mais suppose une entrée UTF-8. Les fichiers non-UTF-8 échoueront au parsing.
- **Requêtes pré-normalisées.** PostgreSQL normalise les requêtes `pg_stat_statements` au niveau du serveur. perf-sentinel applique sa propre normalisation par-dessus pour la référence croisée, ce qui peut produire des templates légèrement différents.
- **Pas de connexion live.** perf-sentinel lit des fichiers CSV ou JSON exportés. Il ne se connecte pas directement à PostgreSQL.
- **Nombre d'entrées.** Le parseur pré-alloue la mémoire en fonction de la taille de l'entrée, plafonné à 100 000 entrées. Les fichiers dépassant 1 000 000 d'entrées (lignes CSV ou éléments de tableau JSON) sont rejetés avec une erreur pour prévenir l'épuisement mémoire.
