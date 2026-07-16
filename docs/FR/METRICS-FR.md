# Référence des metrics exposées

Ce document liste toutes les metrics exposées par le daemon perf-sentinel
sur `/metrics` (format Prometheus text). L'endpoint sert à la fois
`text/plain; version=0.0.4` (Prometheus historique) et
`application/openmetrics-text; version=1.0.0` (OpenMetrics) via content
negotiation, et émet des exemplars quand des `trace_id` sont disponibles
côté findings.

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
filtre I/O délibéré (seuls les spans SQL et HTTP sortants sont
analysables, voir [`LIMITATIONS-FR.md`](./LIMITATIONS-FR.md)). Une
flotte dont l'instrumentation supprime `db.statement` ou `http.url`
convertit chaque requête en zéro événement alors que les requêtes
continuent de répondre en succès, et seule cette paire de counters
rend cela visible : `perf_sentinel_otlp_spans_received_total` qui
monte pendant que `perf_sentinel_events_processed_total` reste plat
signifie que les spans arrivent mais qu'aucun ne porte d'attribut
analysable. Valeurs du label `reason` de
`perf_sentinel_otlp_spans_filtered_total`, pré-warmées à 0 :

- `not_io` : le span ne porte ni statement `db.*` ni url ou méthode
  HTTP (span interne, hit de cache, middleware...). Dominant attendu
  sur les flottes bien instrumentées.
- `missing_db_statement` : le span a `db.system` mais ni
  `db.statement` ni `db.query.text`. Typique des drivers configurés
  pour omettre le texte des requêtes.
- `missing_http_url` : le span a une méthode HTTP mais ni `http.url`
  ni `url.full`.
- `non_sql_datastore` : le span nomme un store non-SQL (Redis,
  MongoDB, ...) dans `db.system`. Écarté à dessein, pas un manque
  d'instrumentation (voir [`LIMITATIONS-FR.md`](./LIMITATIONS-FR.md)).
- `merged_db_span` : span DB fusionné dans l'événement unique d'une
  requête qu'une instrumentation en couches a scindée en plusieurs
  spans (statement sur l'un, durée sur l'autre, par exemple PHP
  Doctrine + PDO). La requête reste analysée, ce n'est pas non plus un
  manque d'instrumentation.

## Metrics d'analyse et de findings

| Metric                                         | Type      | Labels             | Description                                                                                                                                                                                                                                                                                           |
|------------------------------------------------|-----------|--------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_findings_total`                 | counter   | `type`, `severity` | Findings détectés depuis le démarrage. `type` reflète l'enum `Finding.finding_type`, `severity` vaut `critical` / `warning` / `info`. Porte des exemplars OpenMetrics quand un `trace_id` est disponible.                                                                                             |
| `perf_sentinel_traces_analyzed_total`          | counter   | (aucun)            | Compte cumulatif de traces traitées par l'event loop.                                                                                                                                                                                                                                                 |
| `perf_sentinel_events_processed_total`         | counter   | (aucun)            | Compte cumulatif d'events traités par l'event loop.                                                                                                                                                                                                                                                   |
| `perf_sentinel_active_traces`                  | gauge     | (aucun)            | Traces actuellement actives dans la fenêtre glissante.                                                                                                                                                                                                                                                |
| `perf_sentinel_analysis_queue_depth`           | gauge     | (aucun)            | Lots en attente dans la file du worker d'analyse (incrémenté à l'enfilement, décrémenté quand le worker prend un lot). Une valeur non nulle durable signifie que detect+score prend du retard sur l'ingestion.                                                                                        |
| `perf_sentinel_stored_findings`                | gauge     | (aucun)            | Findings actuellement retenus dans le ring buffer de la query API (depuis 0.8.8). À apparier avec `perf_sentinel_max_retained_findings` pour un ratio de headroom.                                                                                                                                    |
| `perf_sentinel_max_active_traces`              | gauge     | (aucun)            | Plafond configuré de la fenêtre glissante (`[daemon] max_active_traces`), positionné une fois au démarrage (depuis 0.8.8). À apparier avec `perf_sentinel_active_traces`. Le conseiller de réglages alerte à 90 %.                                                                                    |
| `perf_sentinel_analysis_queue_capacity`        | gauge     | (aucun)            | Plafond configuré de la file du worker d'analyse (`[daemon] analysis_queue_capacity`), positionné une fois au démarrage (depuis 0.8.8). À apparier avec `perf_sentinel_analysis_queue_depth`.                                                                                                         |
| `perf_sentinel_max_retained_findings`          | gauge     | (aucun)            | Plafond configuré du ring buffer de findings (`[daemon] max_retained_findings`), positionné une fois au démarrage (depuis 0.8.8). À apparier avec `perf_sentinel_stored_findings`.                                                                                                                    |
| `perf_sentinel_analysis_shed_batches_total`    | counter   | (aucun)            | Lots d'analyse délestés parce que la file du worker était pleine ou que le worker s'est arrêté. Remplace le drop implicite précédent : chaque délestage est compté ici. Alerte sur `rate(...) > 0`.                                                                                                   |
| `perf_sentinel_analysis_shed_traces_total`     | counter   | (aucun)            | Traces abandonnées par les lots délestés comptés dans `perf_sentinel_analysis_shed_batches_total`.                                                                                                                                                                                                    |
| `perf_sentinel_correlator_pairs_evicted_total` | counter   | (aucun)            | Paires du corrélateur inter-traces évincées par le plafond `max_tracked_pairs` (depuis 0.8.7). Un taux soutenu signifie que la topologie de corrélation dépasse le plafond et que les paires les moins comptées sont recyclées, `/api/correlations` peut donc perdre des entrées entre deux lectures. |
| `perf_sentinel_slow_duration_seconds`          | histogram | `type`             | Histogramme de durée pour les spans dépassant le seuil slow, par `type` (`sql` ou `http_out`). Buckets : 0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 5, 10, 30 secondes. Utilisé par `histogram_quantile()` Grafana pour des percentiles précis sur des déploiements daemon shardés.                          |
| `perf_sentinel_export_report_requests_total`   | counter   | (aucun)            | Total des requêtes `GET /api/export/report`. Inclut les réponses cold-start (200 avec enveloppe vide).                                                                                                                                                                                                |

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

| Metric                                         | Type    | Labels   | Description                                                                                       |
|------------------------------------------------|---------|----------|-----------------------------------------------------------------------------------------------------|
| `perf_sentinel_kepler_scrape_total`            | counter | `status` | Total des tentatives de scrape Kepler depuis le démarrage, partitionné par issue.                 |
| `perf_sentinel_kepler_scrape_failed_total`     | counter | `reason` | Total des scrapes Kepler en échec depuis le démarrage, partitionné par cause.                     |
| `perf_sentinel_kepler_last_scrape_age_seconds` | gauge   | (aucun)  | Secondes depuis la dernière HTTP 200 (remise à 0 sur toute HTTP 200, voir le piège ci-dessous).   |

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
présence du tag `co2.model` `kepler_ebpf` côté daemon.

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

Notez qu'aucune métrique ne peut détecter un `energy_interval_secs`
faux : les scrapes réussissent, les échantillons correspondent, seule
l'échelle est fausse. Voir
`docs/FR/LIMITATIONS-FR.md#limites-de-précision-alumet`.

