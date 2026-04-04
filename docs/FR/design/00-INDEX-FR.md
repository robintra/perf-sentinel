# Index de la documentation de conception

Ce répertoire contient la documentation de conception approfondie de perf-sentinel. Ces documents expliquent **pourquoi** chaque décision a été prise, pas seulement ce que le code fait. Ils sont destinés aux contributeurs et mainteneurs qui ont besoin de comprendre les raisons derrière l'implémentation.

Pour la documentation orientée utilisateur, voir le répertoire parent `docs/FR/` :
- [ARCHITECTURE-FR.md](../ARCHITECTURE-FR.md) : vue d'ensemble du pipeline et responsabilités des modules
- [CONFIGURATION-FR.md](../CONFIGURATION-FR.md) : référence complète `.perf-sentinel.toml`
- [LIMITATIONS-FR.md](../LIMITATIONS-FR.md) : compromis connus
- [INTEGRATION-FR.md](../INTEGRATION-FR.md) : guides de configuration OTLP (Java, .NET, Rust)

## Table des matières

| Document                                                            | Sujets                                                                                                          |
|---------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------|
| [01 : Pipeline et types](01-PIPELINE-AND-TYPES-FR.md)               | Pipeline vs architecture hexagonale, chaîne de types, découpage en workspace, sortie déterministe, quality gate |
| [02 : Normalisation](02-NORMALIZATION-FR.md)                        | Machine à états SQL, normaliseur HTTP, micro-optimisations (batch push, saut IN-list, UUID codé à la main)      |
| [03 : Corrélation et streaming](03-CORRELATION-AND-STREAMING-FR.md) | Groupement batch par HashMap, cache LRU, buffer circulaire, éviction TTL, budget mémoire                        |
| [04 : Détection](04-DETECTION-FR.md)                                | Algorithmes de détection N+1, redondant et lent, clés empruntées, fenêtre basée sur les itérateurs              |
| [05 : GreenOps et carbone](05-GREENOPS-AND-CARBON-FR.md)            | Formule IIS, dédup du ratio de gaspillage, conversion CO2, alignement SCI                                       |
| [06 : Ingestion et daemon](06-INGESTION-AND-DAEMON-FR.md)           | Conversion OTLP, boucle événementielle du daemon, échantillonnage, renforcement sécurité                        |
| [07 : CLI, config et release](07-CLI-CONFIG-RELEASE-FR.md)          | Commande bench, parsing de la config, profil release, distribution                                              |

## Correspondance avec les fichiers source

| Fichier source             | Document de conception                                 |
|----------------------------|--------------------------------------------------------|
| `lib.rs`                   | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)           |
| `event.rs`                 | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)           |
| `pipeline.rs`              | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)           |
| `quality_gate.rs`          | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)           |
| `normalize/sql.rs`         | [02 : Normalisation](02-NORMALIZATION-FR.md)           |
| `normalize/http.rs`        | [02 : Normalisation](02-NORMALIZATION-FR.md)           |
| `normalize/mod.rs`         | [02 : Normalisation](02-NORMALIZATION-FR.md)           |
| `correlate/mod.rs`         | [03 : Corrélation](03-CORRELATION-AND-STREAMING-FR.md) |
| `correlate/window.rs`      | [03 : Corrélation](03-CORRELATION-AND-STREAMING-FR.md) |
| `detect/mod.rs`            | [04 : Détection](04-DETECTION-FR.md)                   |
| `detect/n_plus_one.rs`     | [04 : Détection](04-DETECTION-FR.md)                   |
| `detect/redundant.rs`      | [04 : Détection](04-DETECTION-FR.md)                   |
| `detect/slow.rs`           | [04 : Détection](04-DETECTION-FR.md)                   |
| `score/mod.rs`             | [05 : GreenOps](05-GREENOPS-AND-CARBON-FR.md)          |
| `score/carbon.rs`          | [05 : GreenOps](05-GREENOPS-AND-CARBON-FR.md)          |
| `ingest/mod.rs`            | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)        |
| `ingest/json.rs`           | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)        |
| `ingest/otlp.rs`           | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)        |
| `ingest/pg_stat.rs`        | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)        |
| `daemon.rs`                | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)        |
| `config.rs`                | [07 : CLI/Config](07-CLI-CONFIG-RELEASE-FR.md)         |
| `report/mod.rs`, `json.rs` | [01 : Pipeline](01-PIPELINE-AND-TYPES-FR.md)           |
| `report/metrics.rs`        | [06 : Ingestion](06-INGESTION-AND-DAEMON-FR.md)        |
| `sentinel-cli/src/main.rs` | [07 : CLI/Config](07-CLI-CONFIG-RELEASE-FR.md)         |
| `sentinel-cli/src/tui.rs`  | [07 : CLI/Config](07-CLI-CONFIG-RELEASE-FR.md)         |
