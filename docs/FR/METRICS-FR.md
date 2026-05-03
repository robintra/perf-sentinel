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