## Compteurs de scrape Redfish (depuis 0.7.4)

Même forme que le bloc Kepler ci-dessus avec `kepler` -> `redfish`
dans les noms de métriques. Le jeu de labels `reason` ajoute trois
valeurs propres à Redfish au set HTTP partagé : `invalid_json`,
`path_missing`, `invalid_value` pour les modes d'échec liés à la
variance JSON des BMC sur la réponse `/Power`.

## Metrics GreenOps

| Metric                                               | Type    | Labels    | Description                                                                                                                                                                                                                                                                                 |
|------------------------------------------------------|---------|-----------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_io_waste_ratio`                       | gauge   | (aucun)   | Ratio I/O waste cumulatif (avoidable / total) depuis le démarrage. Utiliser `rate()` sur les counters sous-jacents pour des valeurs sur fenêtre.                                                                                                                                            |
| `perf_sentinel_energy_kwh`                           | gauge   | (aucun)   | Énergie du workload sur la dernière fenêtre de scoring, kWh (depuis 0.8.8). Total scalaire seulement : le détail par service et par région reste hors `/metrics` (cardinalité) et vit sur les onglets Energy/Trends de `query monitor`.                                                     |
| `perf_sentinel_carbon_gco2`                          | gauge   | (aucun)   | Carbone opérationnel de la dernière fenêtre de scoring, grammes CO2e, sommé sur les régions (depuis 0.8.8). Même logique scalaire que `perf_sentinel_energy_kwh`.                                                                                                                           |
| `perf_sentinel_total_io_ops`                         | counter | (aucun)   | Total cumulatif d'ops I/O traitées.                                                                                                                                                                                                                                                         |
| `perf_sentinel_avoidable_io_ops`                     | counter | (aucun)   | Total cumulatif d'ops I/O évitables détectées.                                                                                                                                                                                                                                              |
| `perf_sentinel_service_io_ops_total`                 | counter | `service` | Ops I/O cumulatives par service (lu par chaque scraper d'énergie mesurée pour l'attribution énergie par service). La cardinalité du label est plafonnée à 1024 services distincts par exécution du daemon, les nouveaux services au-delà du plafond ne sont pas attribués.                  |
| `perf_sentinel_service_io_ops_overflow_total`        | counter | (aucun)   | Ops I/O non attribuées à un counter par service parce que le plafond de cardinalité de 1024 services était atteint (depuis 0.8.7). Une hausse continue signifie que le débit par service et l'attribution d'énergie mesurée sous-comptent les services nouvellement vus.                    |
| `perf_sentinel_scaphandre_last_scrape_age_seconds`   | gauge   | (aucun)   | Secondes depuis le dernier scrape Scaphandre réussi. Reste à 0 quand Scaphandre n'est pas configuré. Utile pour des alertes scraper bloqué.                                                                                                                                                 |
| `perf_sentinel_cloud_energy_last_scrape_age_seconds` | gauge   | (aucun)   | Même pattern pour le scraper cloud SPECpower.                                                                                                                                                                                                                                               |
| `perf_sentinel_kepler_last_scrape_age_seconds`       | gauge   | (aucun)   | Même pattern pour le scraper Kepler. Voir le piège de staleness zéro-échantillon plus haut.                                                                                                                                                                                                 |
| `perf_sentinel_alumet_last_scrape_age_seconds`       | gauge   | (aucun)   | Même pattern pour le scraper Alumet. Voir le piège de staleness zéro-échantillon et la note `energy_interval_secs` plus haut.                                                                                                                                                              |
| `perf_sentinel_redfish_last_scrape_age_seconds`      | gauge   | (aucun)   | Même pattern pour le scraper BMC Redfish.                                                                                                                                                                                                                                                   |

## Kinds de warning : transitoire vs collant

`Report.warning_details` (depuis 0.5.19) compte trois kinds stables,
chacun avec un cycle de vie différent. La distinction compte pour la
stratégie de monitoring : un warning transitoire se résout seul, un
collant persiste jusqu'au redémarrage du daemon.

| Kind              | Cycle de vie | Émis quand                                                                                              | Effacé par                                                                         |
|-------------------|--------------|---------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------|
| `cold_start`      | Transitoire  | `events_processed_total == 0` ou `traces_analyzed_total == 0` sur le daemon                             | Premier batch réussi (les deux compteurs strictement positifs)                     |
| `ingestion_drops` | Collant      | `perf_sentinel_otlp_rejected_total{reason="channel_full" ou "memory_pressure"} > 0` depuis le démarrage | Redémarrage du daemon (reset du compteur)                                          |
| `tuning`          | Mixte        | Un compteur lifetime montre un réglage sous-dimensionné pour la charge (voir dessous)                   | Redémarrage pour les règles à compteurs, baisse de charge pour la règle de fenêtre |

`cold_start` est un warning d'état : "le snapshot n'est pas
significatif maintenant". `ingestion_drops` est un warning d'audit :
"à un moment depuis le démarrage le canal a saturé, voici le count
pour le post-mortem". Acquitter des findings via l'API ack du daemon
n'efface aucun kind, ils reflètent l'état du daemon, pas la sortie de
détection.

### Le conseiller `tuning` (depuis 0.8.7)

Les entrées `tuning` sont des conseils de configuration : chaque
message nomme le réglage, sa valeur actuelle et l'ajustement suggéré.
Sept règles tournent à chaque appel `/api/export/report` :

| Déclencheur                                                                 | Réglage suggéré                                                                        |
|-----------------------------------------------------------------------------|----------------------------------------------------------------------------------------|
| `perf_sentinel_otlp_rejected_total{reason="channel_full"} > 0`              | `[daemon] ingest_queue_capacity`                                                       |
| `perf_sentinel_otlp_rejected_total{reason="memory_pressure"} > 0`           | Limite mémoire du conteneur (le garde-fou borne la RSS)                                |
| `perf_sentinel_analysis_shed_batches_total > 0`                             | `[daemon] analysis_queue_capacity` ou plus de CPU                                      |
| `perf_sentinel_active_traces` à 90 % ou plus de `max_active_traces`         | `[daemon] max_active_traces` ou un `trace_ttl_ms` plus bas                             |
| `perf_sentinel_service_io_ops_overflow_total > 0`                           | Agréger ou réduire les noms de services (le plafond de 1024 séries est fixe)           |
| `perf_sentinel_correlator_pairs_evicted_total > 0` avec corrélation activée | `[daemon.correlation] max_tracked_pairs`                                               |
| Tous les spans OTLP reçus filtrés comme non analysables (après 1000 spans)  | Corriger les attributs de spans ou pointer les services instrumentés vers cet endpoint |

Les règles à compteurs sont collantes (les compteurs lifetime ne se
réinitialisent qu'au redémarrage). La règle de fenêtre de traces lit
une gauge, elle apparaît et disparaît donc avec la charge. Le
conseiller lit le snapshot de config pris au démarrage du daemon, un
hint reflète donc toujours les valeurs réellement utilisées par le
process en cours.

## Références croisées

- Champ `Report.warning_details` (warnings de snapshot côté opérateur) :
  voir [RUNBOOK-FR.md](RUNBOOK-FR.md) section "Lire les warnings du Report".
- Workflow d'acquittements (suppression de findings cross-format) :
  voir [ACKNOWLEDGMENTS-FR.md](ACKNOWLEDGMENTS-FR.md).
- Emitter SARIF pour intégration CI : voir [SARIF-FR.md](SARIF-FR.md).
