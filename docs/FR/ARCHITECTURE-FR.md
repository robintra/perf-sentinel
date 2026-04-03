# Architecture

perf-sentinel est un détecteur polyglotte d'anti-patterns de performance, construit sous forme de workspace Rust avec deux crates :

- **sentinel-core** : bibliothèque contenant toute la logique du pipeline
- **sentinel-cli** : binaire fournissant le point d'entrée CLI (`perf-sentinel`)

## Vue d'ensemble du pipeline

```
                         +-----------+
                         |   Entrée  |
                         | JSON/OTLP |
                         +-----+-----+
                               |
                         SpanEvent[]
                               |
                        +------v------+
                        | Normaliser  |
                        |  sql / http |
                        +------+------+
                               |
                       NormalizedEvent[]
                               |
                        +------v------+
                        |  Corréler   |
                        | par trace_id|
                        +------+------+
                               |
                           Trace[]
                               |
                        +------v------+
                        |  Détecter   |
                        | n+1 / dup / |
                        | lent/fanout |
                        +------+------+
                               |
                          Finding[]
                               |
                        +------v------+
                        |   Scorer    |
                        |  GreenOps   |
                        |    CO2      |
                        +------+------+
                               |
                   Finding[] + GreenSummary
                               |
                        +------v-------+
                        |  Rapporter   |
                        |JSON/CLI/SARIF|
                        | / Prometheus |
                        +--------------+
```

## Modes de fonctionnement

### Mode batch (`perf-sentinel analyze`)

Traite un ensemble complet d'événements et produit un rapport unique avec évaluation du quality gate.

```
Vec<SpanEvent>
  -> normalize::normalize_all()        -> Vec<NormalizedEvent>
  -> correlate::correlate()            -> Vec<Trace>
  -> detect::detect()                  -> Vec<Finding>
  -> score::score_green()              -> (Vec<Finding>, GreenSummary)
  -> quality_gate::evaluate()          -> QualityGate
  -> Report { analysis, findings, green_summary, quality_gate }
```

En mode CI (`--ci`), le processus se termine avec le code 1 si le quality gate échoue.

### Mode streaming (`perf-sentinel watch`)

Fonctionne comme un daemon, recevant les événements en temps réel et émettant les findings au fur et à mesure de leur détection.

```
OTLP gRPC (port 4317)  \
OTLP HTTP (port 4318)   +---> canal mpsc ---> TraceWindow (LRU + TTL)
Socket unix JSON       /                               |
                                              +--------+--------+
                                              |                 |
                                        Éviction LRU    Éviction TTL
                                              |                 |
                                              +--------+--------+
                                                       |
                                          normalize -> detect -> score
                                                       |
                                              NDJSON findings (stdout)
                                              Prometheus /metrics
```

- Les événements sont normalisés en dehors du verrou TraceWindow pour minimiser le temps de détention du verrou.
- Les traces sont évincées lorsque le cache LRU est plein (`max_active_traces`) ou lorsque le TTL expire (`trace_ttl_ms`).
- À l'éviction, la trace est analysée via les étapes detect et score.
- Les findings sont émis en JSON délimité par des sauts de ligne sur stdout.
- Les métriques Prometheus sont exposées sur le même port HTTP (4318) à `/metrics`.

## Responsabilités des modules

