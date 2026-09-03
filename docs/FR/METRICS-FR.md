# Référence des metrics exposées

Ce document liste toutes les metrics exposées par le daemon perf-sentinel
sur `/metrics` (format Prometheus text). L'endpoint sert à la fois
`text/plain; version=0.0.4` (Prometheus historique) et
`application/openmetrics-text; version=1.0.0` (OpenMetrics) via content
negotiation, et émet des exemplars quand des `trace_id` sont disponibles
côté findings.

**Les exemplars sont sur demande explicite.** Seul un scraper qui nomme
`application/openmetrics-text` dans son en-tête `Accept` les reçoit. Un
joker `*/*` ne suffit pas, car un scraper qui accepte tout n'est pas
forcément capable de parser un exemplar : vmagent envoie
`text/plain;version=0.0.4;*/*;q=0.1` et lit le suffixe d'exemplar d'une
gauge sans label comme une partie du nom de métrique, créant une série
morte à chaque scrape. Configurez votre scraper pour demander OpenMetrics
explicitement si vous voulez le clic vers la trace dans Grafana.

## Introduction à Prometheus et OpenMetrics

Si vous n'avez jamais utilisé Prometheus, cette introduction courte est un préalable pour la suite du document. Elle suppose que vous savez ce qu'est HTTP et ce qu'est une métrique. Elle ne suppose pas de familiarité avec le langage de requête Prometheus ou l'opérateur Kubernetes. Les autres docs perf-sentinel renvoient ici pour les concepts Prometheus, voir [docs/FR/HELM-DEPLOYMENT-FR.md](HELM-DEPLOYMENT-FR.md#observabilité) et [docs/FR/QUERY-API-FR.md](QUERY-API-FR.md).

**Qu'est-ce que Prometheus.** Prometheus est un projet de la Cloud Native Computing Foundation (CNCF), le système de métriques open source le plus largement déployé dans l'écosystème cloud-native. Il fonctionne par *scraping* : toutes les 15 à 60 secondes, le serveur Prometheus fait une requête HTTP GET sur l'endpoint `/metrics` de chaque cible, parse la réponse, et stocke les valeurs sous forme de séries temporelles. perf-sentinel expose un tel endpoint `/metrics` quand il tourne en mode daemon. Les opérateurs qui font déjà tourner Prometheus ajoutent perf-sentinel à leurs `scrape_configs`, et les métriques du daemon apparaissent à côté du reste de leur infrastructure.

**Deux formats texte servis par perf-sentinel.** La content negotiation choisit lequel le scraper reçoit.

- `text/plain; version=0.0.4` est le format d'exposition Prometheus original. Stable depuis 2014.
- `application/openmetrics-text; version=1.0.0` est **OpenMetrics**, l'évolution standardisée du format Prometheus publiée par la CNCF en 2020. C'est principalement un sur-ensemble, avec deux ajouts pratiques utilisés par perf-sentinel : les en-têtes `# UNIT` par métrique, et les **exemplars** (références de trace par point qui permettent à un panel Grafana de sauter d'un pic de métrique vers la trace exacte qui l'a produit).

**Types de métriques.** Chaque métrique ci-dessous porte un des trois types.

- **Counter**, une valeur qui ne fait que monter (par exemple le nombre de spans OTLP ingérés). Remise à zéro uniquement au redémarrage. À lire en `rate(metric[5m])` pour avoir un taux par seconde, jamais la valeur brute.
- **Gauge**, une valeur qui monte et descend (par exemple le nombre de findings en vol, ou la mémoire résidente). À lire telle quelle.
- **Histogram**, une distribution d'observations bucketisée par valeur (par exemple la latence de détection). Exposé comme plusieurs séries temporelles : `_bucket{le=...}` par bucket, plus `_sum` et `_count`. À lire avec `histogram_quantile(0.99, ...)` pour obtenir des percentiles de latence.

**Pour aller plus loin.** [prometheus.io](https://prometheus.io/), [spec OpenMetrics](https://github.com/prometheus/OpenMetrics/blob/main/specification/OpenMetrics.md), [exemplars dans OpenMetrics](https://github.com/prometheus/OpenMetrics/blob/main/specification/OpenMetrics.md#exemplars).

## Metrics process (depuis 0.5.19, Linux uniquement)

Metrics standard du `process_collector` de la crate `prometheus`.
Enregistrées uniquement sur Linux (les reads `procfs` sous-jacents
échouent sur macOS/Windows). Les opérateurs sur des hôtes non-Linux ne
voient que les metrics `perf_sentinel_*`.

| Metric                          | Type    | Description                               |
|---------------------------------|---------|-------------------------------------------|
| `process_resident_memory_bytes` | gauge   | RSS du processus daemon.                  |
| `process_virtual_memory_bytes`  | gauge   | Mémoire virtuelle.                        |
| `process_open_fds`              | gauge   | File descriptors ouverts.                 |
| `process_max_fds`               | gauge   | File descriptors max autorisés.           |
| `process_start_time_seconds`    | gauge   | Timestamp Unix du démarrage du processus. |
| `process_cpu_seconds_total`     | counter | Temps CPU cumulatif.                      |
| `process_threads`               | gauge   | Nombre de threads OS.                     |

**Coût par scrape.** Le collector lit `/proc/self/{stat,status,limits}`
et parcourt `/proc/self/fd/` à chaque scrape. Sur un daemon avec des
milliers de connexions OTLP longue durée plus des scrapers sortants,
le parcours FD peut dominer entre 1 et 5 ms par scrape. Le lock
`Registry::gather()` Prometheus est tenu pendant ce temps, donc un
collector lent bloque les scrapes concurrents quand plusieurs scrapers
(Prometheus + vmagent + sidecar) ciblent le même endpoint. Acceptable
à l'intervalle typique de 15 à 60 secondes, à noter pour des intervalles
plus serrés.

**Périmètre d'exposition.** Quand l'opérateur bind l'endpoint metrics
sur `0.0.0.0` (défaut des Pods Kubernetes pour le scraping intra-cluster),
les metrics process exposent des signaux opérationnellement sensibles :
uptime via `process_start_time_seconds` (inférence de patch / restart),
pression sur les file descriptors via `process_open_fds` et
`process_max_fds` (oracle de saturation), empreinte mémoire via
`process_resident_memory_bytes`. Le `--listen-address` par défaut est
`127.0.0.1`, ce qui restreint le scraping à l'hôte ou au Pod lui-même.
Pour les topologies de scraping cluster-wide, mettre `/metrics`
derrière une `NetworkPolicy` Kubernetes et préférer du mTLS côté
Prometheus pour qu'un Pod voisin ne puisse pas lire l'état process du
daemon librement.

## Metrics d'ingestion OTLP

| Metric                                    | Type    | Labels   | Description                                                                                                  |
|-------------------------------------------|---------|----------|--------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_otlp_rejected_total`       | counter | `reason` | Total des requêtes OTLP rejetées par le daemon depuis le démarrage, par raison (depuis 0.5.19).              |
| `perf_sentinel_otlp_spans_received_total` | counter | (aucun)  | Total des spans OTLP reçus toutes requêtes confondues, avant le filtrage I/O (depuis 0.8.7).                 |
| `perf_sentinel_otlp_spans_filtered_total` | counter | `reason` | Spans OTLP écartés par la conversion parce qu'ils ne sont pas des opérations I/O analysables (depuis 0.8.7). |

Valeurs du label `reason` :

- `unsupported_media_type` (HTTP uniquement) : `Content-Type` n'est pas
  `application/x-protobuf`. perf-sentinel n'implémente pas la variante
  OTLP encodée en JSON.
- `parse_error` (HTTP uniquement) : décodage protobuf raté.
- `channel_full` (HTTP et gRPC) : le canal d'événements est saturé ou
  fermé et le daemon n'a pas pu enqueuer le batch. L'enqueue attend
  jusqu'à 2 secondes avant de rejeter, les rafales courtes sont donc
  absorbées sans rejet tandis qu'une saturation soutenue ressort vite.
  La voie HTTP renvoie 503, la voie gRPC renvoie `UNAVAILABLE` en
  saturation (les deux sont retryables selon la spec OTLP) et
  `INTERNAL` seulement quand le canal est fermé pendant l'arrêt.
- `memory_pressure` (HTTP, gRPC et la socket JSON Unix) : le working
  set du cgroup a franchi le seuil `[daemon] memory_high_water_pct`,
  l'ingest est donc rejeté (HTTP 503, gRPC `UNAVAILABLE`, les deux
  retryables) pour borner la RSS indépendamment de la profondeur de
  queue, jusqu'à ce que l'usage retombe 5 points de pourcentage sous le
  seuil (hystérésis). L'état on/off vit sur la gauge
  `perf_sentinel_ingest_memory_pressure` (`1` pendant le rejet), c'est
  elle que l'alerte Helm surveille. Ces rejets précèdent le décodage,
  donc `perf_sentinel_otlp_spans_received_total` n'avance pas pendant
  un épisode (le nombre de spans est inconnaissable), le compteur
  compte des requêtes. Ne se déclenche jamais quand le garde-fou est
  désactivé (`memory_high_water_pct = 0`, défaut) ni sur un hôte sans
  limite mémoire cgroup v2.

Les 4 reasons sont pré-warmées à 0 au démarrage pour que les dashboards
puissent plotter la ligne zéro avant le premier rejet.

`payload_too_large` n'est **pas** comptabilisé par cette metric.
Tower-http (`RequestBodyLimitLayer`) côté HTTP et tonic
(`max_decoding_message_size`) côté gRPC appliquent la limite en amont
et renvoient 413 / `RESOURCE_EXHAUSTED` avant que le handler applicatif
ne tourne. Les opérateurs préoccupés par la taille de payload doivent
monitorer les logs du proxy ou de la gateway upstream, ou câbler un
counter de rejet tower-http dans leur stack.

Les deux counters au niveau span exposent le taux de rétention du
filtre I/O délibéré (seuls les spans SQL, HTTP sortants et de
publication vers un broker sont analysables, voir
[`LIMITATIONS-FR.md`](./LIMITATIONS-FR.md)). Une flotte dont
l'instrumentation supprime `db.statement` ou `http.url`
convertit chaque requête en zéro événement alors que les requêtes
continuent de répondre en succès, et seule cette paire de counters
rend cela visible : `perf_sentinel_otlp_spans_received_total` qui
monte pendant que `perf_sentinel_events_processed_total` reste plat
signifie que les spans arrivent mais qu'aucun ne porte d'attribut
analysable. Valeurs du label `reason` de
`perf_sentinel_otlp_spans_filtered_total`, pré-warmées à 0 :

- `not_io` : le span ne porte ni statement `db.*` ni url ou méthode
  HTTP (span interne, hit de cache, middleware...). Depuis 0.11.2,
  cela couvre aussi un span SERVER dont l'URL décrit sa propre requête
  entrante, puisqu'il s'agit d'un traitement entrant et non d'un appel
  sortant amputé. Dominant attendu sur les flottes bien instrumentées.
- `missing_db_statement` : le span a `db.system` mais ni
  `db.statement` ni `db.query.text`. Typique des drivers configurés
  pour omettre le texte des requêtes.
- `missing_http_url` : le span a une méthode HTTP mais ni `http.url`
  ni `url.full`, et n'est pas un span SERVER. Un traitement entrant ne
  porte légitimement qu'une méthode et un chemin, le compter comme un
  manque signalerait un problème d'instrumentation inexistant.
- `non_sql_datastore` : le span nomme un store non-SQL (Redis,
  MongoDB, ...) dans `db.system`. Écarté à dessein, pas un manque
  d'instrumentation (voir [`LIMITATIONS-FR.md`](./LIMITATIONS-FR.md)).
- `merged_db_span` : span DB fusionné dans l'événement unique d'une
  requête qu'une instrumentation en couches a scindée en plusieurs
  spans (statement sur l'un, durée sur l'autre, par exemple PHP
  Doctrine + PDO). La requête reste analysée, ce n'est pas non plus un
  manque d'instrumentation.

## Metrics d'analyse et de findings

| Metric                                               | Type      | Labels                        | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
|------------------------------------------------------|-----------|-------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_findings_total`                       | counter   | `type`, `severity`, `service`, `grouping` | Findings détectés depuis le démarrage. `type` reflète l'enum `Finding.finding_type`, `severity` vaut `critical` / `warning` / `info`, `service` (depuis 0.18.0) est le service émetteur, plafonné à 128 services distincts par run du daemon, le débordement étant replié dans `service="_other"` pour que les totaux restent exacts. Avec `[daemon] per_service_labels = false` le label est présent mais vide. `grouping` (depuis 0.19.0) est le regroupement effectif du finding, le premier attribut présent parmi `[detection] grouping_attributes` (`k8s.namespace.name`, puis `service.namespace`, par défaut), vide quand le span n'en portait aucun. Plafonné à 16 valeurs distinctes par run du daemon indépendamment du plafond de services, débordement replié dans `grouping="_other"`, donc `sum by (service)` sur ce label redonne exactement la série par service de la 0.18.0. Avec `[daemon] per_grouping_labels = false` le label est présent mais vide. Il ne s'appelle pas `namespace` : Prometheus Operator attache un label de cible `namespace`, et avec le `honorLabels: true` du chart celui du daemon l'emporterait. Le label ne porte que la valeur : deux spans dont le regroupement vient d'attributs configurés différents mais de même valeur partagent une série, là où les findings gardent l'identité (clé, valeur) distincte, donc ne configurez qu'une clé quand cette distinction compte. Porte des exemplars OpenMetrics par (type, severity, service, grouping) quand un `trace_id` est disponible, pendant 15 minutes après le batch qui l'a enregistré. |
| `perf_sentinel_traces_analyzed_total`                | counter   | (aucun)                       | Compte cumulatif de traces traitées par l'event loop.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `perf_sentinel_events_processed_total`               | counter   | (aucun)                       | Compte cumulatif d'events traités par l'event loop.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `perf_sentinel_active_traces`                        | gauge     | (aucun)                       | Traces actuellement actives dans la fenêtre glissante.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `perf_sentinel_analysis_queue_depth`                 | gauge     | (aucun)                       | Lots en attente dans la file du worker d'analyse (incrémenté à l'enfilement, décrémenté quand le worker prend un lot). Une valeur non nulle durable signifie que detect+score prend du retard sur l'ingestion.                                                                                                                                                                                                                                                                                                                             |
| `perf_sentinel_stored_findings`                      | gauge     | (aucun)                       | Findings actuellement retenus dans le ring buffer de la query API (depuis 0.8.8). À apparier avec `perf_sentinel_max_retained_findings` pour un ratio de headroom.                                                                                                                                                                                                                                                                                                                                                                         |
| `perf_sentinel_max_active_traces`                    | gauge     | (aucun)                       | Plafond configuré de la fenêtre glissante (`[daemon] max_active_traces`), positionné une fois au démarrage (depuis 0.8.8). À apparier avec `perf_sentinel_active_traces`. Le conseiller de réglages alerte à 90 %.                                                                                                                                                                                                                                                                                                                         |
| `perf_sentinel_analysis_queue_capacity`              | gauge     | (aucun)                       | Plafond configuré de la file du worker d'analyse (`[daemon] analysis_queue_capacity`), positionné une fois au démarrage (depuis 0.8.8). À apparier avec `perf_sentinel_analysis_queue_depth`.                                                                                                                                                                                                                                                                                                                                              |
| `perf_sentinel_max_retained_findings`                | gauge     | (aucun)                       | Plafond configuré du ring buffer de findings (`[daemon] max_retained_findings`), positionné une fois au démarrage (depuis 0.8.8). À apparier avec `perf_sentinel_stored_findings`.                                                                                                                                                                                                                                                                                                                                                         |
| `perf_sentinel_analysis_shed_batches_total`          | counter   | (aucun)                       | Lots d'analyse délestés parce que la file du worker était pleine ou que le worker s'est arrêté. Remplace le drop implicite précédent : chaque délestage est compté ici. Alertez plutôt sur `perf_sentinel_analysis_shed_traces_total`, un lot délesté pouvant porter une trace comme un millier.                                                                                                                                                                                                                                           |
| `perf_sentinel_analysis_shed_traces_total`           | counter   | (aucun)                       | Traces abandonnées par les lots délestés comptés dans `perf_sentinel_analysis_shed_batches_total`.                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `perf_sentinel_archive_windows_dropped_total`        | counter   | `reason`                      | Fenêtres de l'archive de divulgation abandonnées au lieu d'être écrites, par `reason` (`channel_full`, `writer_exited`, `serialize_error`, `write_error`). La chaîne de hachage de l'archive reste contiguë malgré la perte, ce compteur et le log d'avertissement associé sont donc les seuls témoins d'une archive incomplète. Alertez sur `rate(...) > 0`.                                                                                                                                                                              |
| `perf_sentinel_correlator_pairs_evicted_total`       | counter   | (aucun)                       | Paires du corrélateur inter-traces évincées par le plafond `max_tracked_pairs` (depuis 0.8.7). Un taux soutenu signifie que la topologie de corrélation dépasse le plafond et que les paires les moins comptées sont recyclées, `/api/correlations` peut donc perdre des entrées entre deux lectures. Les refus sont dédupliqués par batch jusqu'à 8192 paires distinctes, au-delà chaque refus compte pour lui-même, donc une topologie très large lit haut plutôt qu'exact.                                                              |
| `perf_sentinel_hub_export_pending`                   | gauge     | (aucun)                       | Signatures de findings distinctes en attente d'export vers PerfSentinelHub. Une signature répétée remplace sa valeur en attente au lieu d'augmenter cette gauge.                                                                                                                                                                                                                                                                                                                                                                           |
| `perf_sentinel_hub_export_dropped_total`             | counter   | (aucun)                       | Entrées Hub invalides, évincées à `hub_export.max_pending`, finding trop grand pour le payload borné, ou lot rejeté par le Hub avec un 4xx non rejouable (le lot entier est compté). Alertez sur `rate(...) > 0`.                                                                                                                                                                                                                                                                                                                          |
| `perf_sentinel_slow_duration_seconds`                | histogram | `type`, `service`, `grouping` | Histogramme de durée pour les spans dépassant le seuil slow, par `type` (`sql`, `http_out` ou `messaging`) et, depuis 0.18.0, `service` (plafonné à 64 services distincts, débordement replié dans `service="_other"`, vide avec `[daemon] per_service_labels = false`) et, depuis 0.19.0, `grouping` (plafonné à 8 valeurs distinctes, débordement replié dans `grouping="_other"`, vide avec `[daemon] per_grouping_labels = false`). Rien n'est pré-chauffé tant que l'un des deux réglages est actif : les séries d'une paire (service, grouping) apparaissent à son premier span lent (pré-créer les seules valeurs connues d'avance, `service="_other"` et `grouping="_other"`, afficherait un débordement qui n'a jamais eu lieu), donc une série absente signifie aucun span lent pour l'instant, pas un worker arrêté. Seule la série des deux réglages désactivés, sans label sur aucun des deux axes, est pré-chauffée à zéro comme en 0.17. Buckets : 0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 5, 10, 30 secondes. Utilisé par `histogram_quantile()` Grafana pour des percentiles précis sur des déploiements daemon shardés.                                                                                     |
| `perf_sentinel_analysis_service_overflow_total`      | counter   | (aucun)                       | Attributions de séries côté analyse (findings, I/O évitables et analysées) repliées dans `service="_other"` parce que le plafond de 128 services était atteint (depuis 0.18.0). Une hausse continue signifie que l'attribution par service se dégrade pour les services nouvellement vus, les totaux restent exacts.                                                                                                                                                                                                                      |
| `perf_sentinel_slow_duration_service_overflow_total` | counter   | (aucun)                       | Spans lents repliés dans la série d'histogramme `service="_other"` parce que le plafond de 64 services de l'histogramme était atteint (depuis 0.18.0). Même contrat que le counter de débordement d'analyse, avec un plafond plus bas car un histogramme coûte 14 séries par paire (type, service) (11 buckets plus `+Inf`, `_sum` et `_count`).                                                                                                                                                                                           |
| `perf_sentinel_analysis_grouping_overflow_total`     | counter   | (aucun)                       | Attributions de séries côté analyse (findings, I/O évitables et analysées) repliées dans `grouping="_other"` parce que le plafond de 16 regroupements était atteint (depuis 0.19.0). Indépendant du plafond de services : une série peut se replier sur l'un des axes ou sur les deux. Une hausse continue signifie que l'attribution par regroupement se dégrade pour les regroupements nouvellement vus, les totaux et les sommes par service restent exacts. |
| `perf_sentinel_slow_duration_grouping_overflow_total` | counter   | (aucun)                       | Spans lents repliés dans une série d'histogramme `grouping="_other"` parce que le plafond de 8 regroupements de l'histogramme était atteint (depuis 0.19.0). Même contrat que le counter service ci-dessus, avec le plafond le plus bas des trois car l'histogramme coûte désormais 14 séries par triplet (type, service, grouping). |
| `perf_sentinel_export_report_requests_total`         | counter   | (aucun)                       | Total des requêtes `GET /api/export/report`. Inclut les réponses cold-start (200 avec enveloppe vide).                                                                                                                                                                                                                                                                                                                                                                                                                                     |

## Metrics d'ack (depuis 0.5.21)

Activité des opérateurs sur l'API ack du daemon
(`POST` / `DELETE /api/findings/{signature}/ack`). Les acks TOML
chargés depuis `.perf-sentinel-acknowledgments.toml` au démarrage
sont en lecture seule et ne sont pas comptés ici, aucune opération
n'a lieu après le chargement initial.

| Metric                                       | Type    | Labels             | Description                                                          |
|----------------------------------------------|---------|--------------------|----------------------------------------------------------------------|
| `perf_sentinel_ack_operations_total`         | counter | `action`           | Opérations ack et unack réussies.                                    |
| `perf_sentinel_ack_operations_failed_total`  | counter | `action`, `reason` | Opérations ack et unack en échec, ventilées par raison.              |

Valeurs du label `action` : `ack`, `unack`.

Valeurs du label `reason` :

- `already_acked` (HTTP 409, `action=ack` uniquement) : signature
  déjà présente dans le JSONL daemon, ou couverte par une baseline
  TOML CI encore active. Les deux cas sont comptés sur la même
  série.
- `not_acked` (HTTP 404, `action=unack` uniquement) : la signature
  n'a pas d'ack daemon actif.
- `unauthorized` (HTTP 401) : `[daemon.ack] api_key` est défini et
  la requête est sans header `X-API-Key` ou avec un header invalide.
  La série est pré-chauffée à zéro, donc une valeur non nulle
  confirme que `api_key` est configurée (le counter n'incrémente
  que quand l'auth est appliquée).
- `no_store` (HTTP 503) : store ack daemon désactivé
  (`[daemon.ack] enabled = false`, ou chemin par défaut non
  résolvable au démarrage).
- `invalid_signature` (HTTP 400) : le segment `{signature}` ne
  passe pas la validation de format canonique.
- `limit_reached` (HTTP 507, `action=ack` uniquement) :
  `MAX_ACTIVE_ACKS` (10 000) atteint, refus du nouvel ack.
- `file_too_large` (HTTP 507, `action=ack` uniquement) : l'append
  ferait dépasser le JSONL au-dessus de 64 Mio. Saturation par
  daemon, indique qu'une compaction est nécessaire au prochain
  redémarrage ou que la limite doit être relevée. Côté `unack` ce
  cas remonte sous `internal_error` (HTTP 500), les endpoints ack
  ne différencient pas la limite sur l'écriture unack aujourd'hui.
- `entry_too_large` (HTTP 507, `action=ack` uniquement) : un seul
  record dépasse 4 Kio après sérialisation, typiquement parce que
  le champ `by` ou `reason` fourni par le caller est trop gros.
  Mauvais usage par requête, indique que la validation côté client
  doit être resserrée. Même réserve `unack` que pour `file_too_large`.
- `internal_error` (HTTP 500) : erreur d'IO, de sérialisation,
  symlink refusé, permissions trop ouvertes, ou pas de chemin de
  stockage par défaut au moment de l'écriture.

**Pré-chauffe**. Les deux counters émettent zéro pour les
combinaisons documentées atteignables avant la première requête, de
sorte que les dashboards peuvent utiliser `rate()` sans clause
`absent()`. Le set pré-chauffé compte 2 séries succès
(`action=ack` et `action=unack`) plus 13 séries d'échec (8 raisons
sur `action=ack`, 5 sur `action=unack`). Les combinaisons
impossibles (par exemple `action=ack,reason=not_acked` ou
`action=unack,reason=already_acked`) ne sont volontairement pas
pré-chauffées pour éviter de fausses séries.

**Exemples de requêtes**.

- `rate(perf_sentinel_ack_operations_total[5m])` : opérations ack et
  unack par seconde, utile pour les courbes de tendance.
- `sum by (reason) (rate(perf_sentinel_ack_operations_failed_total{action="ack"}[5m]))` :
  échecs ack par raison. Pic sur `unauthorized` qui indique une
  mauvaise configuration auth, pic sur `entry_too_large` qui pointe
  un client mal calibré (charges `by` / `reason` trop volumineuses),
  pic sur `limit_reached` ou `file_too_large` qui signale une
  saturation du store.

## Compteurs de scrape Scaphandre (depuis 0.5.25)

Émis par tick du scraper Scaphandre côté daemon (la tâche qui
récupère `scaph_process_power_consumption_microwatts` depuis
l'endpoint `[green.scaphandre]` configuré, toutes les
`scrape_interval_secs`). Enregistrés uniquement quand le daemon est
compilé avec la feature `daemon`.

| Metric                                    | Type  | Labels    | Description                                                                                                                                                           |
|-------------------------------------------|-------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_energy_backend_configured` | gauge | `backend` | `1` quand ce backend est configuré, `0` sinon. Positionné une fois au démarrage, une série par backend (`alumet`, `scaphandre`, `kepler`, `redfish`, `cloud_energy`). |

À lire avant toute autre métrique énergie. Chaque gauge énergie est
pré-enregistrée à zéro que le backend existe ou non, et un scrape réussi
remet `last_scrape_age_seconds` à zéro : « non configuré » et « configuré
et en parfaite santé » sont donc le même zéro plat sur le fil. Filtrez
sur cette gauge pour les distinguer, dans un dashboard
(`... and on(job, instance) (perf_sentinel_energy_backend_configured{backend="alumet"} == 1)`)
ou dans une alerte qui ne doit se déclencher que pour un backend
réellement en service.

| Metric                                             | Type    | Labels   | Description                                                                                  |
|----------------------------------------------------|---------|----------|----------------------------------------------------------------------------------------------|
| `perf_sentinel_scaphandre_scrape_total`            | counter | `status` | Total des tentatives de scrape Scaphandre depuis le démarrage, partitionné par issue.        |
| `perf_sentinel_scaphandre_scrape_failed_total`     | counter | `reason` | Total des scrapes Scaphandre en échec depuis le démarrage, partitionné par cause.            |
| `perf_sentinel_scaphandre_last_scrape_age_seconds` | gauge   | (aucun)  | Secondes depuis le dernier scrape réussi (remis à 0 sur succès). Canari pour scraper bloqué. |

Valeurs du label `status` : `success`, `failed`. Pré-chauffés à zéro
pour que les dashboards tracent un taux nul avant le premier scrape.

Valeurs du label `reason` :

- `unreachable` : échec transport bas niveau (connexion refusée,
  échec DNS, erreur TLS handshake, host down). L'endpoint n'est pas
  joignable depuis le pod du daemon.
- `timeout` : la deadline de 3 secondes sur l'appel HTTP par scrape a
  expiré avant la réponse.
- `http_error` : l'endpoint a répondu avec un statut non-2xx.
- `body_read_error` : erreur transport pendant le streaming du corps
  de réponse, après une lecture de statut réussie.
- `request_error` : hyper n'a pas réussi à construire la requête HTTP
  depuis l'URI (post-validation). Rare, indique un cas-limite de
  configuration que le parser d'URI a manqué.
- `invalid_utf8` : le corps de réponse n'est pas de l'UTF-8 valide.
  Scaphandre émet toujours du texte Prometheus ASCII-safe, donc
  presque toujours signe que l'endpoint n'est pas Scaphandre.

**Pré-chauffage**. Les deux compteurs émettent zéro pour chaque
valeur de label documentée avant le premier scrape, donc les
requêtes `rate()` n'ont pas besoin de garde `absent()`. L'ensemble
pré-chauffé est de 2 séries `status` plus 6 séries `reason`. Les
échecs de parsing de configuration (URI d'endpoint invalide)
abortent la tâche scraper au démarrage avant que le compteur soit
touché, ils ne sont visibles que dans les logs daemon au niveau
`error`.

**Exemples de requêtes**.

- `rate(perf_sentinel_scaphandre_scrape_total{status="success"}[5m])`
  divisé par `rate(perf_sentinel_scaphandre_scrape_total[5m])` :
  ratio de succès des scrapes sur 5 minutes. Utile pour un panel SLO
  ou une alerte (`< 0.95` sur 15 minutes signale un scraper dégradé).
- `topk(1, increase(perf_sentinel_scaphandre_scrape_failed_total[1h]))` :
  raison d'échec dominante sur l'heure écoulée. Un `unreachable`
  persistant pointe typiquement vers un exporteur Scaphandre absent
  du host, un `http_error` persistant vers un exporteur derrière un
  reverse proxy qui renvoie le mauvais statut, un `invalid_utf8`
  persistant vers un endpoint qui n'est pas Scaphandre du tout.

## Compteurs de scrape Kepler (depuis 0.7.4)

Émis par tick du scraper Kepler côté daemon (la tâche qui récupère
les séries `kepler_*_cpu_joules_total` depuis l'endpoint
`[green.kepler]` configuré). Enregistrés uniquement quand le daemon
est compilé avec la feature `daemon`. Le jeu de labels reflète celui
de Scaphandre parce que les deux sources rencontrent les six mêmes
modes d'échec HTTP.

| Metric                                         | Type    | Labels   | Description                                                                                     |
|------------------------------------------------|---------|----------|-------------------------------------------------------------------------------------------------|
| `perf_sentinel_kepler_scrape_total`            | counter | `status` | Total des tentatives de scrape Kepler depuis le démarrage, partitionné par issue.               |
| `perf_sentinel_kepler_scrape_failed_total`     | counter | `reason` | Total des scrapes Kepler en échec depuis le démarrage, partitionné par cause.                   |
| `perf_sentinel_kepler_last_scrape_age_seconds` | gauge   | (aucun)  | Secondes depuis la dernière HTTP 200 (remise à 0 sur toute HTTP 200, voir le piège ci-dessous). |

Les labels `status` et `reason` portent les six mêmes valeurs que les
compteurs Scaphandre ci-dessus (`success`/`failed`, et les six mêmes
causes d'échec HTTP), pré-chauffées à zéro avant le premier scrape.

**Piège de staleness zéro-échantillon**.
`perf_sentinel_kepler_last_scrape_age_seconds` est remise à 0 sur
chaque réponse HTTP 200, *y compris* une HTTP 200 dont le corps ne
contient aucune série Kepler v2 correspondante (le cas classique de la
montée v0.7.4 vers v0.7.5 où le cluster fait encore tourner un
Kepler < 0.10 avec les noms de métriques legacy). Les alertes pilotées
par la seule jauge ne détecteront pas ce scénario. Après trois ticks
HTTP 200 consécutifs sans échantillon correspondant, le daemon émet
une ligne `tracing::warn!` portant les champs `metric` et `label`.
Alertez plutôt sur le log, ou croisez la jauge avec
`rate(perf_sentinel_kepler_scrape_total{status="success"}[5m])` et la
présence du tag `co2.model` `kepler_ebpf` côté daemon. Deux messages
de warn distincts existent, un par cause, chacun avec son propre
streak warn-once : `no samples matched the configured metric` (noms
Kepler legacy ou `metric_kind` en désaccord avec la topologie) et
`none of the configured service_mappings label values were present`
(valeurs de mapping mal saisies, ou toutes les charges mappées
absentes de l'exposition). Les règles d'alerte par motif de log
doivent couvrir les deux. Les compteurs cumulatifs partageant une
valeur de label (un même nom de conteneur répété entre pods) sont
sommés avant le calcul du delta.

## Compteurs de scrape Alumet (depuis 0.9.12)

Même forme que le bloc Kepler ci-dessus avec `kepler` -> `alumet`
dans les noms de métriques (`perf_sentinel_alumet_scrape_total`,
`perf_sentinel_alumet_scrape_failed_total`,
`perf_sentinel_alumet_last_scrape_age_seconds`). Les jeux de labels
`status` et `reason` sont identiques : Alumet est scrapé en HTTP
simple avec les six mêmes modes d'échec, un seul panel de dashboard
peut donc agréger le taux des trois sources scrapées au format
Prometheus.

Le même piège de staleness zéro-échantillon s'applique, et il est plus
probable ici que pour Kepler : `metric_name` et `label_key` sont
fournis par l'opérateur (l'exporteur d'Alumet façonne les noms avec un
`prefix`/`suffix` configurable), donc une coquille ou un renommage
amont produit des HTTP 200 sans échantillon correspondant et une jauge
qui se remet sans cesse à 0. Le daemon avertit après trois ticks
consécutifs de ce type. Croisez la jauge avec
`rate(perf_sentinel_alumet_scrape_total{status="success"}[5m])` et la
présence du tag `co2.model` `alumet_rapl`.

Deux messages de warn distincts existent, un par cause, chacun avec son
propre streak warn-once : `no samples matched the configured metric`
(`metric_name` ou `label_key` faux sur le fil) et `none of the
configured service_mappings label values were present` (valeurs de
mapping mal saisies, ou toutes les charges mappées absentes de
l'exposition). Les règles d'alerte par motif de log doivent couvrir
les deux messages. Deux cas ne déclenchent aucun warn : une table de
mappings partiellement fausse (au moins une valeur correspond, les
autres jamais) et un label apparié dont les relevés sont en permanence
nuls ou invalides. Dans les deux cas, la vérification est
`per_service_energy_model` dans le rapport, qui montre le service sur
un tag proxy au lieu d'`alumet_rapl`.

Notez qu'aucune métrique ne peut détecter un `energy_interval_secs`
faux : les scrapes réussissent, les échantillons correspondent, seule
l'échelle est fausse. Voir
[docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md#limites-de-précision-alumet).

## Compteurs de scrape Redfish (depuis 0.7.4)

Même forme que le bloc Kepler ci-dessus avec `kepler` -> `redfish`
dans les noms de métriques. Le jeu de labels `reason` ajoute trois
valeurs propres à Redfish au set HTTP partagé : `invalid_json`,
`path_missing`, `invalid_value` pour les modes d'échec liés à la
variance JSON des BMC sur la réponse `/Power`.

## Metrics GreenOps

| Metric                                               | Type    | Labels    | Description                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
|------------------------------------------------------|---------|-----------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_io_waste_ratio`                       | gauge   | (aucun)   | Ratio I/O waste cumulatif (avoidable / total) depuis le démarrage. Utiliser `rate()` sur les counters sous-jacents pour des valeurs sur fenêtre.                                                                                                                                                                                                                                                                                                                                                                                          |
| `perf_sentinel_energy_kwh`                           | gauge   | (aucun)   | Énergie du workload sur la dernière fenêtre de scoring, kWh (depuis 0.8.8). Total scalaire seulement : le détail par service et par région reste hors `/metrics` (cardinalité) et vit sur les onglets Energy/Trends de `query monitor`.                                                                                                                                                                                                                                                                                                   |
| `perf_sentinel_carbon_gco2`                          | gauge   | (aucun)   | Carbone opérationnel de la dernière fenêtre de scoring, grammes CO2e, sommé sur les régions (depuis 0.8.8). Même logique scalaire que `perf_sentinel_energy_kwh`.                                                                                                                                                                                                                                                                                                                                                                         |
| `perf_sentinel_total_io_ops`                         | counter | (aucun)   | Total cumulatif d'ops I/O traitées.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `perf_sentinel_avoidable_io_ops`                     | counter | (aucun)   | Total cumulatif d'ops I/O évitables détectées.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `perf_sentinel_service_io_ops_total`                 | counter | `service`, `grouping` | Ops I/O cumulatives par service (lu par chaque scraper d'énergie mesurée pour l'attribution énergie par service). La cardinalité du label est plafonnée à 1024 services distincts par exécution du daemon, les nouveaux services au-delà du plafond ne sont pas attribués. Un span sans nom de service est résolu en `service="unknown"` à l'ingestion (sur les quatre chemins), plutôt que de créer un label vide, et `service="_other"` est réservé comme bucket de repli côté analyse : un vrai service portant ce nom y est fusionné. Depuis 0.19.0 il porte aussi `grouping`, la première valeur de `[detection] grouping_attributes` du span (vide s'il n'en a aucune), plafonné à 32 valeurs distinctes par run et replié dans `grouping="_other"` au-delà, contrairement à l'axe service du même counter, qui tronque, et avec `grouping="_other"` réservé de la même façon (un vrai regroupement portant ce nom fusionne dans le bac de repli sans faire bouger le counter de débordement) ; les scrapers d'énergie le lisent replié en un total par service. |
| `perf_sentinel_service_io_ops_overflow_total`        | counter | (aucun)   | Ops I/O non attribuées à un counter par service parce que le plafond de cardinalité de 1024 services était atteint (depuis 0.8.7). Une hausse continue signifie que le débit par service et l'attribution d'énergie mesurée sous-comptent les services nouvellement vus.                                                                                                                                                                                                                                                                  |
| `perf_sentinel_service_io_ops_grouping_overflow_total` | counter | (aucun) | Ops I/O dont le `grouping` s'est replié dans `_other` sur `perf_sentinel_service_io_ops_total` parce que le plafond d'ingestion de 32 regroupements était atteint (depuis 0.19.0). Rien n'est tronqué : les totaux par service restent exacts, seule la répartition par regroupement se dégrade. |
| `perf_sentinel_service_avoidable_io_ops_total`       | counter | `service`, `grouping` | Part par service de `perf_sentinel_avoidable_io_ops`, dérivée des findings avec la même règle de dédoublonnage (depuis 0.18.0). Les services au-delà du plafond d'analyse de 128 se replient dans `service="_other"`, donc la somme sur les services égale toujours le counter global. Divisez par `perf_sentinel_service_analyzed_io_ops_total` pour un waste ratio par service. Un finding dont les spans viennent de plusieurs services est réparti entre eux, chacun portant ses propres répétitions et le service du finding (le premier span dans l'ordre d'ingestion) une de moins pour l'appel nécessaire, au lieu d'être crédité en entier à un seul service. Le label `grouping` (depuis 0.19.0) est celui du finding : un finding dont les spans viennent de plusieurs services impute la part de chacun sous ce seul regroupement, ce qui est exact et non approximatif : les détecteurs N+1 et redondance partitionnent leurs groupes par l'identité du regroupement, donc chaque span imputé par un finding le porte déjà et le ratio avec `perf_sentinel_service_analyzed_io_ops_total` ne peut pas dépasser 1 ; ce que la répartition approxime, c'est quel service a fait l'unique appel nécessaire.                                                                                                                                                         |
| `perf_sentinel_service_analyzed_io_ops_total`        | counter | `service`, `grouping` | Ops I/O par service des traces analysées (depuis 0.18.0) : le dénominateur du waste ratio, issu de la même passe de scoring, du même plafond et du même gating green que le counter avoidable, contrairement à `perf_sentinel_service_io_ops_total` côté ingest (compté avant analyse, tronqué au-delà de 1024 services, incluant les lots délestés ensuite). Porte `grouping` depuis 0.19.0 sous le même plafond que le counter avoidable, donc le ratio par (service, grouping) ne mélange pas non plus deux populations.                                                                                                                                                                             |
| `perf_sentinel_scaphandre_last_scrape_age_seconds`   | gauge   | (aucun)   | Secondes depuis le dernier scrape Scaphandre réussi. Reste à 0 quand Scaphandre n'est pas configuré. Utile pour des alertes scraper bloqué.                                                                                                                                                                                                                                                                                                                                                                                               |
| `perf_sentinel_cloud_energy_last_scrape_age_seconds` | gauge   | (aucun)   | Même pattern pour le scraper cloud SPECpower.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `perf_sentinel_kepler_last_scrape_age_seconds`       | gauge   | (aucun)   | Même pattern pour le scraper Kepler. Voir le piège de staleness zéro-échantillon plus haut.                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `perf_sentinel_alumet_last_scrape_age_seconds`       | gauge   | (aucun)   | Même pattern pour le scraper Alumet. Voir le piège de staleness zéro-échantillon et la note `energy_interval_secs` plus haut.                                                                                                                                                                                                                                                                                                                                                                                                             |
| `perf_sentinel_redfish_last_scrape_age_seconds`      | gauge   | (aucun)   | Même pattern pour le scraper BMC Redfish.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |

## Kinds de warning : transitoire vs collant

`Report.warning_details` (depuis 0.5.19) compte cinq kinds stables,
chacun avec un cycle de vie différent. La distinction compte pour la
stratégie de monitoring : un warning transitoire se résout seul, un
collant persiste jusqu'au redémarrage du daemon.

| Kind                       | Cycle de vie | Émis quand                                                                                                                                                                                                     | Effacé par                                                                                                                                |
|----------------------------|--------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------|
| `cold_start`               | Transitoire  | `events_processed_total == 0` ou `traces_analyzed_total == 0` sur le daemon                                                                                                                                    | Premier batch réussi (les deux compteurs strictement positifs)                                                                            |
| `ingestion_drops`          | Collant      | `perf_sentinel_otlp_rejected_total{reason="channel_full" ou "memory_pressure"} > 0` depuis le démarrage                                                                                                        | Redémarrage du daemon (reset du compteur)                                                                                                 |
| `tuning`                   | Mixte        | Un compteur lifetime montre un réglage sous-dimensionné pour la charge, ou `sampling_rate` est sous 1.0 (voir dessous)                                                                                         | Redémarrage pour les règles à compteurs, baisse de charge pour la règle de fenêtre, un changement de config pour la règle `sampling_rate` |
| `unmatched_acknowledgment` | Par run      | Un acquittement actif n'a rien supprimé dans cette analyse                                                                                                                                                     | Le run suivant où son finding réapparaît, ou la suppression de l'acquittement                                                             |
| `snapshot_scope`           | Toujours     | Chaque réponse `/api/export/report` passé le démarrage à froid : les chiffres green décrivent un seul batch, et une seconde entrée apparaît quand le store contient plus de findings que l'export n'en expédie | Jamais, il décrit le payload plutôt qu'une panne. L'enveloppe de démarrage à froid et la sortie batch ne portent aucune des deux entrées  |

`cold_start` est un warning d'état : "le snapshot n'est pas
significatif maintenant". `ingestion_drops` est un warning d'audit :
"à un moment depuis le démarrage le canal a saturé, voici le count
pour le post-mortem". Acquitter des findings via l'API ack du daemon
n'efface aucun kind, ils reflètent l'état du daemon, pas la sortie de
détection.

### Le conseiller `tuning` (depuis 0.8.7)

Les entrées `tuning` sont des conseils de configuration : chaque
message nomme le réglage, sa valeur actuelle et l'ajustement suggéré.
Dix règles tournent à chaque appel `/api/export/report` :

| Déclencheur                                                                 | Réglage suggéré                                                                        |
|-----------------------------------------------------------------------------|----------------------------------------------------------------------------------------|
| `[daemon] sampling_rate < 1.0` (aucune métrique en jeu)                     | `[daemon] sampling_rate`, ou lire les comptes comme un échantillon                     |
| `perf_sentinel_otlp_rejected_total{reason="channel_full"} > 0`              | `[daemon] ingest_queue_capacity`                                                       |
| `perf_sentinel_otlp_rejected_total{reason="memory_pressure"} > 0`           | Limite mémoire du conteneur (le garde-fou borne la RSS)                                |
| `perf_sentinel_analysis_shed_batches_total > 0`                             | `[daemon] analysis_queue_capacity` ou plus de CPU                                      |
| `perf_sentinel_active_traces` à 90 % ou plus de `max_active_traces`         | `[daemon] max_active_traces` ou un `trace_ttl_ms` plus bas                             |
| `perf_sentinel_service_io_ops_overflow_total > 0`                           | Agréger ou réduire les noms de services (le plafond de 1024 séries est fixe)           |
| `perf_sentinel_analysis_service_overflow_total > 0` ou `perf_sentinel_slow_duration_service_overflow_total > 0` | Agréger ou réduire les noms de services (les plafonds de 128 et 64 séries sont fixes) |
| `perf_sentinel_analysis_grouping_overflow_total > 0`, `perf_sentinel_slow_duration_grouping_overflow_total > 0` ou `perf_sentinel_service_io_ops_grouping_overflow_total > 0` | Agréger ou réduire les valeurs de regroupement, ou raccourcir `[detection] grouping_attributes` (les plafonds de 16, 8 et 32 séries sont fixes) |
| `perf_sentinel_correlator_pairs_evicted_total > 0` avec corrélation activée | `[daemon.correlation] max_tracked_pairs`                                               |
| Tous les spans OTLP reçus filtrés comme non analysables (après 1000 spans)  | Corriger les attributs de spans ou pointer les services instrumentés vers cet endpoint |

Les règles à compteurs sont collantes (les compteurs lifetime ne se
réinitialisent qu'au redémarrage). La règle de fenêtre de traces lit
une gauge, elle apparaît et disparaît donc avec la charge. La règle
`sampling_rate` ne lit aucune métrique : elle se déclenche sur le
réglage seul, sur un daemon au repos comme sur un daemon chargé, parce
qu'un rapport samplé sous-estime ses comptes quelle que soit la charge.
Le message nomme les comptes et laisse les ratios de côté, puisqu'un
échantillonnage uniforme par trace touche numérateur et dénominateur de
la même façon et que le ratio de gaspillage I/O reste lisible, si bien
que le remettre à l'échelle produirait un nombre faux et non un nombre
corrigé. Un taux exactement égal à `0.0`, que la validation de config
accepte, reçoit son propre message. C'est la seule règle qui avertit sur
la façon de lire le rapport plutôt que sur un réglage que la charge a
dépassé. Le
conseiller lit le snapshot de config pris au démarrage du daemon, un
hint reflète donc toujours les valeurs réellement utilisées par le
process en cours.

Un sampling appliqué **avant** le daemon (un collector qui fait tourner
`tail_sampling` en amont) rétrécit les mêmes comptes et ne lève aucun
avertissement, parce qu'une trace conservée est indiscernable d'une
trace complète. Il est aussi pire pour les ratios : les politiques
`errors` et `slow` biaisent la rétention vers les traces lourdes, donc
l'échantillon survivant n'est pas représentatif comme l'est un hachage
uniforme. Voir
[HELM-DEPLOYMENT-FR.md](HELM-DEPLOYMENT-FR.md#sampling-du-collector-et-ce-qui-atteint-le-daemon)
pour la disposition de pipeline qui l'évite.

## Alertes

Le chart Helm rend ces règles sous forme de `PrometheusRule`, conditionnée par
`prometheusRule.enabled`. `PrometheusRule` est une CRD de l'opérateur
Prometheus, donc hors Kubernetes le même groupe se place dans une entrée
`rule_files` ordinaire. Le template du chart fait foi, ce bloc le reflète.

Chaque règle se déclenche sur une donnée que le daemon a perdue sans pouvoir la
retrouver. Rien ici n'alerte sur la saturation, la cardinalité de services ou de regroupements, ou
l'éviction du corrélateur : ce sont des états que le daemon atteint en
fonctionnant normalement, et chacun est un panneau du tableau de bord. Les
seuils portent un plancher pour la même raison, `rate(...) > 0` se déclenchant
sur un taux de perte d'une fraction de pourcent, ce qui apprend au lecteur à
couper la règle.

```yaml
groups:
  - name: perf-sentinel.rules
    rules:
      - alert: PerfSentinelDown
        expr: up{job="perf-sentinel"} == 0
        for: 15m
        labels: { severity: warning }
      - alert: PerfSentinelIngestRejecting
        expr: rate(perf_sentinel_otlp_rejected_total{reason="channel_full"}[10m]) > 0.05
        for: 15m
        labels: { severity: warning }
      - alert: PerfSentinelMemoryPressureRejecting
        expr: perf_sentinel_ingest_memory_pressure == 1
        for: 5m
        labels: { severity: warning }
      - alert: PerfSentinelAnalysisShedding
        expr: rate(perf_sentinel_analysis_shed_traces_total[10m]) > 1
        for: 15m
        labels: { severity: warning }
      - alert: PerfSentinelArchiveDropping
        expr: rate(perf_sentinel_archive_windows_dropped_total[15m]) > 0
        for: 15m
        labels: { severity: warning }
```

`job="perf-sentinel"` est le nom de job des extraits de collecte de
[INTEGRATION-FR.md](INTEGRATION-FR.md) et
[HELM-DEPLOYMENT-FR.md](HELM-DEPLOYMENT-FR.md). Sous le chart c'est le fullname
de la release, celui que l'opérateur dérive du Service.

Une règle de plus mérite d'exister sans être livrée active, parce qu'elle est
vraie et inutile sur une installation dont la flotte n'émet légitimement aucun
span d'I/O, et qu'une règle fausse dès le premier jour finit coupée pour
toujours. Activez-la une fois votre premier finding vu, avec une fenêtre
supérieure à votre plus longue période d'inactivité légitime :

```yaml
      - alert: PerfSentinelIngestingButProducingNothing
        expr: rate(perf_sentinel_otlp_spans_received_total[30m]) > 0
          and rate(perf_sentinel_events_processed_total[30m]) == 0
        for: 30m
        labels: { severity: warning }
```

C'est la panne où toutes les règles livrées restent vertes : une instrumentation
qui retire `db.statement` ou `http.url` transforme chaque requête en zéro
événement tout en répondant succès, et un lecteur en conclut qu'il n'y a aucun
problème de performance plutôt que de voir que rien n'a été mesuré.

## Références croisées

- Champ `Report.warning_details` (warnings de snapshot côté opérateur) :
  voir [RUNBOOK-FR.md](RUNBOOK-FR.md) section "Lire les warnings du Report".
- Workflow d'acquittements (suppression de findings cross-format) :
  voir [ACKNOWLEDGMENTS-FR.md](ACKNOWLEDGMENTS-FR.md).
- Emitter SARIF pour intégration CI : voir [SARIF-FR.md](SARIF-FR.md).
