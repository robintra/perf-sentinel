# Index de la documentation de conception

Ce répertoire contient la documentation de conception approfondie de perf-sentinel. Ces documents expliquent **pourquoi** chaque décision a été prise, pas seulement ce que le code fait. Ils sont destinés aux contributeurs et mainteneurs qui ont besoin de comprendre les raisons derrière l'implémentation.

Pour la documentation orientée utilisateur, voir le répertoire parent `docs/FR/` :
- [ARCHITECTURE-FR.md](../ARCHITECTURE-FR.md) : vue d'ensemble du pipeline et responsabilités des modules
- [CONFIGURATION-FR.md](../CONFIGURATION-FR.md) : référence complète `.perf-sentinel.toml`
- [LIMITATIONS-FR.md](../LIMITATIONS-FR.md) : compromis connus
- [INTEGRATION-FR.md](../INTEGRATION-FR.md) : vue d'ensemble des topologies et démarrages rapides
- [INSTRUMENTATION-FR.md](../INSTRUMENTATION-FR.md) : guides de configuration OTLP (Java, Quarkus, .NET, Rust)
- [CI-FR.md](../CI-FR.md) : mode CI, recettes GitHub Actions / GitLab CI / Jenkins

## Table des matières

| Document                                                            | Sujets                                                                                                                                                                                         |
|---------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [01 : Pipeline et types](01-PIPELINE-AND-TYPES-FR.md)               | Pipeline vs architecture hexagonale, chaîne de types, découpage en workspace, sortie déterministe, quality gate                                                                                |
| [02 : Normalisation](02-NORMALIZATION-FR.md)                        | Machine à états SQL, normaliseur HTTP, micro-optimisations (batch push, saut IN-list, UUID codé à la main)                                                                                     |
| [03 : Corrélation et streaming](03-CORRELATION-AND-STREAMING-FR.md) | Groupement batch par HashMap, cache LRU, buffer circulaire, éviction TTL, budget mémoire                                                                                                       |
| [04 : Détection](04-DETECTION-FR.md)                                | Algorithmes de détection N+1, redondant et lent, clés empruntées, fenêtre basée sur les itérateurs                                                                                             |
| [05 : GreenOps et carbone](05-GREENOPS-AND-CARBON-FR.md)            | Formule IIS, dédup du ratio de gaspillage, conversion CO2, alignement SCI                                                                                                                      |
| [06 : Ingestion et daemon](06-INGESTION-AND-DAEMON-FR.md)           | Conversion OTLP, boucle événementielle du daemon, échantillonnage, renforcement sécurité                                                                                                       |
| [07 : CLI, config et release](07-CLI-CONFIG-RELEASE-FR.md)          | Sous-commandes bench, query, report, diff. Sink dashboard HTML, export CSV, hash deep-link, modal cheatsheet, raccourcis clavier style vim. Parsing de la config, profil release, distribution |
| [08 : Rapport public périodique](08-PERIODIC-DISCLOSURE-FR.md)      | Déterminisme du schéma v1.0, granularité G1/G2, validator collect-all, attribution par service, writer d'archive daemon, dispatcher CLI `disclose`                                             |
| [09 : Attribution carbone](09-CARBON-ATTRIBUTION-FR.md)             | Énergie + carbone par service au scoring, attribution de la région, précédence des modèles, branchement runtime-vs-proxy dans l'aggregator                                                     |

## Correspondance avec les fichiers source

| Fichier source                 | Document de conception                                                                                  |
|--------------------------------|---------------------------------------------------------------------------------------------------------|
| `lib.rs`                       | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)                                                            |
| `event.rs`                     | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)                                                            |
| `pipeline.rs`                  | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)                                                            |
| `quality_gate.rs`              | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)                                                            |
| `normalize/sql.rs`             | [02 : Normalisation](02-NORMALIZATION-FR.md)                                                            |
| `normalize/http.rs`            | [02 : Normalisation](02-NORMALIZATION-FR.md)                                                            |
| `normalize/mod.rs`             | [02 : Normalisation](02-NORMALIZATION-FR.md)                                                            |
| `correlate/mod.rs`             | [03 : Corrélation](03-CORRELATION-AND-STREAMING-FR.md)                                                  |
| `correlate/window.rs`          | [03 : Corrélation](03-CORRELATION-AND-STREAMING-FR.md)                                                  |
| `detect/mod.rs`                | [04 : Détection](04-DETECTION-FR.md)                                                                    |
| `detect/n_plus_one.rs`         | [04 : Détection](04-DETECTION-FR.md)                                                                    |
| `detect/redundant.rs`          | [04 : Détection](04-DETECTION-FR.md)                                                                    |
| `detect/slow.rs`               | [04 : Détection](04-DETECTION-FR.md)                                                                    |
| `score/mod.rs`                 | [05 : GreenOps](05-GREENOPS-AND-CARBON-FR.md), [09 : Attribution carbone](09-CARBON-ATTRIBUTION-FR.md)  |
| `score/carbon.rs`              | [05 : GreenOps](05-GREENOPS-AND-CARBON-FR.md)                                                           |
| `score/carbon_compute.rs`      | [05 : GreenOps](05-GREENOPS-AND-CARBON-FR.md), [09 : Attribution carbone](09-CARBON-ATTRIBUTION-FR.md)  |
| `score/region_breakdown.rs`    | [05 : GreenOps](05-GREENOPS-AND-CARBON-FR.md)                                                           |
| `ingest/mod.rs`                | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `ingest/json.rs`               | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `ingest/otlp.rs`               | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `ingest/pg_stat.rs`            | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/mod.rs`                | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/event_loop.rs`         | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/listeners.rs`          | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/tls.rs`                | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/json_socket.rs`        | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/sampling.rs`           | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/findings_store.rs`     | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `daemon/query_api.rs`          | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `config.rs`                    | [07 : CLI/Config](07-CLI-CONFIG-RELEASE-FR.md), [08 : Rapport périodique](08-PERIODIC-DISCLOSURE-FR.md) |
| `report/mod.rs`, `json.rs`     | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)                                                            |
| `report/metrics.rs`            | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)                                                         |
| `report/periodic/*`            | [08 : Rapport périodique](08-PERIODIC-DISCLOSURE-FR.md)                                                 |
| `daemon/archive.rs`            | [08 : Rapport périodique](08-PERIODIC-DISCLOSURE-FR.md)                                                 |
| `sentinel-cli/src/main.rs`     | [07 : CLI/Config](07-CLI-CONFIG-RELEASE-FR.md)                                                          |
| `sentinel-cli/src/disclose.rs` | [08 : Rapport périodique](08-PERIODIC-DISCLOSURE-FR.md)                                                 |
| `sentinel-cli/src/tui.rs`      | [07 : CLI/Config](07-CLI-CONFIG-RELEASE-FR.md)                                                          |
| `detect/correlate_cross.rs`    | [04 : Détection](04-DETECTION-FR.md)                                                                    |
