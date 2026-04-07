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

| Cas d'usage | Fiabilité |
|---|---|
| Détecter le gaspillage (N+1, fanout, redondant) | ✅ comptage déterministe |
| Comparer les exécutions (baseline vs. correctif) | ✅ deltas relatifs significatifs |
| Classer les endpoints par impact relatif | ✅ au sein d'un déploiement unique |
| Garde-fous de régression carbone en CI | ✅ via `[thresholds] io_waste_ratio_max` |
| CO₂ absolu pour rapports de conformité | ❌ incertitude multiplicative 2× |
| Comparaison cross-infrastructure | ❌ profil serveur uniforme supposé |
| Remplacer l'énergie mesurée | ❌ proxy uniquement |

### Scoring multi-région (Phase 5a)

Quand les spans OTel portent l'attribut de ressource `cloud.region`, perf-sentinel répartit automatiquement les ops I/O par région et applique le bon coefficient d'intensité réseau. La chaîne de fallback est :

1. `event.cloud_region` depuis l'attribut OTel.
2. `[green.service_regions]` mapping config par service.
3. `[green] default_region`.

Les ops I/O sans région résolvable atterrissent dans un bucket synthétique `"unknown"` et contribuent à zéro CO₂ opérationnel (un `tracing::warn!` est émis). Le carbone embodié est tout de même émis car les émissions matérielles sont indépendantes de la région.

Voir `docs/FR/design/05-GREENOPS-AND-CARBON-FR.md` pour la méthodologie complète, la formule et les notes d'alignement SCI v1.0.

## Constante énergétique gCO2eq (section legacy, conservée pour les références croisées)

L'estimation carbone utilise une constante énergétique fixe (`0,1 uWh par opération I/O`) comme approximation d'ordre de grandeur. Voir **Précision des estimations carbone** ci-dessus pour la méthodologie complète et le disclaimer.

## Ingestion pg_stat_statements

- **Pas de corrélation par trace.** Les données `pg_stat_statements` n'ont pas de `trace_id` ni de `span_id`. Elles ne peuvent pas servir à la détection d'anti-patterns par trace (N+1, redondant). Elles fournissent une analyse complémentaire de hotspots et une référence croisée avec les findings basés sur les traces.
- **Parsing CSV.** Le parseur CSV gère le quoting RFC 4180 (champs entre guillemets doubles, `""` échappé), mais suppose une entrée UTF-8. Les fichiers non-UTF-8 échoueront au parsing.
- **Requêtes pré-normalisées.** PostgreSQL normalise les requêtes `pg_stat_statements` au niveau du serveur. perf-sentinel applique sa propre normalisation par-dessus pour la référence croisée, ce qui peut produire des templates légèrement différents.
- **Pas de connexion live.** perf-sentinel lit des fichiers CSV ou JSON exportés. Il ne se connecte pas directement à PostgreSQL.
- **Nombre d'entrées.** Le parseur pré-alloue la mémoire en fonction de la taille de l'entrée, plafonné à 100 000 entrées. Les fichiers dépassant 1 000 000 d'entrées (lignes CSV ou éléments de tableau JSON) sont rejetés avec une erreur pour prévenir l'épuisement mémoire.
