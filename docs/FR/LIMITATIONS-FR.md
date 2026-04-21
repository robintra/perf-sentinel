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
- **CTEs :** les Common Table Expressions (`WITH ... AS (...)`) sont supportées, le tokenizer normalise correctement les littéraux dans les CTEs, y compris les CTEs imbriquées.
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

## Interprétation des scores

La CLI affiche un qualificatif `(healthy / moderate / high / critical)` à côté de `io_intensity_score` et `io_waste_ratio` et la même classification est émise dans le rapport JSON sous forme de champs siblings `io_intensity_band` et `io_waste_ratio_band`. Les tables de référence sont dans le README principal.

### Pourquoi ces seuils

- **IIS_MODERATE (2.0)** est une règle de pouce, pas empirique. Elle reflète l'intuition qu'un endpoint CRUD typique fait 1-2 opérations I/O par requête. Les agrégateurs, dashboards et générateurs de rapports verront beaucoup d'endpoints "moderate" qui sont des designs légitimes, pas des défauts.
- **IIS_HIGH (5.0)** est ancré sur `Config::default().n_plus_one_threshold = 5`. Un endpoint dont l'IIS atteint 5.0 est arithmétiquement au point où `detect_n_plus_one` commence à émettre des findings : d'où "high, à investiguer".
- **IIS_CRITICAL (10.0)** est ancré sur l'escalade de sévérité hard-codée `indices.len() >= 10` dans `crate::detect::n_plus_one`. Même nombre, même sémantique : si un finding atteint ce compte, il est tagué `Severity::Critical` par le détecteur et la band IIS au niveau endpoint indique que l'empreinte agrégée a franchi la même limite.
- **WASTE_RATIO_HIGH (0.30)** correspond à la valeur **par défaut** de `io_waste_ratio_max`. Si vous surchargez la quality gate dans votre `.perf-sentinel.toml`, l'interprétation CLI/JSON ne suit **pas** : la gate est une policy utilisateur, l'interprétation est une heuristique fixe. Ces deux dimensions sont indépendantes par design, sinon un utilisateur qui relâche la gate pour accepter un service legacy bruyant verrait l'interprétation se décaler silencieusement et manquerait le signal.
- **WASTE_RATIO_CRITICAL (0.50)** signale les runs où au moins la moitié de l'I/O analysée est du gaspillage évitable.

### Contrat de stabilité JSON

Les valeurs d'enum (`healthy`, `moderate`, `high`, `critical`) sont **stables entre versions**. Les consommateurs downstream (SARIF, Grafana, perf-lint, etc.) peuvent se brancher sur ces labels en toute sécurité.

Les **seuils numériques** qui déclenchent ces labels sont **versionnés avec le binaire**. Ils peuvent évoluer à mesure qu'on accumule des données d'usage réelles. Cela reflète le pattern existant où `co2.model` évolue de `io_proxy_v1 → v2 → v3` sans casser les consommateurs qui veulent juste savoir quel modèle a été utilisé.

Si un consommateur a besoin d'une classification indépendante de la version (par exemple, une alerte Grafana qui doit se comporter à l'identique à travers les upgrades de perf-sentinel), il doit lire les champs bruts `io_intensity_score` et `io_waste_ratio` et appliquer ses propres bandes.

### La sévérité par finding est documentée ailleurs

Pour les règles de sévérité par détecteur (`Critical` / `Warning` / `Info` sur N+1, Fanout, Slow, Chatty, Pool, Serialized), voir [`docs/FR/design/04-DETECTION-FR.md`](design/04-DETECTION-FR.md). Ces règles dépendent de seuils par détecteur partiellement config-tunables (par ex. `max_fanout × 3`, `chatty_service_min_calls × 3`) et sont documentées à côté des détecteurs eux-mêmes.

## La détection de fanout nécessite `parent_span_id`

La détection de fanout (`excessive_fanout`) repose sur le champ `parent_span_id` pour construire les relations parent-enfant entre les spans. Si l'instrumentation de tracing ne propage pas les IDs de span parent (certains anciens SDKs OTel ou instrumentations personnalisées), la détection de fanout ne produira pas de findings.

