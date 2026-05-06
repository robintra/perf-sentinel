# Référence des metrics exposées

Ce document liste toutes les metrics exposées par le daemon perf-sentinel
sur `/metrics` (format Prometheus text). L'endpoint sert à la fois
`text/plain; version=0.0.4` (Prometheus historique) et
`application/openmetrics-text; version=1.0.0` (OpenMetrics) via content
negotiation, et émet des exemplars quand des `trace_id` sont disponibles
côté findings.

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

| Metric                              | Type    | Labels   | Description                                                                                     |
|-------------------------------------|---------|----------|-------------------------------------------------------------------------------------------------|
| `perf_sentinel_otlp_rejected_total` | counter | `reason` | Total des requêtes OTLP rejetées par le daemon depuis le démarrage, par raison (depuis 0.5.19). |

Valeurs du label `reason` :

- `unsupported_media_type` (HTTP uniquement) : `Content-Type` n'est pas
  `application/x-protobuf`. perf-sentinel n'implémente pas la variante
  OTLP encodée en JSON.
- `parse_error` (HTTP uniquement) : décodage protobuf raté.
- `channel_full` (HTTP et gRPC) : le canal d'événements est saturé ou
  fermé et le daemon n'a pas pu enqueuer le batch. La voie HTTP renvoie
  503, la voie gRPC renvoie `INTERNAL`.

Les 3 reasons sont pré-warmées à 0 au démarrage pour que les dashboards
puissent plotter la ligne zéro avant le premier rejet.

`payload_too_large` n'est **pas** comptabilisé par cette metric.
Tower-http (`RequestBodyLimitLayer`) côté HTTP et tonic
(`max_decoding_message_size`) côté gRPC appliquent la limite en amont
et renvoient 413 / `RESOURCE_EXHAUSTED` avant que le handler applicatif
ne tourne. Les opérateurs préoccupés par la taille de payload doivent
monitorer les logs du proxy ou de la gateway upstream, ou câbler un
counter de rejet tower-http dans leur stack.

## Metrics d'analyse et de findings