| Module           | Chemin            | Responsabilité                                                                                                                                                                                                                                                                                                               |
|------------------|-------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **event**        | `event.rs`        | Type central `SpanEvent` (variantes SQL et HTTP) avec timestamp, IDs trace/span, service, opération, cible, durée                                                                                                                                                                                                            |
| **ingest**       | `ingest/`         | Sources d'entrée : parseur JSON avec auto-détection du format (`json.rs`), import Jaeger JSON (`jaeger.rs`), import Zipkin JSON v2 (`zipkin.rs`), récepteur OTLP gRPC+HTTP (`otlp.rs`). Implémente le trait `IngestSource`                                                                                                   |
| **normalize**    | `normalize/`      | Produit des `NormalizedEvent` avec template + paramètres extraits. Tokenizer SQL (`sql.rs`) : remplace les littéraux, UUIDs, listes IN. Normaliseur HTTP (`http.rs`) : remplace les segments numériques/UUID, supprime les paramètres de requête                                                                             |
| **correlate**    | `correlate/`      | Regroupe les événements par `trace_id`. Mode batch (`mod.rs`) : agrégation par HashMap. Mode streaming (`window.rs`) : cache LRU avec buffer circulaire par trace et éviction TTL                                                                                                                                            |
| **detect**       | `detect/`         | Détection de patterns sur les traces corrélées. N+1 (`n_plus_one.rs`) : même template, paramètres différents, dans une fenêtre. Redondant (`redundant.rs`) : même template et paramètres. Lent (`slow.rs`) : durée au-dessus du seuil avec template récurrent. Fanout (`fanout.rs`) : span parent avec trop de spans enfants |
| **score**        | `score/`          | Scoring GreenOps (`mod.rs`) : IIS par endpoint, ratio de gaspillage, top offenders, green_impact par finding. Conversion carbone (`carbon.rs`) : gCO2eq optionnel basé sur la région et table d'intensité embarquée                                                                                                          |
| **report**       | `report/`         | Formatage de sortie. Rapport JSON (`json.rs`), export SARIF v2.1.0 (`sarif.rs`), sortie CLI colorée (`mod.rs`), métriques Prometheus (`metrics.rs`)                                                                                                                                                                          |
| **quality_gate** | `quality_gate.rs` | Évalue des règles de seuils configurables par rapport aux findings et au résumé green                                                                                                                                                                                                                                        |
| **pipeline**     | `pipeline.rs`     | Connecte toutes les étapes pour le mode batch : normalize -> correlate -> detect -> score -> quality_gate -> Report                                                                                                                                                                                                          |
| **daemon**       | `daemon.rs`       | Boucle événementielle pour le mode streaming : serveurs d'ingestion, canal mpsc, gestion du TraceWindow, traitement des évictions                                                                                                                                                                                            |
| **time**         | `time.rs`         | Helpers de conversion timestamp partagés (`nanos_to_iso8601`, `micros_to_iso8601`). Utilisé par l'ingestion OTLP, Jaeger et Zipkin                                                                                                                                                                                           |
| **explain**      | `explain.rs`      | Visualiseur d'arbre de trace : construit l'arbre des spans à partir de `parent_span_id`, annote les findings. Sortie texte et JSON                                                                                                                                                                                           |
| **config**       | `config.rs`       | Parse `.perf-sentinel.toml` avec format sectionné ([thresholds], [detection], [green], [daemon]) et rétrocompatibilité avec le format plat legacy                                                                                                                                                                            |

## Types principaux

| Type              | Module           | Description                                                                                             |
|-------------------|------------------|---------------------------------------------------------------------------------------------------------|
| `SpanEvent`       | event            | Événement I/O brut (requête SQL ou appel HTTP) avec métadonnées et parent_span_id optionnel             |
| `NormalizedEvent` | normalize        | SpanEvent enrichi d'un template normalisé et de paramètres extraits                                     |
| `Trace`           | correlate        | Collection de NormalizedEvents partageant le même trace_id                                              |
| `Finding`         | detect           | Anti-pattern détecté avec type, sévérité, détails du pattern, timestamps et green_impact                |
| `GreenSummary`    | score            | Statistiques I/O agrégées : total ops, ops évitables, ratio de gaspillage, top offenders, CO2 optionnel |
| `QualityGate`     | quality_gate     | Résultat pass/fail avec évaluations individuelles des règles                                            |
| `Report`          | report           | Sortie d'analyse complète : métadonnées d'analyse, findings, résumé green, quality gate                 |
| `Config`          | config           | Configuration parsée avec toutes les sections et champs validés                                         |
| `TraceWindow`     | correlate/window | Cache LRU de traces actives pour le mode streaming avec éviction TTL                                    |

## Frontières des crates

```
sentinel-cli (binaire)
  |
  +-- CLI clap : sous-commandes analyze / explain / watch / demo / bench
  |
  +-- dépend de sentinel-core (bibliothèque)
        |
        +-- Toute la logique du pipeline
        +-- Traits uniquement aux frontières : IngestSource, ReportSink
        +-- Fonctions pures entre les étapes
```

Le crate CLI est intentionnellement léger : il parse les arguments, charge la configuration, et délègue aux fonctions de sentinel-core. Toute la logique métier réside dans sentinel-core.