Les findings de fanout, comme les findings lents, ne sont **pas** comptés comme des I/O évitables dans le ratio de gaspillage. Ils représentent un problème structurel (trop d'opérations enfants par parent) plutôt que des I/O éliminables.

### Coefficients énergétiques par opération

Les multiplicateurs d'énergie par opération (pondération par verbe SQL, tiers de taille de payload HTTP) sont des estimations heuristiques dérivées de benchmarks académiques d'énergie SGBD (Xu et al. VLDB 2010, Tsirogiannis et al. SIGMOD 2010) et de la méthodologie Cloud Carbon Footprint. Les ratios relatifs entre opérations (SELECT < DELETE < INSERT/UPDATE) sont plus fiables que les valeurs absolues, qui varient selon les générations de matériel et les moteurs de bases de données.

Limitations principales :

- **Pas d'analyse de complexité de requête.** Un SELECT avec full table scan coûte plus d'énergie qu'un point lookup indexé, mais les deux reçoivent le même coefficient 0.5x.
- **La taille du payload HTTP nécessite des attributs OTel.** L'attribut `http.response.body.size` doit être présent sur les spans HTTP. Quand il est absent, le coefficient retombe à 1.0x.
- **Non utilisé avec l'énergie mesurée.** Quand Scaphandre ou cloud SPECpower fournit de l'énergie mesurée par service, les coefficients par opération sont ignorés.

Mettre `per_operation_coefficients = false` pour désactiver cette fonctionnalité.

### Énergie de transport réseau

Le terme optionnel d'énergie de transport réseau estime le coût énergétique du transfert d'octets entre régions. Le coefficient par défaut (0.04 kWh/Go) est le milieu de la fourchette 0.03-0.06 kWh/Go des études récentes (Mytton, Lunden & Malmodin, J. Industrial Ecology, 2024 ; Sustainable Web Design, 2024).

Limitations principales :

- **Large plage d'estimation.** Les valeurs publiées vont de 0.06 à 0.08 kWh/Go selon l'étude, l'année et le périmètre.
- **Pas d'effets CDN ou compression.** Les réseaux de distribution de contenu, la compression HTTP et la réutilisation de connexions ne sont pas modélisés.
- **Détection inter-région basée sur la config.** La région cible est déterminée en cherchant le hostname dans `[green.service_regions]`. Si le hostname n'est pas mappé, perf-sentinel suppose conservativement la même région.
- **Pas de modélisation du dernier kilomètre.** L'estimation couvre le transport backbone uniquement.
- **Hypothèse de proportionnalité linéaire.** Le modèle kWh/Go suppose que l'énergie augmente linéairement avec le volume de données. Mytton et al. (2024) montrent que c'est une simplification : les équipements réseau ont une puissance de base fixe significative indépendante du trafic. L'estimation est directionnelle, pas précise.
- **Corps de réponse uniquement.** Seule la taille du corps de réponse (`http.response.body.size`) est comptée. Le corps de requête (ex. payloads POST volumineux) n'est pas disponible dans les conventions sémantiques OTel HTTP standard et est exclu. Pour les APIs à écriture intensive, cela sous-estime l'énergie de transport.
- **Intensité réseau du caller.** L'infrastructure réseau est distribuée sur plusieurs grids, mais perf-sentinel utilise l'intensité carbone de la région du caller comme proxy. C'est une simplification connue, cohérente avec l'approche d'estimation directionnelle.

La fonctionnalité est désactivée par défaut (`include_network_transport = false`).

## Détection des services bavards (chatty service)

Le détecteur de services bavards ne compte que les spans HTTP sortants (`type: http_out`). Une trace avec 15 requêtes SQL vers la même base de données n'est pas "bavarde" au sens inter-services. Le seuil est par trace, pas par endpoint : une trace répartie sur 3 endpoints faisant chacun 6 appels (18 au total) déclenchera le seuil même si aucun endpoint individuel n'est particulièrement bavard.

Les findings de type chatty service ne sont PAS comptées comme I/O évitables dans le ratio de gaspillage. Elles représentent un problème architectural (granularité de décomposition des services), pas une opportunité de regroupement.

## Détection de saturation du pool de connexions

Le détecteur de saturation du pool utilise une heuristique basée sur le chevauchement temporel des spans SQL, pas les métriques réelles du pool de connexions. Il calcule la concurrence maximale en traitant chaque span SQL comme un intervalle `[début, début + durée]` et en exécutant un algorithme de balayage (sweep line).

Limitations :
- Les timestamps du tracing distribué peuvent présenter un décalage d'horloge, entraînant une détection imprécise du chevauchement.
- Le détecteur ne peut pas distinguer entre une contention réelle du pool et des requêtes parallèles intentionnelles (par exemple, des patterns scatter-gather).
- Pour un monitoring précis, instrumentez votre application avec les métriques OTel du pool de connexions (`db.client.connection.pool.usage`, `db.client.connection.pool.wait_time`).

Les findings de saturation du pool ne sont PAS comptées comme I/O évitables.

## Détection des appels sérialisés

Le détecteur d'appels sérialisés signale les spans frères séquentiels (même `parent_span_id`) qui appellent des services ou endpoints différents et pourraient potentiellement être exécutés en parallèle. La sévérité est `info` pour refléter l'incertitude inhérente.

Considérations sur les faux positifs :
- Des appels séquentiels au même service PEUVENT avoir des dépendances de données légitimes que l'outil ne peut pas observer (par exemple, "créer un utilisateur" puis "envoyer un email de bienvenue" où l'email a besoin de l'ID utilisateur).
- Le détecteur ignore les séquences où tous les appels partagent le même template normalisé (ce pattern est du N+1, pas de la sérialisation).
- Le champ `parent_span_id` doit être présent sur les spans pour que ce détecteur fonctionne. Les traces sans relations parent-enfant ne déclencheront pas de findings de sérialisation.

Le détecteur remonte au maximum un finding par span parent : la plus longue sous-séquence non chevauchante (trouvée par programmation dynamique). Si un parent contient deux groupes distincts d'appels sérialisables séparés par des spans chevauchants, seul le groupe le plus long est rapporté.

Les findings d'appels sérialisés ne sont PAS comptées comme I/O évitables. Elles représentent une opportunité d'optimisation de latence, pas une réduction d'I/O.

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

## Limites de longueur des champs à l'ingestion

Toutes les frontières d'ingestion (OTLP, JSON, Jaeger, Zipkin) tronquent les champs texte pour empêcher une croissance mémoire non bornée. Limites : `service` 256 octets, `operation` 256 octets, `target` 64 Ko, `source.endpoint` 512 octets, `source.method` 512 octets, `timestamp` 64 octets, `trace_id`/`span_id` 128 octets. La troncation préserve les frontières de caractères UTF-8. Les champs en dessous de la limite ne sont pas modifiés.

## Taille du binaire

Le binaire release cible < 10 Mo avec `lto = "thin"`, `strip = true` et `panic = "abort"`. La table d'intensité carbone embarquée et le support protobuf OTLP contribuent à la taille du binaire. Si vous avez besoin d'un binaire plus petit et n'utilisez pas l'ingestion OTLP, la compilation avec des feature flags (travail futur) pourrait réduire la taille.

## Dashboard HTML : guard formula-injection CSV

Chaque cellule des CSV exportés par le bouton **Export CSV** par onglet du dashboard HTML est vérifiée contre l'OWASP CSV injection. Si le premier caractère d'une cellule est `=`, `+`, `-`, `@`, ou une tabulation horizontale (`\t`), une apostrophe simple est préfixée pour qu'Excel, LibreOffice Calc et Google Sheets affichent le texte littéral plutôt que l'évaluer comme une formule à l'ouverture. Le préfixe est invisible dans la vue tableur et ne modifie pas la donnée pour les consommateurs qui parsent le CSV en texte brut. Les triggers ne sont neutralisés qu'en position 0, donc un template légitime comme `abc=def` s'exporte inchangé.

## Pas d'authentification (TLS disponible, auth non intégrée)

perf-sentinel n'implémente **pas** d'authentification sur ses endpoints d'ingestion. Par défaut, le daemon écoute sur `127.0.0.1` (loopback uniquement), ce qui est sûr pour les déploiements sur une seule machine.

**Le TLS est supporté** sur les listeners OTLP gRPC et HTTP via les champs de configuration `[daemon] tls_cert_path` et `tls_key_path`. Lorsque les deux sont renseignés, le daemon sert OTLP et `/metrics` en TLS. Le socket unix JSON et le scraping Prometheus `/metrics` ne sont pas configurables séparément : `/metrics` partage le port HTTP et hérite de son paramètre TLS. Voir [`docs/FR/CONFIGURATION-FR.md`](CONFIGURATION-FR.md) pour la référence complète.

Si vous exposez perf-sentinel sur un réseau :

- **Activez le TLS** via `tls_cert_path` et `tls_key_path` pour chiffrer le trafic en transit
- Utilisez des politiques réseau (Kubernetes `NetworkPolicy`, isolation réseau Docker, règles de pare-feu) pour restreindre l'accès
- Pour l'**authentification**, placez perf-sentinel derrière un reverse proxy (nginx, envoy) qui gère les tokens bearer ou les certificats client mTLS
- Acheminez les traces via un OpenTelemetry Collector avec ses propres extensions d'authentification et transmettez à perf-sentinel sur un réseau interne de confiance

N'exposez jamais perf-sentinel directement sur des réseaux non fiables sans au minimum le TLS activé et des contrôles d'accès réseau en place.

### Durcissement du socket JSON

Le socket unix JSON (`[daemon] json_socket`) se défend contre les attaques locales sur un hôte multi-utilisateurs avec deux mécanismes :

- **Permissions `0o600`** appliquées juste après `bind()`. Les autres utilisateurs locaux ne peuvent pas se connecter pour injecter des événements.
- **Pré-vérification des symlinks** : avant que le daemon ne supprime un éventuel fichier socket résiduel, il appelle `symlink_metadata()` et refuse de continuer si le chemin est un lien symbolique. Cela empêche un attaquant local qui contrôle le répertoire parent du socket de faire pointer `json_socket` vers un fichier victime (par exemple `/etc/passwd`) et de laisser le `remove_file()` de démarrage du daemon le supprimer.

Ces deux défenses ne comptent que si `json_socket` se trouve dans un répertoire accessible en écriture par d'autres utilisateurs locaux. Si vous placez le socket dans un répertoire appartenant au daemon (`/var/run/perf-sentinel/` avec `0o700`), la surface est déjà fermée au niveau du système de fichiers.

### Budget payload par connexion sur le socket JSON

`[daemon] max_payload_size` (défaut 1 Mio) cape les batches NDJSON individuels soumis au socket JSON. Une seule connexion peut streamer plusieurs batches avant de se fermer et le daemon tolère jusqu'à **16× `max_payload_size`** par connexion avant de tronquer le flux. Avec les valeurs par défaut, cela veut dire qu'une connexion peut transférer jusqu'à 16 Mio de données de traces.

Le facteur est intentionnel : il accommode les clients qui émettent beaucoup de petits batches sur une connexion longue durée (par exemple un sidecar qui vide une file d'attente bufferisée après un flush), sans exposer le daemon à une exhaustion mémoire depuis un attaquant. Un client qui a besoin de plus de 16× la taille de batch configurée doit ouvrir une nouvelle connexion. Le cap ne peut pas être désactivé.

### Cap de concurrence sur les handshakes TLS

Chaque listener TLS (OTLP gRPC et OTLP HTTP) limite à **128** les handshakes en vol et les connexions HTTPS actives simultanées. Les handshakes tournent dans des tasks dédiées pour qu'un seul pair qui stalle ne bloque pas la boucle d'accept et le cap borne les fds, les buffers rustls et les slots de tasks face à un flood de handshakes. Un timeout de 10s (`TLS_HANDSHAKE_TIMEOUT`) coupe les pairs qui terminent le TCP sans envoyer de `ClientHello`. Le cap n'est pas configurable, il est aligné sur le budget du socket JSON Unix.

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

Des profils d'intensité carbone horaire UTC sont embarqués pour plus de 30 régions cloud couvrant tous les principaux fournisseurs et zones géographiques. Quatre régions (FR, DE, GB, US-East) ont des profils **mois x heure** (288 valeurs chacun) qui capturent la variation saisonnière. Les autres régions ont des profils **annuels plats** (24 valeurs, même forme toute l'année).

**Régions mois x heure** (12 mois x 24 heures) :

- **France (`eu-west-3`)** : baseload nucléaire avec pic de gaz hivernal. Plus haute intensité en hiver, plus basse en été.
- **Allemagne (`eu-central-1`)** : charbon + renouvelables. Forte variance saisonnière : utilisation du charbon significativement accrue en hiver.
- **Royaume-Uni (`eu-west-2`)** : éolien + gaz. L'hiver a plus de chauffage au gaz, l'été plus d'éolien.
- **US-East (`us-east-1`)** : gaz + charbon. La climatisation estivale et le chauffage hivernal poussent l'intensité au-dessus du printemps/automne.

**Régions horaires annuelles plates** (profil 24h, même toute l'année) :

- **Europe (ENTSO-E)** : Irlande (`eu-west-1`), Pays-Bas (`eu-west-4`), Suède (`eu-north-1`), Belgique (`europe-west1`), Finlande (`europe-north1`), Italie (`eu-south-1`), Espagne (`europe-southwest1`), Pologne (`europe-central2`), Norvège (`europe-north2`).
- **Amériques (EIA / IESO / ONS)** : US Ohio (`us-east-2`), US N. Californie (`us-west-1`), US Oregon (`us-west-2`), Canada Québec (`ca-central-1`), Brésil (`sa-east-1`).
- **Asie-Pacifique (best-effort)** : Japon (`ap-northeast-1`), Singapour (`ap-southeast-1`), Inde (`ap-south-1`), Australie (`ap-southeast-2`).

Les alias de codes pays (`fr`, `de`, `gb`, `ie`, `se`, `no`, `jp`, `br`, etc.) et synonymes fournisseurs cloud (`westeurope`, `northeurope`, `uksouth`, `francecentral`, etc.) sont supportés et résolvent vers le même profil.

Quand `[green] use_hourly_profiles = true` (le défaut), l'étape de scoring utilise l'intensité spécifique à l'heure (et au mois quand disponible) pour chaque span basée sur son timestamp UTC. Les régions sans profil utilisent toujours la valeur annuelle plate. Les rapports sont tagués `model = "io_proxy_v3"` (mois x heure), `"io_proxy_v2"` (horaire annuel plat) ou `"io_proxy_v1"` (annuel) et chaque ligne de breakdown par région porte un champ `intensity_source` (`"annual"`, `"hourly"` ou `"monthly_hourly"`).

**Ce que ça fait et ne fait pas.** Le chemin horaire capture la variance au fil de la journée (un N+1 à 3h du matin en France coûte moins qu'un N+1 à 19h). Les profils mois x heure capturent aussi la variance saisonnière pour les 4 régions listées. Il ne capture PAS :

- **Les fluctuations liées à la météo** : les valeurs embarquées sont des moyennes typiques, pas des données temps-réel.
- **Les données en temps réel** : les profils embarqués sont statiques. Pour l'intensité carbone en temps réel (marquée `intensity_source = "real_time"` dans les rapports), activer l'intégration opt-in `[green.electricity_maps]` en mode daemon, voir `docs/FR/CONFIGURATION-FR.md`.

**Profils estimés.** Les profils Asie-Pacifique et Brésil sont estimés à partir de la composition du mix de combustibles plutôt que de données horaires de génération. Ils sont annotés comme tels dans le code source.

**Exigences de timestamp.** perf-sentinel parse les timestamps en UTC et exige la forme canonique ISO 8601 `YYYY-MM-DDTHH:MM:SS[.fff]Z` (Z final) ou la variante avec espace. Les chaînes avec offset non-UTC (`+02:00`, `-05:00`) sont rejetées plutôt que silencieusement décalées, car la table carbone est ancrée UTC et un traitement naïf des offsets fausserait systématiquement l'estimation. Les spans avec timestamps non-parsables retombent sur l'intensité annuelle plate.

**Amélioration de précision (approximative).** Par rapport au modèle plat-annuel, les profils horaires réduisent la composante temps-de-jour du budget d'incertitude de ~±50% à ~±20% **pour les 4 régions listées uniquement**. L'intervalle d'incertitude multiplicative 2× global sur l'estimation CO₂ est inchangé, car la constante proxy énergie-par-op reste la source d'erreur dominante.

Pour figer les rapports sur le modèle annuel plat (ex. pour comparer des runs historiques sans le décalage horaire), mettre `[green] use_hourly_profiles = false` dans la config.

#### ⚠️ Le profil horaire Allemagne (`eu-central-1`) diverge de l'annuel plat

Contrairement à la France, au Royaume-Uni et aux US-East, dont les profils horaires restent dans les ±5% de leur valeur annuelle plate correspondante dans la table carbone principale, le profil horaire Allemagne a une **moyenne arithmétique de ~442 g/kWh**, alors que la valeur annuelle plate embarquée dans `CARBON_TABLE[eu-central-1]` est de **338 g/kWh** (écart d'environ 31%). Cela reflète les données ENTSO-E récentes (2023-2024) sur le réseau allemand, dominé par le charbon et les renouvelables variables avec des pics prononcés ; la valeur annuelle plate embarquée précède ce décalage et est optimiste par comparaison.

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
- **CPU x86_64 Intel ou AMD avec support RAPL** : la plupart des puces serveur et desktop récentes, mais notamment **PAS ARM64**. Apple Silicon, Ampere, Graviton et instances cloud ARM similaires ne peuvent pas utiliser cette intégration.
- **Bare metal ou VMs avec passthrough RAPL.** La plupart des VMs cloud (AWS EC2, GCP GCE, Azure VMs) n'exposent **pas** les compteurs RAPL aux OS invités. Les pods Kubernetes s'exécutant sur des nœuds bare-metal peuvent accéder à RAPL si l'hôte expose `/sys/class/powercap/intel-rapl/` dans le conteneur (nécessite accès privilégié ou mount explicite).

Sur les plateformes non supportées, la section `[green.scaphandre]` est parsée et le scraper est lancé, mais il échouera à trouver l'endpoint et retombera silencieusement sur le modèle proxy. Une seule ligne de log au niveau warn est émise au premier échec pour que les opérateurs remarquent la mauvaise configuration.

**Ce que Scaphandre améliore.** L'intégration remplace le coefficient proxy fixe (0,1 µWh par op I/O) par une **valeur mesurée au niveau service** dérivée de la consommation réelle du processus mappé sur la fenêtre de scrape. Formule :

```
energy_per_op_kwh = (process_power_watts × scrape_interval_secs) / ops_in_window / 3_600_000
```

Ce qui capture :

- **La puissance processus réelle** (pas une approximation moyenne).
- **Les différences entre services** : Java vs .NET vs Node vs Go auront des empreintes énergétiques différentes même pour des charges I/O similaires.
- **La variance de charge dans le temps** : un service idle et un service en charge obtiennent des coefficients différents pendant que le daemon tourne.

Les rapports où au moins un service a utilisé un coefficient mesuré sont tagués `model = "scaphandre_rapl"`. Chaîne de priorité complète : `electricity_maps_api` > `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`. Quand des facteurs de calibration sont actifs sur les modèles proxy, le suffixe `+cal` est ajouté (ex. `io_proxy_v2+cal`).

**Ce que Scaphandre ne fait PAS.** C'est la limitation critique : **Scaphandre donne des coefficients par service, pas d'attribution par finding**. Spécifiquement :

1. **RAPL est au niveau processus, pas au niveau span.** La métrique `scaph_process_power_consumption_microwatts{exe="java"}` rapporte la consommation totale du processus `java`. Elle ne peut pas distinguer deux findings N+1 concurrents tournant dans le même processus au même moment : ils partagent le coefficient par construction.
2. **L'intervalle de scrape n'est PAS le goulot de précision.** Une fenêtre de 5 secondes moyenne la puissance sur 5 secondes. Passer à 1 seconde ne donnerait pas de précision par finding parce que RAPL lui-même moyenne à la granularité du pas Scaphandre (~2s). Le plancher de précision réel est "un coefficient par (service, fenêtre_scrape)".
3. **Les services concurrents dans le même processus ne partagent rien.** Si votre architecture fait tourner plusieurs services logiques dans la même JVM, la lecture `exe="java"` de Scaphandre couvre tous ensemble. perf-sentinel attribue l'énergie mesurée au nom de service que vous avez mappé, ce qui est une simplification.
4. **Bruit de l'ordonnanceur OS.** L'attribution de puissance par processus via `process_cpu_time / total_cpu_time` est intrinsèquement bruitée sous charges mixtes.

**Modèle mental correct.** Scaphandre vous donne un **coefficient dynamique mesuré par service** au lieu d'une **constante proxy fixe et globale**. C'est une amélioration significative dans la couche d'attribution énergétique de la pile d'estimation carbone, mais cela ne transforme pas perf-sentinel en outil de comptabilité carbone grade-réglementaire. L'intervalle d'incertitude multiplicatif 2× s'applique toujours.

**Gestion de la fraîcheur.** Le daemon jette les entrées plus anciennes que 3× l'intervalle de scrape lors de la construction du snapshot par tick. Un scraper bloqué ou un service qui cesse d'émettre des événements retombera silencieusement sur le modèle proxy après ~3 intervalles de scrape. La jauge Prometheus `perf_sentinel_scaphandre_last_scrape_age_seconds` permet aux opérateurs de configurer des alertes Grafana sur la santé du scraper.

**Mode batch.** Le mode batch `analyze` ne lance jamais le scraper et n'utilise jamais les données Scaphandre. Même si `[green.scaphandre]` est présent dans la config, la commande `analyze` l'ignore entièrement et utilise toujours le modèle proxy. Seul le daemon `watch` intègre Scaphandre.

### Limites de précision du cloud SPECpower

perf-sentinel embarque une intégration opt-in pour l'estimation d'énergie cloud-native via utilisation CPU% + interpolation SPECpower. Quand `[green.cloud]` est configuré, le daemon `watch` scrape les métriques CPU% depuis un endpoint Prometheus et les combine avec une table de lookup embarquée (watts idle/max par type d'instance, issue des données SPECpower de Cloud Carbon Footprint) pour estimer la consommation énergétique par service. Supporte AWS, GCP et Azure.

**Exigences plateforme.** L'intégration cloud nécessite :

- **Un endpoint Prometheus/VictoriaMetrics accessible** exposant les métriques d'utilisation CPU pour les services cibles (ex. `container_cpu_usage_seconds_total` via cAdvisor, `CPUUtilization` via cloudwatch_exporter ou équivalent GCP/Azure).
- **Un mapping type d'instance → watts** dans la table embarquée. La table couvre les types d'instance courants AWS (c5, m5, r5, c6g, m6g, etc.), GCP (n2-standard, e2, c2, etc.) et Azure (Standard_D, Standard_E, Standard_F, etc.). Les types inconnus retombent sur un défaut au niveau fournisseur.

Sur les instances non supportées ou quand le endpoint Prometheus est inaccessible, le scoring retombe silencieusement sur le modèle proxy.

**Ce que ça améliore.** L'intégration cloud remplace le coefficient proxy fixe par une **valeur dérivée de l'utilisation CPU réelle** interpolée entre la puissance idle et maximale de l'instance. Formule :

```
watts = idle_watts + (max_watts - idle_watts) × cpu_utilization
energy_per_op_kwh = (watts × scrape_interval_secs) / ops_in_window / 3_600_000
```

Ce qui capture :

- **L'utilisation CPU réelle du service** (pas une constante fixe).
- **Les caractéristiques de l'instance** : un `c5.4xlarge` (16 vCPUs, 32 GiB) a un profil énergétique différent d'un `m5.xlarge` (4 vCPUs, 16 GiB).
- **La variance de charge dans le temps** : un service au repos et un service en charge obtiennent des coefficients différents pendant que le daemon tourne.

Les rapports où au moins un service a utilisé l'estimation cloud sont tagués `model = "cloud_specpower"` (priorité : `electricity_maps_api` > `scaphandre_rapl` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`).

**Ce que ça ne fait PAS.** Comme Scaphandre, le modèle cloud SPECpower donne des coefficients par service, pas d'attribution par finding. De plus :

1. **L'interpolation SPECpower est linéaire.** La consommation réelle d'un serveur n'est pas parfaitement linéaire entre idle et max. La précision résultante est d'environ **+/-30%**, meilleure que le proxy (~facteur 2) mais nettement moins précise que les mesures RAPL directes de Scaphandre.
2. **Le CPU n'est pas le seul consommateur d'énergie.** La mémoire, le réseau et le stockage contribuent à la consommation totale mais ne sont pas capturés par ce modèle.
3. **Les VMs partagées faussent les lectures.** Sur des instances partagées (burstable comme `t3`, `e2-micro`), l'utilisation CPU visible ne reflète pas nécessairement la consommation réelle au niveau de l'hôte physique.
4. **La table de lookup vieillit.** Les nouvelles générations d'instances nécessitent des mises à jour de la table embarquée. Les types d'instance inconnus retombent sur un profil générique du fournisseur.

**Modèle mental correct.** Le modèle cloud SPECpower vous donne un **coefficient dynamique par service basé sur l'utilisation CPU réelle** au lieu d'une **constante proxy fixe globale**. C'est une amélioration significative pour les déploiements cloud où Scaphandre n'est pas disponible (la plupart des VMs cloud n'exposent pas RAPL). L'intervalle d'incertitude passe d'un facteur ~2× (proxy) à environ +/-30% (SPECpower), mais l'outil reste un compteur de gaspillage directionnel, pas un instrument de comptabilité carbone.

**Mode batch.** Le mode batch `analyze` ne lance jamais le scraper Prometheus et n'utilise jamais les données cloud. Même si `[green.cloud]` est présent dans la config, la commande `analyze` l'ignore entièrement et utilise toujours le modèle proxy. Seul le daemon `watch` intègre l'estimation cloud.

## Corrélation cross-trace

La corrélation temporelle cross-trace (`[daemon.correlation]`) nécessite le mode daemon (`perf-sentinel watch`) avec un trafic soutenu et représentatif. Les corrélations sont statistiques : elles détectent des co-occurrences temporelles, pas des relations causales. Une corrélation élevée entre un N+1 dans le service A et une saturation du pool dans le service B signifie qu'ils co-surviennent fréquemment dans le délai configuré, pas que l'un cause l'autre.

Limitations :

- **Démarrage à froid.** Le corrélateur a besoin de temps pour accumuler suffisamment d'observations. Avec `min_co_occurrences = 3` et une fenêtre de 10 minutes, il faut au moins 3 co-occurrences en 10 minutes avant qu'une corrélation remonte. Les environnements à faible trafic peuvent ne jamais atteindre ce seuil.
- **Mode batch non supporté.** La commande `analyze` ne lance pas le corrélateur. La corrélation cross-trace est intrinsèquement une préoccupation du streaming.
- **Cardinalité.** Le plafond `max_tracked_pairs` (défaut 1000) empêche la croissance mémoire non bornée. Si vous avez de nombreux types de findings distincts sur de nombreux services, certaines paires peuvent être évincées avant d'atteindre le seuil de co-occurrences.

## Attributs de code source OTel

Les findings incluent un champ `code_location` (avec `function`, `filepath`, `lineno`, `namespace`) quand les spans OTel portent les attributs `code.*` correspondants. Cela permet des annotations au niveau source dans les rapports SARIF (annotations inline GitHub/GitLab).

Limitations :

- **La plupart des agents d'auto-instrumentation OTel n'émettent pas `code.lineno` ou `code.filepath`.** Une instrumentation manuelle ou une configuration spécifique de l'agent est nécessaire. Sans ces attributs, les findings apparaissent sans localisation source (pas de bruit, dégradation gracieuse).
- **`code.function` est l'attribut le plus souvent disponible.** Si seul `code.function` est présent, la CLI l'affiche mais SARIF ne peut pas produire de `physicalLocation` (qui nécessite au minimum un chemin de fichier).
- **Les numéros de ligne peuvent être approximatifs.** Certains agents rapportent le point d'entrée de la méthode, pas la ligne exacte de l'appel I/O.
- **Les valeurs `code.filepath` hostiles sont supprimées du SARIF.** L'attribut OTel `code.filepath` est contrôlé par le client. Avant émission comme `artifactLocation.uri` SARIF, perf-sentinel rejette les chaînes de type URI, les chemins absolus, le path traversal (littéral et percent-encodé), les séquences double-encodées, les préfixes UTF-8 overlong, les caractères de contrôle et les caractères Unicode BiDi/invisibles (classe Trojan Source). Les findings au filepath rejeté apparaissent toujours dans le rapport, sans `physicalLocations`.

## API de requêtage du daemon

La sous-commande `perf-sentinel query` et les endpoints HTTP `/api/*` exposent l'état interne du daemon. L'API de requêtage n'a pas d'authentification ni d'autorisation intégrée. Le contrôle d'accès doit être géré extérieurement via des politiques réseau ou un reverse proxy, comme pour les endpoints d'ingestion OTLP. Voir "Pas d'authentification" ci-dessus.

- **Kill-switch.** Mettre `[daemon] api_enabled = false` désactive toutes les routes `/api/*` tout en conservant l'ingestion OTLP et `/metrics`. Utilisez cette option quand le daemon tourne dans un environnement où même l'exposition en loopback des findings est inacceptable. Notez que `/metrics` expose toujours les compteurs de findings via `perf_sentinel_findings_total` et métriques associées : le flag de l'API ne supprime donc pas toute sortie observable.
- **La mémoire n'est pas libérée par `api_enabled = false` seul.** Le buffer circulaire `FindingsStore` est toujours peuplé à chaque tick même quand l'API est désactivée, car la détection tourne avant la vérification de l'API. Pour libérer cette mémoire, mettez `[daemon] max_retained_findings = 0`. Cela court-circuite le `push_batch` du store et garde le RSS du daemon minimal quand l'API de requêtage est désactivée.
- **Taille de réponse plafonnée.** `/api/findings` plafonne à 1000 entrées par requête (le paramètre `?limit=` est tronqué). `/api/correlations` tronque au top 1000 par confiance. Ces plafonds protègent contre les requêtes coûteuses quand le daemon a accumulé une grosse empreinte mémoire.
- **Les findings retenus sont bornés.** Le buffer circulaire `FindingsStore` (défaut 10 000 findings) évince les entrées les plus anciennes quand il est plein. Pour les daemons à fort trafic, augmentez `max_retained_findings` ou acceptez que les findings plus anciens ne seront pas interrogeables.
- **Pas de persistance.** Le daemon stocke les findings en mémoire uniquement. Un redémarrage efface tous les findings retenus et l'état de corrélation. Pour investiguer des traces plus anciennes que la fenêtre live de 30 secondes (incidents de production regardés après coup), voir [RUNBOOK-FR.md](RUNBOOK-FR.md).

## Ingestion automatisée pg_stat depuis Prometheus

Le flag `--prometheus` de `pg-stat` scrape les métriques exposées par `postgres_exporter`. Cela nécessite :

- Une instance `postgres_exporter` en cours d'exécution configurée pour collecter les métriques `pg_stat_statements`.
- L'endpoint Prometheus doit être joignable depuis la machine exécutant perf-sentinel.
- Seules les métriques disponibles dans l'exporteur Prometheus sont utilisées. Certains champs présents dans la vue `pg_stat_statements` brute (ex. `blk_read_time`, `blk_write_time`) peuvent ne pas être exposés par toutes les versions de l'exporteur.

Le mode `--input` par fichier existant est inchangé et reste l'approche recommandée pour les pipelines CI.

## Secrets et credentials

perf-sentinel ne stocke jamais de secrets dans sa sortie de config. Pour les scrapers qui ont besoin de credentials, le pattern "variable d'environnement préférée" s'applique partout :

- **Clé API Electricity Maps** : variable `PERF_SENTINEL_EMAPS_TOKEN`. Un `[green.electricity_maps] api_key` dans le fichier de config fonctionne mais émet un warning au chargement, car les fichiers de config committés sont une source fréquente de fuites de credentials.
- **Connection string PostgreSQL** pour `pg-stat --connection-string` : variable `PERF_SENTINEL_PG_CONNECTION`. Passer une connection string avec mot de passe en clair sur la CLI fonctionne aussi mais émet un warning (recommandé : `.pgpass` en production).
- **URLs d'endpoints scraper** (Scaphandre, cloud energy, Electricity Maps, pg-stat Prometheus) : les credentials dans l'URL (`http://user:pass@host`) sont rejetés au chargement de la config. Utilisez le mécanisme d'authentification natif du scraper.
- **Fichier de clé TLS** : les permissions de `[daemon] tls_key_path` sont vérifiées au démarrage ; une clé lisible par le groupe ou les autres émet un warning.

Le daemon n'écrit jamais de secrets sur stdout/stderr : tous les chemins d'erreur des scrapers utilisent `redact_endpoint` pour retirer les userinfo de toute URL avant de logger.

Quand le daemon tourne avec `api_enabled = true`, l'API de requêtage expose les findings (pas les secrets) mais sans authentification. Restreignez l'accès loopback via des politiques réseau ou un reverse proxy ou mettez `api_enabled = false` pour désactiver toute la surface API.

## API Electricity Maps

- **Clé API requise.** L'intégration Electricity Maps nécessite une clé API (tier gratuit ou payant). La clé doit être fournie via la variable `PERF_SENTINEL_EMAPS_TOKEN` plutôt que dans le fichier de config.
- **HTTPS fortement recommandé.** Quand l'endpoint configuré est `http://` (cleartext) et qu'un auth token est défini, perf-sentinel émet un warning au chargement de la config. L'API production d'Electricity Maps est servie uniquement en HTTPS ; un endpoint `http://` est presque toujours une erreur de configuration ou un setup de test local.
- **Limites de débit.** Le tier gratuit permet environ 30 requêtes par mois par zone. Avec le `poll_interval_secs = 300` par défaut, ce budget serait épuisé en moins de 3 heures. Les utilisateurs du tier gratuit doivent utiliser `poll_interval_secs = 3600` ou plus.
- **Mode daemon uniquement.** Le scraper Electricity Maps ne fonctionne qu'en mode `perf-sentinel watch`.
- **Repli en cas de staleness.** Si l'API est inaccessible plus longtemps que 3x l'intervalle de sondage, le scraper retombe sur les profils horaires embarqués.

## Ingestion Tempo

- **Format protobuf.** La sous-commande `perf-sentinel tempo` demande les traces en protobuf OTLP depuis l'API HTTP de Tempo.
- **Plafond de concurrence sur le fetch parallèle.** Le flow search-then-fetch (`--service --lookback`) récupère les corps de trace en parallèle via un `tokio::task::JoinSet`, capé à 16 requêtes in-flight par un sémaphore interne. Le cap n'est pas configurable par l'utilisateur aujourd'hui. Timeout par fetch à 30s (vs. 5s pour l'étape search) pour laisser la query-frontend assembler un trace à fort fan-out depuis ingesters + stockage long terme. Sur un Tempo sous-dimensionné avec des fenêtres longues (24h par exemple), certains fetches peuvent malgré tout timeout. Le remède est côté Tempo : scaler `tempo-query-frontend`, ajuster `max_search_duration` et `max_concurrent_queries`.
- **Ctrl-C préserve les résultats partiels.** Interrompre un fetch parallèle long abort toutes les tasks in-flight et retourne les traces déjà complétées. La CLI renvoie l'erreur dédiée `TempoError::Interrupted` si zéro trace n'a eu le temps de se compléter avant le signal, pour que les quality gates CI distinguent un abort opérateur d'un vrai résultat vide (`NoTracesFound`).
- **API de recherche.** Le mode recherche utilise l'endpoint `GET /api/search` de Tempo, qui doit être activé dans la configuration Tempo.

## Constante énergétique gCO2eq (section legacy, conservée pour les références croisées)

L'estimation carbone utilise une constante énergétique fixe (`0,1 uWh par opération I/O`) comme approximation d'ordre de grandeur. Voir **Précision des estimations carbone** ci-dessus pour la méthodologie complète et le disclaimer.

## Ingestion pg_stat_statements

- **Pas de corrélation par trace.** Les données `pg_stat_statements` n'ont pas de `trace_id` ni de `span_id`. Elles ne peuvent pas servir à la détection d'anti-patterns par trace (N+1, redondant). Elles fournissent une analyse complémentaire de hotspots et une référence croisée avec les findings basés sur les traces.
- **Parsing CSV.** Le parseur CSV gère le quoting RFC 4180 (champs entre guillemets doubles, `""` échappé), mais suppose une entrée UTF-8. Les fichiers non-UTF-8 échoueront au parsing.
- **Requêtes pré-normalisées.** PostgreSQL normalise les requêtes `pg_stat_statements` au niveau du serveur. perf-sentinel applique sa propre normalisation par dessus pour la référence croisée, ce qui peut produire des templates légèrement différents.
- **Pas de connexion directe à PostgreSQL.** En mode fichier (`--input`), perf-sentinel lit des fichiers CSV ou JSON exportés. Le flag `--prometheus` scrape les métriques `postgres_exporter` au lieu de se connecter directement à PostgreSQL. Voir "Ingestion automatisée pg_stat depuis Prometheus" ci-dessus pour les limitations spécifiques au mode Prometheus.
- **Nombre d'entrées.** Le parseur pré-alloue la mémoire en fonction de la taille de l'entrée, plafonné à 100 000 entrées. Les fichiers dépassant 1 000 000 d'entrées (lignes CSV ou éléments de tableau JSON) sont rejetés avec une erreur pour prévenir l'épuisement mémoire.