| Metric                                       | Type      | Labels             | Description                                                                                                                                                                                                                                                                  |
|----------------------------------------------|-----------|--------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_findings_total`               | counter   | `type`, `severity` | Findings détectés depuis le démarrage. `type` reflète l'enum `Finding.finding_type`, `severity` vaut `critical` / `warning` / `info`. Porte des exemplars OpenMetrics quand un `trace_id` est disponible.                                                                    |
| `perf_sentinel_traces_analyzed_total`        | counter   | (aucun)            | Compte cumulatif de traces traitées par l'event loop.                                                                                                                                                                                                                        |
| `perf_sentinel_events_processed_total`       | counter   | (aucun)            | Compte cumulatif d'events traités par l'event loop.                                                                                                                                                                                                                          |
| `perf_sentinel_active_traces`                | gauge     | (aucun)            | Traces actuellement actives dans la fenêtre glissante.                                                                                                                                                                                                                       |
| `perf_sentinel_slow_duration_seconds`        | histogram | `type`             | Histogramme de durée pour les spans dépassant le seuil slow, par `type` (`sql` ou `http_out`). Buckets : 0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 5, 10, 30 secondes. Utilisé par `histogram_quantile()` Grafana pour des percentiles précis sur des déploiements daemon shardés. |
| `perf_sentinel_export_report_requests_total` | counter   | (aucun)            | Total des requêtes `GET /api/export/report`. Inclut les réponses cold-start (200 avec enveloppe vide).                                                                                                                                                                       |

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

Issue par tick du scraper Scaphandre cote daemon (la tache qui
recupere `scaph_process_power_consumption_microwatts` depuis
l'endpoint `[green.scaphandre]` configure, toutes les
`scrape_interval_secs`). Enregistres uniquement quand le daemon est
compile avec la feature `daemon`.

| Metric                                          | Type    | Labels    | Description                                                                                       |
|-------------------------------------------------|---------|-----------|---------------------------------------------------------------------------------------------------|
| `perf_sentinel_scaphandre_scrape_total`         | counter | `status`  | Total des tentatives de scrape Scaphandre depuis le demarrage, partitionne par issue.             |
| `perf_sentinel_scaphandre_scrape_failed_total`  | counter | `reason`  | Total des scrapes Scaphandre en echec depuis le demarrage, partitionne par cause.                 |
| `perf_sentinel_scaphandre_last_scrape_age_seconds` | gauge | (aucun)   | Secondes depuis le dernier scrape reussi (remis a 0 sur succes). Canari pour scraper bloque.      |

Valeurs du label `status` : `success`, `failed`. Pre-chauffes a zero
pour que les dashboards plotent un taux nul avant le premier scrape.

Valeurs du label `reason` :

- `unreachable` : echec transport bas niveau (connexion refusee, echec
  DNS, erreur TLS handshake, host down). L'endpoint n'est pas
  joignable depuis le pod du daemon.
- `timeout` : la deadline de 3 secondes sur l'appel HTTP par scrape a
  expire avant la reponse.
- `http_error` : l'endpoint a repondu avec un statut non-2xx.
- `body_read_error` : erreur transport pendant le streaming du corps
  de reponse, apres une lecture de statut reussie.
- `request_error` : hyper n'a pas reussi a construire la requete HTTP
  depuis l'URI (post-validation). Rare, indique un cas-limite de
  configuration que le parser d'URI a manque.
- `invalid_utf8` : le corps de reponse n'est pas de l'UTF-8 valide.
  Scaphandre emet toujours du texte Prometheus ASCII-safe, donc
  presque toujours signe que l'endpoint n'est pas Scaphandre.

**Pre-chauffage**. Les deux compteurs emettent zero pour chaque
valeur de label documentee avant le premier scrape, donc les
requetes `rate()` n'ont pas besoin de garde `absent()`. L'ensemble
pre-chauffe est de 2 series `status` plus 6 series `reason`. Les
echecs de parsing de configuration (URI d'endpoint invalide) abortent
la tache scraper au demarrage avant que le compteur soit touche, ils
ne sont visibles que dans les logs daemon au niveau `error`.

**Exemples de requetes**.

- `rate(perf_sentinel_scaphandre_scrape_total{status="success"}[5m])`
  divise par `rate(perf_sentinel_scaphandre_scrape_total[5m])` :
  ratio de succes des scrapes sur 5 minutes. Utile pour un panel SLO
  ou une alerte (`< 0.95` sur 15 minutes signale un scraper degrade).
- `topk(1, increase(perf_sentinel_scaphandre_scrape_failed_total[1h]))` :
  raison d'echec dominante sur l'heure ecoulee. Un `unreachable`
  persistant pointe typiquement vers un exporteur Scaphandre absent
  du host, un `http_error` persistant vers un exporteur derriere un
  reverse proxy qui renvoie le mauvais statut, un `invalid_utf8`
  persistant vers un endpoint qui n'est pas Scaphandre du tout.

## Metrics GreenOps

| Metric                                               | Type    | Labels    | Description                                                                                                                                      |
|------------------------------------------------------|---------|-----------|--------------------------------------------------------------------------------------------------------------------------------------------------|
| `perf_sentinel_io_waste_ratio`                       | gauge   | (aucun)   | Ratio I/O waste cumulatif (avoidable / total) depuis le démarrage. Utiliser `rate()` sur les counters sous-jacents pour des valeurs sur fenêtre. |
| `perf_sentinel_total_io_ops`                         | counter | (aucun)   | Total cumulatif d'ops I/O traitées.                                                                                                              |
| `perf_sentinel_avoidable_io_ops`                     | counter | (aucun)   | Total cumulatif d'ops I/O évitables détectées.                                                                                                   |
| `perf_sentinel_service_io_ops_total`                 | counter | `service` | Ops I/O cumulatives par service (lu par le scraper Scaphandre pour l'attribution énergie par service).                                           |
| `perf_sentinel_scaphandre_last_scrape_age_seconds`   | gauge   | (aucun)   | Secondes depuis le dernier scrape Scaphandre réussi. Reste à 0 quand Scaphandre n'est pas configuré. Utile pour des alertes scraper bloqué.      |
| `perf_sentinel_cloud_energy_last_scrape_age_seconds` | gauge   | (aucun)   | Même pattern pour le scraper cloud SPECpower.                                                                                                    |

## Références croisées

- Champ `Report.warning_details` (warnings de snapshot côté opérateur) :
  voir [RUNBOOK-FR.md](RUNBOOK-FR.md) section "Lire les warnings du Report".
- Workflow d'acquittements (suppression de findings cross-format) :
  voir [ACKNOWLEDGMENTS-FR.md](ACKNOWLEDGMENTS-FR.md).
- Emitter SARIF pour intégration CI : voir [SARIF-FR.md](SARIF-FR.md).
