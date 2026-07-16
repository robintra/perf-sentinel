# Limitations connues et compromis

## Sommaire

- [Fiabilité de la capture OTLP](#fiabilité-de-la-capture-otlp) : pourquoi perf-sentinel peut manquer des spans en tant qu'écouteur passif.
- [La qualité de l'instrumentation borne les findings](#la-qualité-de-linstrumentation-borne-les-findings) : pourquoi un rapport maigre peut signaler une instrumentation manquante, pas un service sain.
- [Les datastores non-SQL ne sont pas analysés](#les-datastores-non-sql-ne-sont-pas-analysés) : pourquoi les spans Redis, MongoDB et similaires sont écartés à l'ingestion.
- [Tokenizer SQL](#tokenizer-sql) : compromis du normaliseur regex vs un parseur SQL complet.
- [Paramètres bindés des ORM et classification N+1 vs redundant](#paramètres-bindés-des-orm-et-classification-n1-vs-redundant) : impact des placeholders nommés sur la classification.
- [Redaction de la query string HTTP et visibilité des N+1](#redaction-de-la-query-string-http-et-visibilité-des-n1) : pourquoi les boucles N+1 sur paramètre de query sont invisibles avec les instrumentations qui redactent la query string.
- [Findings lents et ratio de gaspillage](#findings-lents-et-ratio-de-gaspillage) : pourquoi les findings lents ne contribuent pas au ratio de gaspillage I/O.
- [Interprétation des scores](#interprétation-des-scores) : les bandes healthy / moderate / high / critical pour `io_intensity_score` et `io_waste_ratio`.
- [La détection de fanout nécessite `parent_span_id`](#la-détection-de-fanout-nécessite-parent_span_id) : prérequis d'instrumentation.
- [Détection des services bavards (chatty service)](#détection-des-services-bavards-chatty-service) : portée par-trace, HTTP uniquement.
- [Détection de saturation du pool de connexions](#détection-de-saturation-du-pool-de-connexions) : heuristique basée sur le chevauchement des spans SQL, pas sur les métriques du pool.
- [Détection des appels sérialisés](#détection-des-appels-sérialisés) : heuristique de niveau info sur les spans frères séquentiels.
- [`rss_peak_bytes` sous Windows](#rss_peak_bytes-sous-windows) : pourquoi le RSS du bench est null sous Windows.
- [Échantillonnage en amont et précision de la détection](#échantillonnage-en-amont-et-précision-de-la-détection) : pourquoi un échantillonnage head-based à 1-10% masque les patterns rares et fait taire la corrélation cross-trace.
- [Échantillonnage en mode daemon](#échantillonnage-en-mode-daemon) : conséquences de `sampling_rate < 1.0`.
- [Nombre maximum d'événements par trace](#nombre-maximum-dévénements-par-trace) : cap du ring buffer par trace.
- [Traces longues et éviction TTL en mode daemon](#traces-longues-et-éviction-ttl-en-mode-daemon) : pourquoi les traces à rafales espacées sous-comptent en mode streaming.
- [Contre-pression d'analyse et délestage de charge](#contre-pression-danalyse-et-délestage-de-charge) : pourquoi un worker d'analyse lent déleste des lots entiers, de façon explicite et métrée.
- [Modèle d'état du daemon, en mémoire, mono-processus, sans état partagé](#modèle-détat-du-daemon-en-mémoire-mono-processus-sans-état-partagé) : pourquoi les replicas ne partagent pas d'état et un kill non gracieux perd la fenêtre en vol.
- [Limites de longueur des champs à l'ingestion](#limites-de-longueur-des-champs-à-lingestion) : caps en octets appliqués à la frontière d'ingestion.
- [Taille du binaire](#taille-du-binaire) : cible de la release et ce qui contribue à la taille.
- [Dashboard HTML : guard formula-injection CSV](#dashboard-html--guard-formula-injection-csv) : neutralisation OWASP CSV-injection dans les CSVs exportés.
- [Pas d'authentification (TLS disponible, auth non intégrée)](#pas-dauthentification-tls-disponible-auth-non-intégrée) : politique d'accès réseau pour les endpoints d'ingestion.
- [Subcommands query-API : `--endpoint` est une entrée de confiance](#subcommands-query-api---endpoint-est-une-entrée-de-confiance) : surface SSRF sur `tempo` et `jaeger-query`.
- [Précision des estimations carbone](#précision-des-estimations-carbone) : méthodologie proxy I/O vers énergie vers CO₂ et son incertitude.
- [Corrélation cross-trace](#corrélation-cross-trace) : co-occurrence statistique, pas causalité.
- [Attributs de code source OTel](#attributs-de-code-source-otel) : les attributs `code.*` requis pour `code_location`.
- [API de requêtage du daemon](#api-de-requêtage-du-daemon) : pas d'auth intégrée, à gater via network policy ou reverse proxy.
- [Ingestion automatisée pg_stat depuis Prometheus](#ingestion-automatisée-pg_stat-depuis-prometheus) : prérequis pour le flag `--prometheus`.
- [Secrets et credentials](#secrets-et-credentials) : pattern env-var-prioritaire pour les scrapers.
- [API Electricity Maps](#api-electricity-maps) : gestion de la clé d'API et caveats.
- [Ingestion Tempo](#ingestion-tempo) : prérequis du format protobuf.
- [Constante énergétique gCO2eq (section legacy)](#constante-énergétique-gco2eq-section-legacy-conservée-pour-les-références-croisées) : référence croisée vers Précision des estimations carbone.
- [Ingestion pg_stat_statements](#ingestion-pg_stat_statements) : pas de corrélation par trace, signal hotspot complémentaire.

## Fiabilité de la capture OTLP

perf-sentinel est un **écouteur passif** : il reçoit les traces transmises par les SDKs ou collecteurs OpenTelemetry. Contrairement à un agent in-process (ex. Hypersistence Utils), il ne peut pas garantir la capture de chaque span. Des spans peuvent être perdus à cause de :

- Problèmes réseau entre l'application et perf-sentinel
- Échantillonnage configuré au niveau du SDK ou du collecteur
- Plantages de l'application avant le flush des spans

**Atténuation :** Pour les pipelines CI critiques, utilisez le mode batch (`perf-sentinel analyze`) avec des fichiers de traces pré-collectés plutôt que de dépendre de la capture en direct.

## La qualité de l'instrumentation borne les findings

Chaque finding dérive d'un span normalisé. perf-sentinel lit une liste fermée d'attributs porteurs (le texte de la requête `db.statement` / `db.query.text`, l'URL cible `http.url` / `url.full`, plus les attributs d'enrichissement listés dans [Attributs de span requis](./INSTRUMENTATION-FR.md#attributs-de-span-requis)). Un span qui n'en porte aucun n'est pas une opération d'I/O et est ignoré. Un span SQL qui *est* une opération d'I/O mais arrive sans texte de requête, ou un span HTTP sans URL, est lui aussi ignoré : il n'y a rien à normaliser, donc aucun finding ne peut être produit. La détection est bornée par la qualité de l'instrumentation en amont, de la même manière que tout outil purement logiciel est borné par sa source de mesure.

L'écartement n'émet ni avertissement ni erreur par span, donc un attribut manquant ne remonte pas comme un problème, il remonte comme l'*absence* de finding. Depuis 0.8.7 le daemon compte ce filtrage en agrégé : `perf_sentinel_otlp_spans_received_total` et `perf_sentinel_otlp_spans_filtered_total{reason}` exposent le taux de rétention sur `/metrics` (voir [METRICS-FR.md](./METRICS-FR.md#metrics-dingestion-otlp)), une flotte dont tous les spans sont filtrés devient donc visible sans bruit par span. La cause courante en pratique est une instrumentation qui omet le texte de la requête par défaut : .NET exige `SetDbStatementForText = true`, et plusieurs bibliothèques masquent les requêtes pour des raisons de sécurité tant que la capture du texte n'est pas activée explicitement. Voir [Attributs de span requis](./INSTRUMENTATION-FR.md#attributs-de-span-requis) pour les réglages par langage.

La conséquence opérationnelle : un rapport maigre ou vide n'est pas la preuve qu'un service est sain. Cela peut tout aussi bien signifier que les spans n'ont jamais porté ce dont perf-sentinel a besoin. Auditez votre propre tracing avant de faire confiance à un score bas. Lancez `perf-sentinel inspect --input <events.json>` (ou `query --daemon <URL> inspect` contre un daemon en cours) et confirmez que les spans SQL et HTTP apparaissent avec leur texte de requête et leurs URLs. Un arbre de spans clairsemé ou vide est le signal que le coût d'entrée est un travail d'instrumentation, pas un feu vert.

Un manque de qualité de statement est réparé automatiquement, dans certaines bornes : les instrumentations en couches qui scindent une requête entre un span ~0 ms porteur du statement et un span de durée sans statement (PHP Doctrine + PDO) sont recousues en un seul événement à la conversion OTLP. La couture ne s'applique que sur le chemin OTLP (les imports JSON Jaeger et Zipkin gardent l'angle mort), jamais aux datastores non-SQL, qu'aux spans sans statement dont le nom évoque une exécution de requête (`execute`, `query`), et qu'au sein d'un même bloc `ResourceSpans` : une paire scindée entre deux batchs collector retombe sur le filtrage `missing_db_statement` décrit ci-dessus. Un span sans statement et sans `db.system` (la couche Doctrine ne porte le moteur que sur son enfant pdo) n'est recousu que s'il a un frère porteur de statement, de sorte qu'un span enveloppe ne s'approprie pas le statement de son enfant SQL. Les spans fusionnés sont comptés sous la raison `merged_db_span`.

Comme la couture est bornée à une requête d'export, les requêtes lentes sont son pire cas : le span prepare d'une requête lente finit immédiatement (exporté dans un batch précoce) alors que son span execute finit des centaines de millisecondes à des secondes plus tard (un batch ultérieur), donc le batching de l'exporteur scinde la paire entre deux requêtes et l'execute lent retombe sur `missing_db_statement`. La lenteur *est* le gap temporel qui scinde le batch. Les requêtes rapides (le motif N+1) ne scindent pas, elles se cousent donc de façon fiable. Sur les émetteurs split-couche (PHP Doctrine) en charge, assez de paires co-batchent par coïncidence pour que `slow_sql` se déclenche quand même, mais son compte d'occurrences sous-estime par rapport à un émetteur mono-couche exécutant la même faute. Un opérateur qui a besoin du plein rendement `slow_sql` pour un tel émetteur peut élargir la fenêtre de batching de l'exporteur au-delà de la requête la plus lente, pour que le prepare et l'execute d'une requête partent dans la même requête d'export : augmenter le `timeout` (et le `send_batch_size`) du batch processor du Collector OpenTelemetry, ou le `scheduledDelayMillis` du `BatchSpanProcessor` du SDK. Le coût est une latence de findings plus élevée et des batchs d'export plus gros (à garder sous `[daemon] max_payload_size`, 16 Mio par défaut).

## Les datastores non-SQL ne sont pas analysés

perf-sentinel modélise deux types d'I/O, le SQL relationnel et les appels sortants (HTTP et RPC comme gRPC). Un span dont le `db.system` désigne un datastore non-SQL (`redis`, `memcached`, `mongodb`, `cassandra`, `dynamodb`, `couchbase`, `couchdb`, `elasticsearch`, `opensearch`, `neo4j`, `hbase`, `geode`, `influxdb`) est écarté à l'ingestion, car son `db.statement` n'est pas du SQL relationnel et le tokenizer SQL le déformerait (un `GET user:123` Redis n'est pas un template de requête). L'écart est décidé sur le seul `db.system`, donc un span non-SQL est écarté qu'il porte ou non un statement ou une URL, de façon cohérente sur les chemins OTLP, Jaeger et Zipkin. L'écarter est le choix de réduction du risque : cela évite de faux findings N+1 et redundant sur le trafic cache ou document. Un `db.system` absent ou désignant un moteur SQL (`postgresql`, `mysql`, `mssql`, `oracle`, `clickhouse`, `cockroachdb`, ...) est toujours traité comme du SQL, donc aucun trafic relationnel n'est écarté par erreur.

Comme le span n'entre jamais dans le pipeline, un appel non-SQL écarté est aussi invisible aux détecteurs structurels (fanout excessif, appels sérialisés) : une requête qui éclate en de nombreux appels cache ou document ne lèvera pas ces findings. Sur le chemin OTLP, l'écart est compté sous la raison dédiée `non_sql_datastore` dans `perf_sentinel_otlp_spans_filtered_total`, gardée distincte de `not_io` pour qu'une flotte purement cache ne déclenche pas l'avertissement daemon de rétention nulle, qui ne compte que les vraies raisons de manque d'instrumentation.

## Tokenizer SQL

Le normaliseur SQL utilise un tokenizer maison basé sur les regex plutôt qu'un parseur SQL complet. C'est intentionnel : cela maintient le binaire petit, évite les dépendances lourdes et fonctionne avec tous les dialectes SQL. Cependant, il a des limitations :

- **Pas d'analyse sémantique :** le tokenizer remplace les littéraux et UUIDs de manière positionnelle. Il ne construit pas d'AST et ne peut pas raisonner sur la structure de la requête.
- **Limite de longueur de requête :** les requêtes SQL dépassant 64 Ko sont tronquées à une frontière de caractère avant la normalisation. Cela empêche les allocations mémoire illimitées depuis des entrées adverses ou pathologiques.
- **CTEs :** les Common Table Expressions (`WITH ... AS (...)`) sont supportées, le tokenizer normalise correctement les littéraux dans les CTEs, y compris les CTEs imbriquées.
- **Identifiants entre quotes :** les identifiants entre quotes sont préservés tels quels et les chiffres à l'intérieur ne sont pas confondus avec des littéraux : ANSI `"MaTable"` et MySQL `` `table` ``. Le scan ferme sur le premier délimiteur, donc un délimiteur doublé d'échappement (`""`, `` ` `` `` ` ``) n'est pas décodé.
- **Chaînes dollar-quoted :** les chaînes dollar-quoted PostgreSQL (`$$body$$`, `$tag$body$tag$`) sont remplacées par des placeholders `?`, y compris dans les corps de fonctions.
- **Instructions `CALL` :** les paramètres littéraux dans `CALL` sont normalisés (`CALL process(42, 'rush')` devient `CALL process(?, ?)`). Les expressions SQL comme `NOW()`, `INTERVAL '...'` sont gérées (la chaîne dans `INTERVAL` est remplacée, l'appel de fonction est préservé).
- **Identifiants SQL Server `[...]` :** ils ne sont pas traités spécialement (`[` est un caractère normal). Les courants comme `[Order Details]` ou `[Col1]` restent intacts, tandis qu'un identifiant entre crochets purement numérique comme `[123]` voit ses chiffres remplacés par `?`. Traiter `[` comme un caractère normal garde les littéraux et sous-scripts de tableau PostgreSQL (`ARRAY['a', 'b']`, `arr[1]`) correctement masqués.

Si vous rencontrez une requête mal normalisée, veuillez ouvrir une issue avec le SQL brut (anonymisé).

**Complémentarité avec pg_stat_statements :** perf-sentinel détecte les patterns par trace (N+1, appels redondants) que pg_stat_statements ne peut pas voir. Inversement, pg_stat_statements fournit des statistiques agrégées côté serveur (total d'appels, temps moyen) que perf-sentinel ne suit pas. Ils se complètent, utilisez les deux pour une visibilité complète.

## Paramètres bindés des ORM et classification N+1 vs redundant

Les ORM qui utilisent des paramètres nommés (Entity Framework Core avec `@__param_0`, Hibernate avec `?1`) produisent des spans SQL ou les valeurs des paramètres ne sont pas visibles dans l'attribut `db.statement`/`db.query.text`. perf-sentinel voit le template avec les placeholders mais pas les valeurs réelles.

Cela signifie que les patterns N+1 (même requête, valeurs différentes) peuvent être classifiés comme `redundant_sql` (même requête, mêmes params visibles) au lieu de `n_plus_one_sql` (même requête, params différents). Les deux findings identifient correctement le pattern de requêtes répétées et la suggestion de batcher ou cacher reste valide.

Les ORM qui injectent les valeurs littérales (SeaORM avec des requêtes brutes, JDBC sans prepared statements) produisent des spans avec des valeurs de paramètres visibles, permettant une classification précise N+1 vs redundant.

La même limitation s'applique au RPC (gRPC, Dubbo). La charge utile d'une requête gRPC vit dans le corps du message protobuf, pas dans un attribut de span, donc N appels distincts à une même méthode partagent un unique template à paramètres vides et sont lus comme `redundant_http` plutôt que `n_plus_one_http`. Les spans RPC sont aussi modélisés comme des appels HTTP sortants (ils ne portent ni statement ni URL), donc leurs findings sortent sous les types `_http` avec une remédiation teintée HTTP, et seul le côté sortant `SpanKind::Client` est admis (le span du handler entrant porte les mêmes clés `rpc.*` mais représente du travail entrant). Voir [INSTRUMENTATION-FR.md](./INSTRUMENTATION-FR.md#attributs-de-span-requis).

## Redaction de la query string HTTP et visibilité des N+1

La détection des N+1 HTTP dépend de la visibilité du paramètre variable dans le span. Une boucle N+1 qui fait varier un segment de path (`GET /api/orders/1`, `/api/orders/2`, ...) se normalise en `GET /api/orders/{id}` avec des params extraits distincts, et est détectée. Une boucle N+1 qui fait varier un paramètre de query (`GET /api/mock?seq=1`, `?seq=2`, ...) n'est détectée que si la query string survit jusqu'au span.

Certaines instrumentations redactent la query string avant l'export. OpenTelemetry .NET `System.Net.Http` la redacte en `?*` par défaut (désactivable avec `OTEL_DOTNET_EXPERIMENTAL_HTTPCLIENT_DISABLE_URL_QUERY_REDACTION=true`). Quand la query est redactée, chaque appel de la boucle porte un `url.full` identique au byte près, donc perf-sentinel voit le pattern comme `redundant_http` (même URL répétée) et non `n_plus_one_http` (même URL, paramètre différent). Le paramètre variable a été détruit en amont, donc aucun consommateur de traces (Jaeger, Tempo, ou n'importe quel backend OTLP) ne peut le récupérer, pas seulement perf-sentinel.

Les deux verdicts identifient le pattern d'appels répétés et la suggestion de batcher reste valide. Pour obtenir `n_plus_one_http` spécifiquement sur .NET, désactivez la redaction de la query via la variable d'environnement ci-dessus, ou modélisez l'identifiant variable comme un segment de path plutôt qu'un paramètre de query.

## Compromis du regroupement par host HTTP

Le template HTTP sortant garde le host de l'appelé (`GET user-svc/api/x`), donc deux appels au même chemin sur des backends différents restent des groupes séparés au lieu de fusionner en un faux `redundant_http`. Le coût inverse : un vrai N+1 peut se scinder si un même backend est adressé avec une orthographe de host incohérente dans une trace. Les IP littérales IPv4/IPv6 sont retirées (les replicas load-balancés continuent de se dédupliquer), le point final DNS est canonicalisé (`svc.` == `svc`) et le host est mis en minuscules, mais un nom court et sa forme pleinement qualifiée (`user-svc` vs `user-svc.default.svc.cluster.local`) ne sont pas réconciliés, donc une boucle qui mélange les deux orthographes contre un même backend peut se scinder en groupes sous le seuil et passer inaperçue. De même, une boucle par item qui éclate vers des backends aux noms distincts sur le même chemin (`GET shard-1/lookup`, `GET shard-2/lookup`, ...) est traitée comme des opérations distinctes plutôt qu'un N+1, puisque ces appels ne peuvent pas être batchés en un seul endpoint. Émettre une orthographe de host cohérente par appelé garde le regroupement N+1 intact.

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

Les valeurs d'enum (`healthy`, `moderate`, `high`, `critical`) sont **stables entre versions**. Les consommateurs downstream (SARIF, Grafana, intégrations IDE planifiées comme perf-lint, etc.) peuvent se brancher sur ces labels en toute sécurité.

Les **seuils numériques** qui déclenchent ces labels sont **versionnés avec le binaire**. Ils peuvent évoluer à mesure qu'on accumule des données d'usage réelles. Cela reflète le pattern existant où `co2.model` évolue de `io_proxy_v1 → v2 → v3` sans casser les consommateurs qui veulent juste savoir quel modèle a été utilisé.

Si un consommateur a besoin d'une classification indépendante de la version (par exemple, une alerte Grafana qui doit se comporter à l'identique à travers les upgrades de perf-sentinel), il doit lire les champs bruts `io_intensity_score` et `io_waste_ratio` et appliquer ses propres bandes.

### La sévérité par finding est documentée ailleurs

Pour les règles de sévérité par détecteur (`Critical` / `Warning` / `Info` sur N+1, Fanout, Slow, Chatty, Pool, Serialized), voir [`docs/FR/design/04-DETECTION-FR.md`](design/04-DETECTION-FR.md). Ces règles dépendent de seuils par détecteur partiellement config-tunables (par ex. `max_fanout × 3`, `chatty_service_min_calls × 3`) et sont documentées à côté des détecteurs eux-mêmes.

## La détection de fanout nécessite `parent_span_id`

La détection de fanout (`excessive_fanout`) repose sur le champ `parent_span_id` pour construire les relations parent-enfant entre les spans. Si l'instrumentation de tracing ne propage pas les IDs de span parent (certains anciens SDKs OTel ou instrumentations personnalisées), la détection de fanout ne produira pas de findings.

Les findings de fanout, comme les findings lents, ne sont **pas** comptés comme des I/O évitables dans le ratio de gaspillage. Ils représentent un problème structurel (trop d'opérations enfants par parent) plutôt que des I/O éliminables.

### Coefficients énergétiques par opération

Les multiplicateurs d'énergie par opération (pondération par verbe SQL, tiers de taille de payload HTTP) sont des estimations heuristiques dérivées de benchmarks académiques d'énergie SGBD (Xu et al. ICDE 2010, Tsirogiannis et al. SIGMOD 2010) et de la méthodologie Cloud Carbon Footprint. Les ratios relatifs entre opérations (SELECT < DELETE < INSERT/UPDATE) sont plus fiables que les valeurs absolues, qui varient selon les générations de matériel et les moteurs de bases de données.

Limitations principales :

- **Pas d'analyse de complexité de requête.** Un SELECT avec full table scan coûte plus d'énergie qu'un point lookup indexé, mais les deux reçoivent le même coefficient 0.5x.
- **La taille du payload HTTP nécessite des attributs OTel.** L'attribut `http.response.body.size` doit être présent sur les spans HTTP. Quand il est absent, le coefficient retombe à 1.0x.
- **Non utilisé avec l'énergie mesurée.** Quand Scaphandre ou cloud SPECpower fournit de l'énergie mesurée par service, les coefficients par opération sont ignorés.

Mettre `per_operation_coefficients = false` pour désactiver cette fonctionnalité.

### Énergie de transport réseau

Le terme optionnel d'énergie de transport réseau estime le coût énergétique du transfert d'octets entre régions. Le coefficient par défaut (0.04 kWh/Go) est un défaut prudent sous les moyennes réseau récentes (Sustainable Web Design Model v4, 2024 : 0.059 kWh/Go opérationnel pour les réseaux) et une borne haute pour le trafic serveur inter-régions, où les coefficients inter-datacenters descendent à 0.001 kWh/Gb (Cloud Carbon Footprint).

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

## Échantillonnage en amont et précision de la détection

Cette section traite de l'échantillonnage appliqué **avant** perf-sentinel, dans le SDK ou le collecteur. Elle est distincte du knob `sampling_rate` propre au daemon, traité plus bas.

L'échantillonnage head-based garde ou écarte une trace entière à sa racine. Les traces conservées arrivent complètes, donc les détecteurs par trace (`n_plus_one`, `chatty_service`, `excessive_fanout`, `pool_saturation`, `serialized_calls`) restent corrects sur les traces que perf-sentinel voit : un N+1 dans une trace conservée reste entièrement visible. Ce qu'un échantillonnage head-based agressif (le classique 1% à 10% en production, pour le coût) dégrade, c'est la couverture, pas la structure par trace :

- **Les patterns rares peuvent ne jamais apparaître.** Un pattern qui ne survient que dans une petite fraction du trafic peut être entièrement écarté et ne jamais atteindre la détection.
- **Les agrégats sont calculés sur un sous-ensemble non représentatif.** Le ratio de gaspillage I/O et les compteurs Prometheus ne reflètent que les traces échantillonnées, donc ils ne peuvent pas se lire comme des chiffres sur l'ensemble du trafic.
- **La corrélation cross-trace cesse de fait de produire.** Le [corrélateur cross-trace](#corrélation-cross-trace) a besoin qu'une paire de findings se répète (`min_co_occurrences`, défaut 5) dans sa fenêtre. À bas taux d'échantillonnage, les co-occurrences répétées survivent rarement, donc le corrélateur reste silencieux même quand le couplage sous-jacent est réel.

perf-sentinel n'inspecte pas le flag W3C `sampled` et ne distingue pas une trace complète d'une survivante d'un échantillonnage head-based. Il traite ce qui arrive comme la trace complète.

Recommandations :

- Pour les quality gates CI, utilisez le mode batch (`perf-sentinel analyze`) sur des traces intégralement capturées. Un gate qui décide sur 1% du trafic n'est pas un gate.
- Dans le daemon, si vous devez échantillonner pour le coût, préférez un échantillonnage **tail-based** au niveau du collecteur. Le tail-based garde lui aussi des traces entières, mais permet de biaiser la rétention vers les traces lentes ou en erreur, là où le gaspillage structurel se concentre.

## Échantillonnage en mode daemon

Ceci est le knob d'échantillonnage propre à perf-sentinel, appliqué après l'ingestion, distinct de l'échantillonnage en amont décrit ci-dessus. Lorsque `sampling_rate` est défini en dessous de 1.0 dans la configuration `[daemon]`, perf-sentinel supprime aléatoirement des traces pour réduire l'utilisation des ressources. Cela signifie :

- Certains patterns N+1 ou redondants peuvent ne pas être détectés
- Le ratio de gaspillage est calculé uniquement sur les traces échantillonnées et peut ne pas représenter l'ensemble du trafic
- Les métriques Prometheus (`perf_sentinel_traces_analyzed_total`) ne reflètent que les traces échantillonnées

Pour une détection précise, utilisez `sampling_rate = 1.0` (le défaut) ou échantillonnez au niveau du collecteur où vous avez plus de contrôle.

## Nombre maximum d'événements par trace

En mode streaming, chaque trace contient au maximum `max_events_per_trace` événements (défaut : 1000) dans un buffer circulaire. Si une trace génère plus d'événements, les plus anciens sont supprimés. Cela peut causer :

- Des patterns N+1 manqués si les opérations répétées tombent en dehors de la fenêtre conservée
- Un sous-comptage des occurrences dans les findings

Pour les traces avec un très grand nombre d'événements, augmentez `max_events_per_trace` ou investiguer pourquoi une seule trace génère autant d'opérations.

## Traces longues et éviction TTL en mode daemon

La fenêtre de détection en streaming évince une trace lorsqu'elle est restée inactive pendant `trace_ttl_ms` (défaut 30s). "Inactive" signifie qu'aucun span event pour ce `trace_id` n'a été ingéré dans le TTL. Le TTL actif est réinitialisé à chaque ingestion de span, donc une trace qui émet un span toutes les <30s reste vivante indéfiniment.

Mais une trace qui émet des spans creux et espacés (par exemple un job batch long qui émet un span toutes les 60s, ou un websocket en long polling) sera évincée entre les rafales. Un span tardif portant le même `trace_id` qui arrive après l'éviction crée un **nouveau** bucket de trace ; les events précédents sont perdus. Les détections threshold-driven qui s'appuient sur des spans co-localisés dans une même trace (`n_plus_one`, `chatty_service`, `excessive_fanout`, `pool_saturation`, `serialized_calls`) sous-rapportent silencieusement parce que chaque fragment passe sous le seuil per-trace.

Mitigations, par ordre de précision :

- **Augmentez `trace_ttl_ms`** si vous connaissez l'écart maximum attendu entre rafales (`[daemon] trace_ttl_ms = 120000` pour 2 minutes). La mémoire croît avec `max_active_traces`, pas avec le TTL : un TTL plus long ne coûte rien tant que le profil de trafic ne dépasse pas le cap LRU.
- **Utilisez le mode batch** (`perf-sentinel analyze`) sur un dump de trace capturé pour une investigation hors-ligne. La corrélation batch n'a pas de frontière TTL ; la trace entière est corrélée en une seule passe.
- **Raccourcissez la trace en amont.** Si une trace est conceptuellement longue parce qu'elle couvre plusieurs actions utilisateur, envisagez de la découper côté application (une trace par requête logique).

C'est une propriété de la fenêtre streaming, pas un bug. La détection temps réel sur un buffer circulaire borné troque toujours durée de trace contre mémoire ; le daemon retient 30s comme défaut adapté aux profils request-response classiques (API HTTP, RPC).

## Contre-pression d'analyse et délestage de charge

detect+score tournent sur un unique worker d'analyse dédié, découplé de la boucle `select!` d'ingestion / éviction par un canal borné (1024 lots). La boucle enfile sans bloquer les lots évincés et expirés, donc une passe d'analyse lente ne peut plus bloquer l'ingestion ni retarder l'éviction TTL. En contrepartie, lorsque l'analyse ne suit pas sous charge soutenue, la file se remplit et des lots entiers sont **délestés**. Le délestage est explicite et métré, pas silencieux :

- `perf_sentinel_analysis_queue_depth` expose le backlog courant. Une valeur non nulle durable signifie que le worker prend du retard.
- `perf_sentinel_analysis_shed_batches_total` et `perf_sentinel_analysis_shed_traces_total` comptent ce qui a été abandonné. Alertez sur `rate(perf_sentinel_analysis_shed_batches_total[5m]) > 0`.

Un lot délesté est totalement écarté de la détection : ses findings ne sont jamais émis et le corrélateur cross-trace ne le voit jamais. Comme perf-sentinel fait remonter des motifs *récurrents*, un N+1 ou un chemin bavard délesté est normalement redétecté à la requête suivante une fois le worker rattrapé. Un délestage soutenu signale un daemon sous-dimensionné pour le volume de traces : scalez horizontalement (shard par `trace_id`), gardez de la marge via `sampling_rate`, ou réduisez le coût par trace en amont.

Le délestage répond à la *surcharge*, pas à une *panne*. Si le worker d'analyse lui-même s'arrête (par exemple un détecteur qui panique sur une trace pathologique), le daemon ne reste pas debout à n'analyser plus rien : il sort en erreur pour qu'un superviseur (Kubernetes, systemd) redémarre le process, le même comportement fail-loud que l'ancienne détection en ligne, où une panique crashait tout le daemon. Tout lot enfilé dans la brève fenêtre avant la sortie est compté comme délesté plutôt que perdu silencieusement.

Un arrêt gracieux ne déleste **pas** : il vide la fenêtre et joint le worker afin que chaque lot en vol soit analysé avant la sortie.

Le délestage d'analyse est distinct de la rétention d'archive. L'archive de divulgation par fenêtre (`daemon/archive.rs`, le NDJSON que `disclose` agrège ensuite) a son propre canal borné avec une politique explicite de rejet quand il est plein. Sous charge soutenue, ou si la tâche d'écriture prend du retard sur les I/O disque, des fenêtres entières sont abandonnées de l'archive même quand leurs findings ont été analysés et servis en direct par l'API, et un arrêt gracieux vide le worker d'analyse mais n'étend pas la même garantie de livraison à l'archive. C'est un fonctionnement au mieux par conception (transparence publique, pas de niveau réglementaire), donc considérez l'archive comme un enregistrement échantillonné plutôt qu'un registre complet.

## Modèle d'état du daemon, en mémoire, mono-processus, sans état partagé

L'état de corrélation du daemon est entièrement en mémoire : une fenêtre glissante de 30s (`trace_ttl_ms`) sur un LRU de 10 000 traces (`max_active_traces`), tous deux réglables sous `[daemon]`. Il n'y a pas de couche de persistance, pas de write-ahead log, pas de snapshot, pas de débordement sur disque. Cela façonne trois propriétés opérationnelles qui comptent pour un déploiement de production sérieux.

**Un arrêt gracieux draine, un kill non gracieux non.** Sur un arrêt propre, le daemon draine sa fenêtre à travers la détection avant de quitter. SIGINT (Ctrl+C) et, sous Unix, SIGTERM déclenchent tous deux ce drain, donc une terminaison de pod Kubernetes normale (rolling update, scale-down) flushe la fenêtre en vol au lieu de la jeter. Une mort *non gracieuse* la perd quand même : SIGKILL (le kill forcé du kubelet après la période de grâce de terminaison), un OOM kill ou un crash du processus sautent le drain et jettent les traces en vol, jusqu'à une fenêtre entière, sans reprise.

L'impact pratique est faible. Ce qui est en vol à cet instant, ce sont des traces incomplètes (elles n'ont pas atteint leur TTL, donc elles reçoivent peut-être encore des spans), et perf-sentinel surface des patterns *récurrents* : un N+1 ou un chemin bavard que la fenêtre jetée aurait signalé réapparaît à la requête suivante et est capturé par le nouveau processus en quelques secondes. Les données de trace ne sont pas perdues non plus, elles vivent en amont dans votre collecteur ou votre store de traces. Les acknowledgments runtime sont sur disque et survivent (voir [mode StatefulSet](./HELM-DEPLOYMENT-FR.md#statefulset)). Le seul endroit où un trou est visible est l'archive NDJSON par fenêtre (opt-in), qui manque la fenêtre en vol au moment du kill. Si cela compte, gardez `trace_ttl_ms` court, ou exécutez les gates en mode batch où il n'y a pas de fenêtre à perdre.

**Les replicas ne partagent pas d'état.** Chaque instance du daemon est indépendante : sa propre fenêtre, ses propres métriques, son propre corrélateur. Le chart Helm expose `workload.replicas`, mais il n'y a ni leader election ni store partagé. Deux replicas qui analysent le même service calculent deux vues partielles, jamais une vue fusionnée. Les compteurs Prometheus sont par replica et doivent être agrégés au niveau PromQL.

**La corrélation suppose des spans co-localisés.** Les détecteurs par trace et le [corrélateur cross-trace](#corrélation-cross-trace) sont une structure par processus. Ils supposent que chaque span d'une trace donnée, et chaque trace liée, atterrit dans le même daemon. Conséquences pour le passage à l'échelle horizontal :

- **Les détecteurs par trace** (`n_plus_one` et consorts) sont corrects avec plusieurs replicas *seulement* si le collecteur répartit par `trace_id` afin que toutes les spans d'une trace atteignent la même instance. Le `loadbalancingexporter` du Collector OTel avec `routing_key: traceID` fait cela, et la [topologie shardée](./HELM-DEPLOYMENT-FR.md#deployment-par-défaut) en dépend.
- **La corrélation cross-service** n'a pas de réponse distribuée aujourd'hui. Elle ne voit que ce qu'un processus met en tampon, donc elle doit tourner sur un daemon unique qui reçoit tous les services concernés, ou vous acceptez une corrélation partielle. La répartition par `trace_id` n'aide pas ici, car la corrélation cross-service couvre des traces *différentes*.

## Limites de longueur des champs à l'ingestion

Toutes les frontières d'ingestion (OTLP, JSON, Jaeger, Zipkin) tronquent les champs texte pour empêcher une croissance mémoire non bornée. Limites : `service` 256 octets, `operation` 256 octets, `target` 64 Ko, `source.endpoint` 512 octets, `source.method` 512 octets, `timestamp` 64 octets, `trace_id`/`span_id` 128 octets. La troncation préserve les frontières de caractères UTF-8. Les champs en dessous de la limite ne sont pas modifiés.

## Taille du binaire

Le binaire release cible < 15 Mo avec `lto = "thin"`, `strip = true` et `panic = "abort"`. La table d'intensité carbone embarquée et le support protobuf OTLP contribuent à la taille du binaire. Si vous avez besoin d'un binaire plus petit et n'utilisez pas l'ingestion OTLP, la compilation avec des feature flags (travail futur) pourrait réduire la taille.

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

## Subcommands query-API : `--endpoint` est une entrée de confiance

Les subcommands `tempo` et `jaeger-query` effectuent tous deux des requêtes HTTP sortantes vers un backend fourni par l'utilisateur. Une contrainte à connaître :

- **`--endpoint` est une entrée de confiance.** Le validateur rejette les schémas non-`http(s)` et les URLs avec credentials (`user:pass@host`), mais accepte loopback, RFC 1918, link-local et les targets cloud metadata (`169.254.169.254`). Dans une invocation CLI mono-utilisateur c'est le comportement attendu (setups locaux dev, backends port-forwardés). Dans un pipeline CI où la valeur d'endpoint pourrait provenir d'une PR externe ou d'une variable d'environnement non fiable, assainissez la valeur en amont avant d'invoquer le subcommand.

### Headers d'authentification

Les deux subcommands supportent un flag optionnel `--auth-header "Name: Value"` qui attache un header custom à chaque requête backend. Utilisable pour Bearer tokens, Basic Auth ou headers API-key custom. La valeur parsée est marquée `sensitive`, donc hyper la redacte des debug outputs et des tables HPACK HTTP/2, et le subcommand ne log jamais la valeur. Exemples :

```bash
perf-sentinel jaeger-query --endpoint https://jaeger.prod \
  --service order-svc --lookback 1h \
  --auth-header "Authorization: Bearer ${JAEGER_TOKEN}"

perf-sentinel tempo --endpoint https://tempo.prod \
  --service order-svc --lookback 1h \
  --auth-header "X-API-Key: ${TEMPO_KEY}"
```

Validation (rejet au parse avec exit code dédié) :

- Entrée brute sous 8 KiB.
- Nom et valeur non vides après trim.
- Valeur HTTP valide selon RFC 7230 (pas de CR, LF ni ASCII non visible).
- Nom d'header refusé si : `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`, `Proxy-Connection`. Ces headers de framing et d'authority sont bloqués pour éviter request smuggling et cache poisoning via une variable d'environnement non fiable.

### `--auth-header-env NAME` : alternative ps-safe

Les deux subcommands acceptent aussi `--auth-header-env NAME`, qui lit la ligne d'header depuis la variable d'environnement nommée au lieu de `argv`. Cela évite l'exposition `ps`/`/proc/<pid>/cmdline`. La valeur de la variable doit déjà être au format curl `Name: Value`. `--auth-header` et `--auth-header-env` sont mutuellement exclusifs au niveau clap.

```bash
export JAEGER_AUTH="Authorization: Bearer ${JAEGER_TOKEN}"
perf-sentinel jaeger-query --endpoint https://jaeger.prod \
  --service order-svc --lookback 1h \
  --auth-header-env JAEGER_AUTH
```

Caveats partagés par les deux flags :

- Un seul header par invocation. Si vous avez besoin de Basic Auth plus d'un header de tenant, composez le flag avec le schéma d'auth primaire et gérez le secondaire au niveau du reverse proxy.
- Passer `--auth-header` avec un endpoint `http://` émet un `tracing::warn!` car la credential voyagerait en clair. Préférez `https://` dès que le backend le permet.

## Précision des estimations carbone

perf-sentinel utilise un **modèle proxy I/O → énergie → CO₂** pour estimer l'empreinte carbone des charges de travail analysées. La chaîne comporte trois étapes, chacune introduisant une marge d'erreur :

1. **Opérations I/O → énergie** : chaque opération I/O détectée (requête SQL, appel HTTP) est multipliée par une constante fixe `ENERGY_PER_IO_OP_KWH` de `0,0000001 kWh` (~0,1 µWh). Cette valeur n'est **pas mesurée**, c'est une approximation d'ordre de grandeur.
2. **Énergie → CO₂** : l'énergie est multipliée par une intensité carbone réseau par région (gCO₂eq/kWh) issue d'Electricity Maps et Cloud Carbon Footprint (moyennes annuelles 2023-2024), avec un PUE par fournisseur (AWS 1,15, GCP 1,09, Azure 1,17, Generic 1,2). Les trois PUE fournisseurs ne sont pas strictement comparables en périmètre : AWS publie une moyenne flotte mondiale pour l'année calendaire 2024, GCP une moyenne TTM (trailing-twelve-month) sur la flotte mondiale en 2024, Azure une valeur FY25 (juillet 2024 à juin 2025) pour ses seuls datacenters owned-and-controlled (le leased et le colocation sont exclus). L'écart de fenêtre est d'environ 12 mois et l'écart de périmètre est de l'ordre de quelques pourcents de la flotte.
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

Le champ `co2.total` contient le **numérateur SCI v1.0** `(E × I) + M`, sommé sur toutes les traces analysées (une **empreinte**, émissions absolues). Le score d'**intensité** par unité fonctionnelle que la spécification SCI appelle "SCI" est émis à côté, sur `co2.sci_per_trace` :

```
co2.sci_per_trace.mid = co2.total.mid / analysis.traces_analyzed
```

L'unité fonctionnelle R est déclarée sur `co2.functional_unit` (`"trace"`). Les deux vues sont conservées car elles répondent à des questions différentes : l'empreinte dimensionne l'impact absolu, l'intensité le normalise par unité de travail. Le champ `methodology` de chaque `CarbonEstimate` tague la sémantique :

- `co2.total.methodology = "sci_v1_numerator"` : l'empreinte `(E × I) + M` sur les traces analysées.
- `co2.sci_per_trace.methodology = "sci_v1_intensity"` : l'intensité par R `((E × I) + M) / R`, R = 1 trace.
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

#### Profil horaire Allemagne (`eu-central-1`) : divergence résolue en 0.8.7

Jusqu'en 0.8.6 le profil horaire Allemagne portait une moyenne arithmétique de ~431 g/kWh face à une valeur annuelle plate embarquée de 338, un écart de ~28% que la documentation décrivait comme "des données récentes plus élevées". Un audit contre les sources primaires a inversé ce récit : le réseau allemand s'est décarboné sur 2023-2025 (Electricity Maps consommation : 475 en 2022, 379 en 2023, 341 en 2024), donc le niveau du profil horaire était le côté périmé, figé au niveau de la crise charbon de 2022, alors que la valeur annuelle 338 était à jour. Tel que livré avant 0.8.7, activer les profils horaires gonflait le CO₂ de Francfort d'environ 27% par rapport aux données primaires actuelles.

Depuis 0.8.7 le profil est recalibré (forme préservée, niveau normalisé sur la moyenne Electricity Maps 2024 de ~341), donc les chemins horaire et annuel plat concordent dans les ±5% comme toutes les autres régions profilées.

**Ce que ça signifie pour vos rapports :**

- Les rapports avec `default_region = "eu-central-1"` (ou des spans portant `cloud.region = eu-central-1`) et le défaut `use_hourly_profiles = true` affichent des **chiffres CO₂ environ 21% plus bas** qu'en 0.8.6.
- Si vos quality gates CI (`[thresholds] io_waste_ratio_max` etc.) sont calibrés sur les anciens chiffres horaires DE, recalibrez-les après la mise à niveau.

Un test de régression (`de_flat_annual_numerical_regression`) épingle la valeur annuelle plate, et l'invariant ±5% profil-contre-annuel est désormais appliqué à toutes les régions sans exception.

### Ce que couvre une attribution purement logicielle

perf-sentinel est un outil d'attribution purement logiciel. Cette classe regroupe les lecteurs RAPL (`intel-rapl` via Powercap) et les estimateurs basés sur un modèle qui dérivent l'énergie de l'utilisation CPU (Cloud SPECpower, coefficients SPECpower épinglés par SKU). Sur un serveur typique, ni l'un ni l'autre ne voit la puissance totale prise au wattmètre. RAPL rapporte les packages CPU et DRAM et manque le contrôleur de stockage, les SSD, les cartes réseau, les ventilateurs, le BMC, ainsi que les pertes de conversion de l'alimentation. Les estimateurs basés sur un modèle héritent du même périmètre par construction, puisque leurs coefficients sont calibrés sur la puissance CPU et DRAM.

Les mesures publiées varient selon le matériel et la charge, mais l'ordre de grandeur est cohérent : sur les CPU serveurs Intel courants, RAPL capte environ la moitié à deux tiers de la puissance prise au wattmètre, le reste étant la périphérie. perf-sentinel appartient à la même classe et se situe dans la même plage. Pour l'énergie totale serveur sur un SKU connu, il faut le coupler à un wattmètre externe (PDU SNMP, smart plug) ou à une lecture matérielle. Pour l'énergie compute et DRAM attribuable par trace, le modèle est ce qu'il est, et la discussion de précision se trouve dans les sections [Limites de précision Scaphandre](#limites-de-précision-scaphandre) et [Limites de précision du cloud SPECpower](#limites-de-précision-du-cloud-specpower) ci-dessous.

Quand vous lisez des benchmarks qui comparent ces outils à un wattmètre externe, gardez deux grandeurs séparées. Premièrement, la périphérie qu'aucun signal purement logiciel ne peut couvrir. Deuxièmement, à quel point un outil donné attribue correctement la fraction qu'il voit à un container, un processus ou un span. Seule la deuxième est une propriété de l'outil. La première est une propriété du signal.

### Limites de précision Alumet

perf-sentinel intègre en opt-in [Alumet](https://github.com/alumet-dev/alumet) (INRIA/LIG, EUPL-1.2), scrapé via son plugin de sortie `prometheus-exporter`. `alumet_rapl` est en tête de la chaîne de précédence de l'énergie mesurée.

**Pourquoi il surclasse Scaphandre.** Les deux lisent RAPL. L'échantillonnage d'Alumet est mesurablement moins erroné, comme le caractérisent ses propres auteurs dans [Dissecting the software-based measurement of CPU energy consumption](https://hal.science/hal-04420527v2/document) (Raffin et al.), et il attribue par cgroup plutôt que par processus, ce qui colle mieux aux charges conteneurisées. Être classé premier est une affirmation sur la fidélité de l'attribution, pas sur la couverture : comme Scaphandre, RAPL ne voit que le CPU et la DRAM, soit environ la moitié à deux tiers de la puissance prise au wattmètre. Pour l'énergie totale du serveur, voir [Limites de précision Redfish BMC](#limites-de-précision-redfish-bmc).

**Le mode d'échec de l'intervalle. À lire.** Le `prometheus-exporter` d'Alumet publie chaque mesure comme une **jauge Prometheus contenant la dernière valeur flushée**, et `rapl_consumed_energy` est un `CounterDiff` : les joules consommés pendant un `poll_interval` de la source. Ce n'est ni un compteur cumulatif (comme Kepler), ni une puissance (comme Scaphandre). Deux conséquences :

- Sommer les relevés bruts entre scrapes est faux dans les deux sens. Scraper plus vite qu'Alumet ne flushe compte deux fois la même valeur, scraper moins vite perd des intervalles entiers. perf-sentinel ne somme donc jamais, il divise par `energy_interval_secs` pour retrouver des watts et intègre sur sa propre fenêtre de scrape, exactement comme il le fait pour la jauge de puissance de Scaphandre.
- **`energy_interval_secs` ne peut pas être vérifié sur le fil.** L'intervalle n'apparaît nulle part dans l'exposition. Si la valeur déclarée s'écarte du `poll_interval` côté Alumet, toutes les valeurs d'énergie et de carbone des services concernés sont mises à une échelle linéairement fausse, **en silence** : déclarer `1.0` alors qu'Alumet échantillonne à `5s` surestime l'énergie d'un facteur 5, sans avertissement, sans scrape en échec, et avec une étiquette de provenance `measured` qui a l'air faisant autorité. C'est le plus gros risque de justesse de l'intégration. Revérifiez les deux fichiers ensemble dès que l'un des deux change. Le démon affiche la valeur utilisée dans la ligne de log `Alumet scraper started`, la faute est donc au moins visible au démarrage.

L'hypothèse de stationnarité est la même que celle que porte Scaphandre : l'intervalle échantillonné est pris comme représentatif de toute la fenêtre de scrape. Le relevé d'Alumet est une moyenne sur son `poll_interval` plutôt qu'un échantillon instantané, ce qui est plutôt le mieux comporté des deux.

**L'attribution demande une composition de plugins.** `rapl` seul mesure la machine, sans notion de charge de travail. `procfs` n'identifie les consommateurs que par PID, ce qui est inutilisable pour un mapping de service stable. L'attribution par service demande `rapl` + `k8s` + `energy-attribution`, et la métrique résultante porte le nom d'une formule choisie par l'opérateur. C'est pourquoi `metric_name` et `label_key` sont des champs de configuration obligatoires sans défaut, voir `docs/FR/CONFIGURATION-FR.md`.

**Version amont.** Alumet est **pré-1.0** (v0.9.5 au moment de l'écriture) et n'a pas encore de job CI de conformité de fil : les runners GitHub n'exposent aucun arbre powercap, RAPL ne peut donc pas y tourner. Les noms de métriques et la configuration des plugins peuvent changer d'une version à l'autre. Le filet à l'exécution est l'avertissement unique zéro-échantillon, qui se déclenche après trois ticks HTTP 200 consécutifs sans échantillon correspondant et nomme les causes probables. Traitez une montée de version d'Alumet comme une raison de relancer `curl <endpoint> | grep -i energy` et de comparer avec `metric_name`.

**Prérequis de plateforme.** Linux, et ce qu'exige le plugin source choisi. La source `rapl` demande un x86_64 Intel ou AMD avec accès RAPL (perf-events ou un `/sys/devices/virtual/powercap/intel-rapl` lisible), donc les mêmes contraintes bare-metal et de passthrough RAPL que Scaphandre s'appliquent. Alumet fournit aussi des sources pertinentes sur ARM (`nvidia-jetson`, `grace-hopper`) que perf-sentinel peut scraper via la même surface générique `metric_name` / `label_key`. **Attention : l'étiquette de modèle `alumet_rapl` est appliquée à toute lecture Alumet, quel que soit le plugin source qui l'a produite** : perf-sentinel n'a aucun moyen de distinguer une série RAPL d'une série Jetson, le nom de métrique étant choisi par l'opérateur. Scraper une source Alumet non-RAPL étiquette donc ces chiffres `alumet_rapl` dans le rapport, dans `energy_source_models` et dans toute divulgation publiée. Ne pointez `[green.alumet]` que vers une série adossée à RAPL, sauf si vous assumez cette étiquette de provenance.

### Limites de précision Scaphandre

perf-sentinel embarque une intégration opt-in avec [Scaphandre](https://github.com/hubblo-org/scaphandre) pour la mesure énergétique par processus via les compteurs Intel RAPL. Quand `[green.scaphandre]` est configuré, le daemon `watch` scrape l'endpoint Prometheus Scaphandre toutes les quelques secondes et utilise les lectures de puissance mesurées pour remplacer la constante proxy `ENERGY_PER_IO_OP_KWH` fixe pour chaque service mappé.

**Version upstream.** Le parser est version-agnostic par design : il consomme l'exposition Prometheus standard de `scaph_process_power_consumption_microwatts` avec les labels `exe` et `cmdline`, stable à travers les releases upstream. Le job CI wire-conformance (`.github/workflows/upstream-wire-conformance.yml`) pin **Scaphandre v1.0.2** par SHA256 de l'artefact `.deb` upstream comme version de référence validée. Les autres releases récentes sont attendues comme fonctionnelles, un renommage upstream du métrique ou des labels déclencherait au runtime le filet warn-once "zero-sample" et l'assertion wire-conformance en CI.

**Exigences plateforme.** Scaphandre fonctionne sur :

- **Linux uniquement** (pas Windows, pas macOS, pas BSD).
- **CPU x86_64 Intel ou AMD avec support RAPL** : la plupart des puces serveur et desktop récentes, mais notamment **PAS ARM64**. Apple Silicon, Ampere, Graviton et instances cloud ARM similaires ne peuvent pas utiliser cette intégration.
- **Bare metal ou VMs avec passthrough RAPL.** La plupart des VMs cloud (AWS EC2, GCP GCE, Azure VMs) n'exposent **pas** les compteurs RAPL aux OS invités. Les pods Kubernetes s'exécutant sur des nœuds bare-metal peuvent accéder à RAPL si l'hôte expose `/sys/class/powercap/intel-rapl/` dans le conteneur (nécessite accès privilégié ou mount explicite).

**Pourquoi la branche Scaphandre ne couvre pas ARM64.** L'upstream Scaphandre suit le support ARM dans [l'issue #35](https://github.com/hubblo-org/scaphandre/issues/35), ouverte depuis 2020 sans implémentation. RAPL est une interface Intel reprise par AMD à partir du noyau 5.11. Les CPU ARM n'ont pas d'équivalent qui exposerait des compteurs d'énergie par package via `/sys/class/powercap/`. La feuille de route Scaphandre mentionne un "capteur basé sur l'estimation" qui fonctionnerait sur toute architecture, mais il reste non implémenté (dernière activité upstream sur le sujet : novembre 2023). Sur Graviton, Ampere, Apple Silicon et Raspberry Pi, le binaire Scaphandre se compile pour `aarch64` mais le capteur RAPL échoue au démarrage. perf-sentinel intègre désormais deux sources d'énergie mesurée qui fonctionnent sur ARM : [Limites de précision Kepler](#limites-de-précision-kepler) (énergie mesurée par pod via eBPF) et [Limites de précision Redfish BMC](#limites-de-précision-redfish-bmc) (puissance murale bare-metal). Les deux se placent avant `cloud_specpower` dans la chaîne de priorité, donc les charges ARM obtiennent un vrai signal avant que la pile ne retombe sur les coefficients CCF Graviton/Cobalt (±40%) puis sur le proxy I/O.

Sur les plateformes non supportées, la section `[green.scaphandre]` est parsée et le scraper est lancé, mais il échouera à trouver l'endpoint et retombera silencieusement sur le modèle proxy. Une seule ligne de log au niveau warn est émise au premier échec pour que les opérateurs remarquent la mauvaise configuration.

**Ce que Scaphandre améliore.** L'intégration remplace le coefficient proxy fixe (0,1 µWh par op I/O) par une **valeur mesurée au niveau service** dérivée de la consommation réelle du processus mappé sur la fenêtre de scrape. Formule :

```
energy_per_op_kwh = (process_power_watts × scrape_interval_secs) / ops_in_window / 3_600_000
```

Ce qui capture :

- **La puissance processus réelle** (pas une approximation moyenne).
- **Les différences entre services** : Java vs .NET vs Node vs Go auront des empreintes énergétiques différentes même pour des charges I/O similaires.
- **La variance de charge dans le temps** : un service idle et un service en charge obtiennent des coefficients différents pendant que le daemon tourne.

Les rapports où au moins un service a utilisé un coefficient mesuré sont tagués `model = "scaphandre_rapl"`. Chaîne de priorité complète : `electricity_maps_api` > `alumet_rapl` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`. Quand des facteurs de calibration sont actifs sur les modèles proxy, le suffixe `+cal` est ajouté (ex. `io_proxy_v2+cal`). Le suffixe `+cal` ne s'applique jamais à un tag mesuré, le multiplicateur de calibration cible le coefficient proxy et n'a plus de sens dès qu'une lecture mesurée le remplace.

**Ce que Scaphandre ne fait PAS.** C'est la limitation critique : **Scaphandre donne des coefficients par service, pas d'attribution par finding**. Spécifiquement :

1. **RAPL est au niveau processus, pas au niveau span.** La métrique `scaph_process_power_consumption_microwatts{exe="java"}` rapporte la consommation totale du processus `java`. Elle ne peut pas distinguer deux findings N+1 concurrents tournant dans le même processus au même moment : ils partagent le coefficient par construction.
2. **L'intervalle de scrape n'est PAS le goulot de précision.** Une fenêtre de 5 secondes moyenne la puissance sur 5 secondes. Passer à 1 seconde ne donnerait pas de précision par finding parce que RAPL lui-même moyenne à la granularité du pas Scaphandre (~2s). Le plancher de précision réel est "un coefficient par (service, fenêtre_scrape)".
3. **Les services concurrents dans le même processus ne partagent rien.** Si votre architecture fait tourner plusieurs services logiques dans la même JVM, la lecture `exe="java"` de Scaphandre couvre tous ensemble. perf-sentinel attribue l'énergie mesurée au nom de service que vous avez mappé, ce qui est une simplification.
4. **Bruit de l'ordonnanceur OS.** L'attribution de puissance par processus via `process_cpu_time / total_cpu_time` est intrinsèquement bruitée sous charges mixtes.

**Modèle mental correct.** Scaphandre vous donne un **coefficient dynamique mesuré par service** au lieu d'une **constante proxy fixe et globale**. C'est une amélioration significative dans la couche d'attribution énergétique de la pile d'estimation carbone, mais cela ne transforme pas perf-sentinel en outil de comptabilité carbone grade-réglementaire. L'intervalle d'incertitude multiplicatif 2× s'applique toujours.

**Gestion de la fraîcheur.** Le daemon jette les entrées plus anciennes que 3× l'intervalle de scrape lors de la construction du snapshot par tick. Un scraper bloqué ou un service qui cesse d'émettre des événements retombera silencieusement sur le modèle proxy après ~3 intervalles de scrape. La jauge Prometheus `perf_sentinel_scaphandre_last_scrape_age_seconds` permet aux opérateurs de configurer des alertes Grafana sur la santé du scraper.

**Mode batch.** Le mode batch `analyze` ne lance jamais le scraper et n'utilise jamais les données Scaphandre. Même si `[green.scaphandre]` est présent dans la config, la commande `analyze` l'ignore entièrement et utilise toujours le modèle proxy. Seul le daemon `watch` intègre Scaphandre.

### Limites de précision Kepler

perf-sentinel embarque une intégration opt-in pour [Kepler](https://github.com/sustainable-computing-io/kepler) (projet CNCF sandbox) qui mesure l'énergie par conteneur ou par processus via eBPF + compteurs de performance CPU. Quand `[green.kepler]` est configuré, le daemon `watch` scrape l'endpoint Prometheus `/metrics` de Kepler, calcule le delta de joules par service par rapport au scrape précédent, et publie un coefficient mesuré par opération taggué `model = "kepler_ebpf"`.

**Exigences plateforme.**

- **Linux uniquement**, toute architecture CPU supportée par Kepler (x86_64 et ARM64).
- **Kepler installé et exposant `/metrics`.** Les déploiements production exécutent en général Kepler comme `DaemonSet` Kubernetes, un pod par nœud. Dans ce cas, faire pointer l'endpoint vers le pod local au nœud ou, plus robuste, vers un Prometheus amont qui scrape l'ensemble du `DaemonSet` (le mode Prometheus-médié est réservé à une version ultérieure, cette release ne couvre que le scrape direct).
- **Support kernel eBPF** (noyau 5.4+ en pratique).

**Pourquoi cette branche couvre ARM64 alors que Scaphandre ne le fait pas.** Kepler ne dépend pas de RAPL. Sur x86_64 avec accès RAPL, il utilise les mêmes compteurs que Scaphandre, sur ARM64 il bascule sur un modèle eBPF + compteurs de performance qui produit un vrai signal, à précision dégradée. Le modèle ARM eBPF est moins précis que la voie RAPL x86, voir [l'issue Kepler #1556](https://github.com/sustainable-computing-io/kepler/issues/1556) pour le suivi upstream des limites connues (échecs de tracepoints, modèle DRAM plus faible). Pour les charges ARM, l'alternative était le proxy `cloud_specpower` à ±40%. Kepler à précision dégradée reste une amélioration significative.

**Ce que Kepler améliore vs le proxy.** Même forme que Scaphandre : remplace la constante fixe `ENERGY_PER_IO_OP_KWH` par un coefficient mesuré par service, dérivé de la lecture d'énergie eBPF et du delta d'opérations par service de la fenêtre de scrape courante. La lecture circule dans la chaîne de priorité comme `kepler_ebpf`, entre `scaphandre_rapl` (RAPL x86, plus précis) et `cloud_specpower` (CCF ±40%).

**Ce que Kepler ne fait PAS.**

1. **Granularité conteneur / processus, pas d'attribution par-finding.** Deux findings N+1 dans le même conteneur pendant la même fenêtre de scrape partagent le même coefficient par construction.
2. **Le modèle eBPF ARM est sensiblement moins précis que la voie RAPL x86.** Considérer les lectures Kepler ARM comme un signal plus fort que le proxy, non comme un substitut à un wattmètre externe.
3. **Couverture DRAM partielle sur ARM.** Le projet amont Kepler n'expose pas encore les joules DRAM par processus sur tous les SoC ARM. Prévoir une perte de périphérie en plus de la mise en garde habituelle "RAPL capte environ la moitié à deux tiers de la prise murale".
4. **Pas de partage entre pods via le collecteur de processus.** Les services co-localisés dans le même conteneur partagent un coefficient. Associer chaque service à son propre `container_name` (ou sa valeur `comm` si `metric_kind = "process"`).

**Gestion de la fraîcheur.** Même règle `3 × scrape_interval` que Scaphandre, avec la jauge Prometheus `perf_sentinel_kepler_last_scrape_age_seconds` pour la détection de scraper bloqué. La jauge est initialisée à l'instant de démarrage du scraper, donc un endpoint Kepler cassé dès le boot fait quand même progresser la métrique : les alertes Grafana sur un scraper qui n'a jamais réussi se déclenchent correctement. Une réinitialisation de compteur (redémarrage de l'exporteur Kepler) produit un delta négatif que la garde rejette (filtre `delta > 0.0 && delta.is_finite()`, soustraction `f64` classique puisque `f64` n'a pas de `saturating_sub`), le scrape suivant produit le prochain delta significatif.

**Mode batch.** Même forme que Scaphandre, `analyze` ne lance jamais le scraper Kepler. Seul `watch` intègre Kepler.

### Limites de précision Redfish BMC

perf-sentinel embarque une intégration opt-in avec le standard BMC [Redfish](https://www.dmtf.org/standards/redfish) pour les lectures de puissance murale sur bare-metal. Quand `[green.redfish]` est configuré avec un ou plusieurs endpoints de châssis, le daemon `watch` interroge la ressource `/Power` de chaque châssis pour `PowerConsumedWatts`, distribue les joules au niveau du châssis sur les services mappés proportionnellement à leurs opérations, et publie les coefficients par service taggués `model = "redfish_bmc"`.

**Exigences plateforme.**

- **Nœuds bare-metal avec un BMC supportant Redfish 1.0+.** Dell iDRAC, HPE iLO, Lenovo XCC, Supermicro X11+ conviennent, comme la référence OpenBMC. Les VMs cloud n'exposent pas de BMC et ne peuvent pas utiliser cette intégration.
- **HTTPS joignable depuis le daemon vers le BMC.** La plupart des BMCs présentent un certificat auto-signé par défaut. **Le support de bundle CA fourni par l'opérateur est réservé à une version ultérieure.** Dans cette release, définir `ca_bundle_path` empêche le scraper de démarrer avec une erreur claire. Les opérateurs avec des certificats BMC auto-signés doivent placer le BMC derrière un reverse proxy qui présente un certificat signé publiquement (ou utiliser HTTP sur un segment réseau de confiance).
- **Authentification Basic.** L'authentification Session-token Redfish (POST `/SessionService/Sessions`) n'est pas encore supportée. Le champ `auth_header` porte une ligne Basic au format curl.

**Ce que Redfish améliore par rapport au proxy.** Une mesure réelle de puissance murale au niveau du châssis. Contrairement à Scaphandre et Kepler (qui voient CPU + DRAM uniquement via RAPL ou eBPF), le BMC lit la sortie réelle de l'alimentation, donc la périphérie (NIC, disques, ventilateurs, pertes PSU) est incluse par construction.

**Ce que Redfish ne fait PAS.** Limite critique de la puissance au niveau du nœud :

1. **Granularité châssis, pas par service ni par finding.** Chaque service mappé au même châssis reçoit le **même** coefficient (`chassis_joules / somme_des_deltas_ops`) pour une fenêtre de scrape donnée. Deux services sur le même nœud n'auront jamais de coefficients mesurés distincts via Redfish.
2. **Pas d'attribution au niveau processus.** Les processus inactifs consomment toujours une puissance de base qui se retrouve allouée aux services actifs. Considérer le coefficient par service comme une borne supérieure de ce que ces services ont tiré.
3. **Pas d'attribution par finding.** Même limite que tous les autres tags mesurés de la chaîne.
4. **Variance entre fournisseurs dans la réponse JSON.** Certains BMCs retournent `null` ou `0` pour `PowerConsumedWatts` (ou `PowerWatts.Reading` sur le schema moderne) pendant les états transitoires (démarrage, rampe de ventilateurs). perf-sentinel rejette les valeurs null/zéro/négatives/NaN comme invalides et garde le coefficient précédent jusqu'à une lecture valide. Les chemins OEM des fournisseurs (ex. `Oem.Hpe.PowerSummary.Watts` chez HPE) ne sont plus configurables : v0.7.6 a typé le schema en enum (`legacy_power` ou `environment_metrics`) et a retiré le pointeur JSON tapé par l'opérateur. Les OEMs qui publient la puissance à un chemin non standard doivent placer le BMC derrière un reverse proxy qui re-formate la réponse vers le schema standard.

**Choix du schema et lissage du capteur.** Les deux schemas supportés résolvent vers le même tag `redfish_bmc` en aval, le choix concerne uniquement la forme de la donnée. Les opérateurs doivent savoir que les deux chemins exposent typiquement des caractéristiques de lissage différentes : `legacy_power` retourne une puissance lissée par le fournisseur (Dell iDRAC ~5 s en moyenne glissante, HPE iLO 1-5 s), alors que `EnvironmentMetrics.PowerWatts.Reading` est un `SensorPowerExcerpt` typé comme une jauge instantanée. Changer de schema sur un châssis préserve la moyenne du coefficient sur fenêtre longue mais resserre l'histogramme de variance, attendez plus de jitter sur la série carbone-par-op `redfish_bmc` après migration. Choisir `legacy_power` pour la compatibilité fleet-wide aujourd'hui, `environment_metrics` pour les BMCs dont la firmware le documente explicitement.

**Énergie cumulée pas encore lue.** `EnvironmentMetrics` expose aussi `EnergykWh.Reading` (kWh cumulés), qui permettrait un coefficient calculé par delta-integration façon Kepler (joules_total), strictement plus précis que `watts_instantanés × scrape_interval` pour les longs intervalles ou les charges en pic. Le parser actuel lit seulement la jauge de puissance instantanée des deux schemas. Une release ultérieure pourra opter pour la lecture cumulative quand la couverture fournisseur sera assez large pour en faire le chemin par défaut.

**Protection contre la limitation de débit.** `scrape_interval_secs` est écrêté à `[15, 3600]` pour Redfish. Plusieurs BMCs (notamment HPE iLO 4/5) limitent les requêtes Redfish en dessous de 30 secondes, et de nombreux fournisseurs maintiennent la valeur en cache interne sur un cycle de mise à jour de 30 s, donc un intervalle plus rapide n'apporte aucune information tout en s'exposant à des erreurs 429. Valeur par défaut : 60 s.

**IPMI hors périmètre.** Redfish est le standard moderne, le chemin IPMI nécessiterait de lier `ipmitool` ou `freeipmi` en C, ce qui sort de la règle "pas de dépendances lourdes". Documenter toute flotte uniquement IPMI comme une lacune connue.

**Gestion de la fraîcheur.** Même règle `3 × scrape_interval` que les autres sources mesurées, avec `perf_sentinel_redfish_last_scrape_age_seconds` initialisée à l'instant de démarrage du scraper, donc la jauge progresse depuis le boot même si aucun châssis n'a encore réussi. La jauge est **agrégée** : elle retombe à zéro dès qu'un châssis réussit son scrape dans un tick. Une flotte multi-châssis avec un BMC sain et plusieurs en échec affiche donc `age = 0`, le signal d'échec par châssis vit dans `perf_sentinel_redfish_scrape_failed_total{reason=...}`. Coupler les deux métriques dans les alertes Grafana qui ont besoin de granularité par châssis.

**Mode batch.** Même forme, `analyze` ne lance jamais le scraper Redfish.

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

Les rapports où au moins un service a utilisé l'estimation cloud sont tagués `model = "cloud_specpower"` (priorité : `electricity_maps_api` > `alumet_rapl` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`).

**Ce que ça ne fait PAS.** Comme Scaphandre, le modèle cloud SPECpower donne des coefficients par service, pas d'attribution par finding. De plus :

1. **L'interpolation SPECpower est linéaire.** La consommation réelle d'un serveur n'est pas parfaitement linéaire entre idle et max. La précision résultante est d'environ **+/-30%**, meilleure que le proxy (~facteur 2) mais nettement moins précise que les mesures RAPL directes de Scaphandre.
2. **Le CPU n'est pas le seul consommateur d'énergie.** La mémoire, le réseau et le stockage contribuent à la consommation totale mais ne sont pas capturés par ce modèle.
3. **Les VMs partagées faussent les lectures.** Sur des instances partagées (burstable comme `t3`, `e2-micro`), l'utilisation CPU visible ne reflète pas nécessairement la consommation réelle au niveau de l'hôte physique.
4. **La table de lookup vieillit.** Les nouvelles générations d'instances nécessitent des mises à jour de la table embarquée. Les types d'instance inconnus retombent sur un profil générique du fournisseur.

**Méthodologie unique après le refresh 2026-04-24.** La table embarquée suit désormais une méthodologie homogène : `idle_watts = vCPU * idle_per_vCPU_coefficient` et `max_watts = vCPU * max_per_vCPU_coefficient`, avec les coefficients tirés par fournisseur du snapshot Cloud Carbon Footprint `ccf-coefficients` 2026-04-24. AWS, GCP et Azure partagent uniformément cette approche. La colonne d'overhead baseboard AWS du snapshot 2023-05-01 n'est plus publiée par CCF, elle a donc été retirée partout. Quand le calcul direct `SPECpower_ssj 2008` (2024 Q1 - 2026 Q2) divergeait de plus de 5 pour cent sur idle ou max, la valeur a été alignée sur CCF par cohérence de source (Sapphire Rapids, EPYC Genoa, Graviton 3/4). Les entrées modernes dont le calcul direct reste dans les 5 pour cent de CCF, ou dont l'architecture est absente du CSV du fournisseur (Emerald Rapids Azure, Genoa Azure, Turin GCP, Ampere Altra GCP, Cobalt 100 Azure), conservent leur valeur SPECpower directe et sont étiquetées explicitement dans `table.rs`. **Conséquence** : les instances AWS legacy (`m5`, `c5`, `r5`, `m6i`) lisent plus bas qu'avant parce que l'overhead baseboard n'est plus ajouté, les instances Sapphire Rapids (`m7i`, `c7i`, `r7i`, GCP `c3`) lisent plus haut parce que l'agrégat SPECpower CCF est plus récent que notre échantillon direct 2024 Q1.

**Graviton 3/4 et Cobalt 100 sont estimés, pas mesurés.** AWS ne soumet pas Graviton à SPECpower, Microsoft ne soumet pas Cobalt 100. Le refresh CCF 2026-04-24 mappe Graviton 2 / 3 / 3E / 4 sur son coefficient EPYC 2nd Gen (0.474 idle / 1.693 max W/vCPU) comme proxy conservateur en l'absence de données mesurées, donc toutes les générations Graviton partagent la même valeur per-vCPU. AWS revendique publiquement que Graviton 4 est plus efficace que Graviton 3, mais aucune soumission SPECpower n'existe pour les différencier. Cobalt 100 (Neoverse N2) est absent du CSV CCF Azure et conserve un midpoint 0.60/2.20 W/vCPU entre Ampere Altra Q80-30 (Neoverse N1, SPECpower 2024 Q1, 0.67/1.75 W/vCPU comme plancher) et la référence Graviton 3 V1, en attendant des données SPECpower Cobalt directes. Ces valeurs ARM portent une couche d'incertitude supplémentaire : prévoir **+/-40% plutôt que +/-30%** pour les entrées Graviton, Cobalt 100 et Ampere Altra.

**EPYC 5th Gen Turin proxié sur Genoa en attendant une correction amont de CCF.** L'entrée CCF 2026-04-24 pour EPYC 5th Gen Turin est 3.682 idle / 8.961 max W/vCPU, soit environ cinq fois plus haut que le coefficient voisin EPYC 4th Gen Genoa (0.739 / 2.282) sur la même structure de table. La soumission SPECpower amont a probablement été mesurée au niveau chip plutôt que thread, ou reflète un échantillon trop petit pour généraliser. Nous overridons Turin (AWS `m8a` / `c8a`) sur le coefficient Genoa plutôt que d'importer la ligne CCF verbatim, une inflation silencieuse 4x sur les clients m8a dégraderait la crédibilité directionnelle de l'outil tandis qu'un proxy Genoa est au pire conservateur et au mieux correct puisque Zen 5 est censé être au moins aussi efficient que Zen 4 par thread. L'override est tracé ici pour ré-évaluation quand CCF publiera une ligne EPYC 5th Gen révisée ou quand des soumissions SPECpower indépendantes pour EPYC 9755 / 9655 sortiront. Prévoir **+/-40%** d'incertitude sur Turin en attendant.

**SKUs memory-optimized portent un premium DRAM additif sur le coefficient CPU.** CCF 2026-04-24 ne publie pas de premium memory-class, nous en ajoutons donc un sur le coefficient CPU per-vCPU pour les familles memory-optimized : `r5`, `r5a`, `r6i`, `r7i`, `r7a` sur AWS, `n2-highmem-*` sur GCP, et `Standard_E*` v3 à v6 sur Azure. Le premium est `0.02 W/GB` idle et `0.05 W/GB` max (datasheets Crucial DDR4 RDIMM, modèle Boavizta DIMM), et le ratio mémoire 8 GB/vCPU de ces familles donne un uplift per-vCPU de `+0.16` idle / `+0.40` max. C'est l'une des deux déviations méthodologiques par rapport au CSV dans le refresh 2026-04-24 (l'override Turin étant l'autre), documentée inline dans `table.rs`. Les entrées AWS memory-optimized r-series sur silicium AMD (`r5a` sur EPYC 1st Gen, etc.) reçoivent le même uplift que les r-series Intel puisque la DRAM est indépendante de l'architecture CPU. Les familles general-purpose (`m5`, `m6i`, etc.) portent environ 4 GB/vCPU de DRAM, les familles compute-optimized (`c5`, `c6i`, etc.) environ 2 GB/vCPU. Ni les unes ni les autres ne reçoivent le premium sous la règle actuelle, ce qui sous-estime leur idle d'environ 6 à 8 pour cent (m-series) et 3 à 4 pour cent (c-series). Les deux restent dans le bracket d'incertitude 2x, et nous n'appliquons pas de demi-premium pour ne pas composer la divergence méthodologique avec CCF.

**Modèle mental correct.** Le modèle cloud SPECpower vous donne un **coefficient dynamique par service basé sur l'utilisation CPU réelle** au lieu d'une **constante proxy fixe globale**. C'est une amélioration significative pour les déploiements cloud où Scaphandre n'est pas disponible (la plupart des VMs cloud n'exposent pas RAPL). L'intervalle d'incertitude passe d'un facteur ~2× (proxy) à environ +/-30% (SPECpower), mais l'outil reste un compteur de gaspillage directionnel, pas un instrument de comptabilité carbone.

**Mode batch.** Le mode batch `analyze` ne lance jamais le scraper Prometheus et n'utilise jamais les données cloud. Même si `[green.cloud]` est présent dans la config, la commande `analyze` l'ignore entièrement et utilise toujours le modèle proxy. Seul le daemon `watch` intègre l'estimation cloud.

## Corrélation cross-trace

La corrélation temporelle cross-trace (`[daemon.correlation]`) nécessite le mode daemon (`perf-sentinel watch`) avec un trafic soutenu et représentatif. Les corrélations sont statistiques : elles détectent des co-occurrences temporelles, pas des relations causales. Une corrélation élevée entre un N+1 dans le service A et une saturation du pool dans le service B signifie qu'ils co-surviennent fréquemment dans le délai configuré, pas que l'un cause l'autre.

Limitations :

- **Démarrage à froid.** Le corrélateur a besoin de temps pour accumuler suffisamment d'observations. Avec `min_co_occurrences = 3` et une fenêtre de 10 minutes, il faut au moins 3 co-occurrences en 10 minutes avant qu'une corrélation remonte. Les environnements à faible trafic peuvent ne jamais atteindre ce seuil.
- **Mode batch non supporté.** La commande `analyze` ne lance pas le corrélateur. La corrélation cross-trace est intrinsèquement une préoccupation du streaming.
- **Cardinalité.** Le plafond `max_tracked_pairs` (défaut 1000) empêche la croissance mémoire non bornée. Si vous avez de nombreux types de findings distincts sur de nombreux services, certaines paires peuvent être évincées avant d'atteindre le seuil de co-occurrences.

Pour consommer les corrélations :

- Lancer un daemon : `perf-sentinel watch --otlp-grpc 0.0.0.0:4317`.
- Interroger : `perf-sentinel query correlations`.
- Ou ouvrir le dashboard généré par `perf-sentinel report` à partir d'un payload qui contient des corrélations (seuls les rapports produits par le daemon en contiennent).

Le mode batch `analyze` reporte toujours un tableau de corrélations vide. C'est voulu, pas un bug.

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
- **Plafond de concurrence sur le fetch parallèle.** Le flow search-then-fetch (`--service --lookback`) récupère les corps de trace en parallèle via un `tokio::task::JoinSet`, capé à 16 requêtes in-flight par un sémaphore interne. Le cap n'est pas configurable par l'utilisateur aujourd'hui. Timeout par fetch à 30s (vs. 5s pour l'étape search) pour laisser la query-frontend assembler un trace à fort fanout depuis ingesters + stockage long terme. Sur un Tempo sous-dimensionné avec des fenêtres longues (24h par exemple), certains fetches peuvent malgré tout timeout. Le remède est côté Tempo : scaler `tempo-query-frontend`, ajuster `max_search_duration` et `max_concurrent_queries`.
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
