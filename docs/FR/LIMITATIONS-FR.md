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
- **ASCII uniquement :** le tokenizer opère octet par octet et suppose un SQL en ASCII. Les caractères non-ASCII dans les identifiants, commentaires ou littéraux de chaîne (ex. caractères accentués) peuvent produire des valeurs de template ou de paramètres incorrectes. Les mots-clés et opérateurs SQL sont toujours en ASCII, donc cela n'affecte que les valeurs de paramètres extraites pour les littéraux de chaîne non-ASCII.
- **Cas limites :** les CTEs profondément imbriquées, les extensions SQL non standard ou les styles de guillemets inhabituels peuvent ne pas être normalisés correctement.
- **Procédures stockées :** les instructions `CALL` avec des expressions de paramètres complexes peuvent ne pas être entièrement normalisées.

Si vous rencontrez une requête mal normalisée, veuillez ouvrir une issue avec le SQL brut (anonymisé).

**Complémentarité avec pg_stat_statements :** perf-sentinel détecte les patterns par trace (N+1, appels redondants) que pg_stat_statements ne peut pas voir. Inversement, pg_stat_statements fournit des statistiques agrégées côté serveur (total d'appels, temps moyen) que perf-sentinel ne suit pas. Ils se complètent ; utilisez les deux pour une visibilité complète.

## Timestamps traversant minuit

La sélection min/max des timestamps pour les findings utilise une comparaison lexicographique ISO 8601, qui trie chronologiquement et fonctionne au-delà de minuit. Cependant, la **durée de fenêtre** (`window_ms`) est calculée à partir de l'heure du jour uniquement (heures, minutes, secondes, millisecondes depuis minuit). Les traces qui traversent minuit calculeront une fenêtre incorrecte de 0ms, ce qui peut causer des faux positifs de détection N+1 pour des événements qui sont en réalité espacés de ~24 heures.

**Atténuation :** En pratique, c'est rare car les traces individuelles se terminent typiquement en quelques secondes. En mode daemon, le TTL par défaut (30s) empêche les traces de s'accumuler au-delà de minuit. En mode batch, assurez-vous que les traces analysées ne traversent pas la frontière de minuit, ou acceptez le risque de faux positifs sur les traces traversant minuit.

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
- Acheminez les traces via un OpenTelemetry Collector avec ses propres extensions d'authentification, et transmettez à perf-sentinel sur un réseau interne de confiance

N'exposez jamais perf-sentinel directement sur des réseaux non fiables sans couche de sécurité en amont.

## Constante énergétique gCO2eq

L'estimation carbone utilise une constante énergétique fixe (`0,1 uWh par opération I/O`) comme approximation d'ordre de grandeur. Cette valeur n'est **pas** une quantité mesurée : la consommation réelle d'énergie dépend du type d'I/O, du matériel, de la complexité de la requête et de l'infrastructure. La constante vise à fournir une orientation directionnelle (plus d'I/O = plus d'énergie) plutôt qu'une mesure précise. Lors de la comparaison des valeurs gCO2eq entre les exécutions, les différences relatives sont significatives même si les valeurs absolues sont approximatives.
