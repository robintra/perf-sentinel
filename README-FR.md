<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frobintra%2Fperf-sentinel%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&suffix=%20stable&label=rust%202024&color=D34516&logo=rust" alt="Rust" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml/badge.svg" alt="Security Audit" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
  <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-dark-horizontal.svg">
  <img alt="perf-sentinel" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-horizontal.svg">
</picture>

Analyse les traces d'exécution (requêtes SQL, appels HTTP) pour détecter les requêtes N+1, les appels redondants, et évalue l'intensité I/O par endpoint (GreenOps).

## Pourquoi perf-sentinel ?

Les anti-patterns de performance comme les requêtes N+1 existent dans toute application qui fait des I/O — monolithes comme microservices. Dans les architectures distribuées, un appel utilisateur cascade sur plusieurs services, chacun avec ses propres I/O, et personne n'a de visibilité sur le chemin complet. Les outils existants sont soit spécifiques à un runtime (Hypersistence Utils -> JPA uniquement), soit lourds et propriétaires (Datadog, New Relic), soit limités aux tests unitaires sans vision cross-service.

perf-sentinel adopte une approche différente : **l'analyse au niveau protocole**. Il observe les traces produites par l'application (requêtes SQL, appels HTTP) quel que soit le langage ou l'ORM utilisé. Il n'a pas besoin de comprendre JPA, EF Core ou SeaORM — il voit les requêtes qu'ils génèrent.

## GreenOps : scoring éco-conception intégré

Chaque finding inclut un **I/O Intensity Score (IIS)** : le nombre d'opérations I/O générées par requête utilisateur pour un endpoint donné. Réduire les I/O inutiles (N+1, appels redondants) améliore les temps de réponse *et* réduit la consommation énergétique — ce ne sont pas des objectifs concurrents.

- **I/O Intensity Score** = opérations I/O totales pour un endpoint / nombre d'invocations
- **I/O Waste Ratio** = opérations I/O évitables (issues des findings) / opérations I/O totales

Aligné avec la composante **Energy** du [modèle SCI (ISO/IEC 21031:2024)](https://github.com/Green-Software-Foundation/sci) de la Green Software Foundation.

## Positionnement

| Critère              | [Hypersistence Utils](https://github.com/vladmihalcea/hypersistence-utils) | [Datadog APM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Digma](https://digma.ai/) | **perf-sentinel** |
|----------------------|----------------------------------------------------------------------------|-------------------------------------------------------|-----------------------------------------------------------------------|----------------------------|-------------------|
| Détection N+1 SQL    | ✅ JPA uniquement                                                           | ✅ (runtime)                                           | ✅ (runtime)                                                           | ✅ (JVM)                    | ✅ Polyglotte      |
| Détection N+1 HTTP   | ❌                                                                          | ✅                                                     | ✅                                                                     | ⚠️ Partiel                 | ✅                 |
| Polyglotte           | ❌ Java/JPA                                                                 | ✅ (agents par langage)                                | ✅ (agents par langage)                                                | ❌ JVM                      | ✅ Protocol-level  |
| Cross-service        | ❌                                                                          | ✅                                                     | ✅                                                                     | ⚠️ Partiel                 | ✅ Trace ID        |
| Angle GreenOps / SCI | ❌                                                                          | ❌                                                     | ❌                                                                     | ❌                          | ✅ Natif           |
| Léger                | N/A (lib)                                                                  | ❌ (~150 Mo)                                           | ❌ (~150 Mo)                                                           | ❌ (~100 Mo)                | ✅ (<10 Mo RSS)    |
| Open-source          | ✅ MIT                                                                      | ❌                                                     | ⚠️ Free tier limité                                                   | ⚠️ Freemium                | ✅ AGPL v3         |
| CI/CD quality gate   | ⚠️ (assertions manuelles)                                                  | ❌                                                     | ⚠️ (alertes, pas de gate natif)                                       | ⚠️                         | ✅ Natif           |

## Que remonte-t-il ?

Pour chaque anti-pattern détecté, perf-sentinel remonte :

- **Type :** N+1 SQL, N+1 HTTP, ou requête redondante
- **Template normalisé :** la requête ou l'URL avec les paramètres remplacés par des placeholders (`?`, `{id}`)
- **Occurrences :** combien de fois le pattern s'est déclenché dans la fenêtre de détection
- **Endpoint source :** quel endpoint applicatif l'a généré (ex : `GET /api/orders`)
- **Suggestion :** par exemple *"batch cette requête"* ou *"utilise un batch endpoint"*
- **Impact GreenOps :** estimation des I/O évitables et I/O Intensity Score

```
$ perf-sentinel demo

=== perf-sentinel demo ===
Analyzed 14 events across 2 traces in 2ms

Found 2 issue(s):

  [WARNING] #1 N+1 HTTP
    Trace:    trace-demo-game
    Service:  game
    Endpoint: POST /api/game/42/start
    Template: GET /api/account/{id}
    Hits:     6 occurrences, 6 distinct params, 250ms window
    Window:   2025-07-10T14:32:01.300Z → 2025-07-10T14:32:01.550Z
    Suggestion: Use batch endpoint with ?ids=... to batch 6 calls into one
    Extra I/O: 5 avoidable ops
    IIS:      12.0

  [WARNING] #2 N+1 SQL
    Trace:    trace-demo-game
    Service:  game
    Endpoint: POST /api/game/42/start
    Template: SELECT * FROM player WHERE game_id = ?
    Hits:     6 occurrences, 6 distinct params, 250ms window
    Window:   2025-07-10T14:32:01.000Z → 2025-07-10T14:32:01.250Z
    Suggestion: Use WHERE ... IN (?) to batch 6 queries into one
    Extra I/O: 5 avoidable ops
    IIS:      12.0

--- GreenOps Summary ---
  Total I/O ops:     14
  Avoidable I/O ops: 10
  I/O waste ratio:   71.4%

  Top offenders:
    - POST /api/game/42/start: IIS 12.0, 12.0 I/O ops/req (service: game)
    - GET /api/users/1: IIS 2.0, 2.0 I/O ops/req (service: user-svc)

Quality gate: FAILED
```

En mode batch/CI (`perf-sentinel analyze`), la sortie est un rapport JSON structuré :

<details>
<summary>Exemple de rapport JSON</summary>

```json
{
  "analysis": {
    "duration_ms": 1,
    "events_processed": 6,
    "traces_analyzed": 1
  },
  "findings": [
    {
      "type": "n_plus_one_sql",
      "severity": "warning",
      "trace_id": "trace-n1-sql",
      "service": "game",
      "source_endpoint": "POST /api/game/42/start",
      "pattern": {
        "template": "SELECT * FROM player WHERE game_id = ?",
        "occurrences": 6,
        "window_ms": 250,
        "distinct_params": 6
      },
      "suggestion": "Use WHERE ... IN (?) to batch 6 queries into one",
      "first_timestamp": "2025-07-10T14:32:01.000Z",
      "last_timestamp": "2025-07-10T14:32:01.250Z",
      "green_impact": {
        "estimated_extra_io_ops": 5,
        "io_intensity_score": 6.0
      }
    }
  ],
  "green_summary": {
    "total_io_ops": 6,
    "avoidable_io_ops": 5,
    "io_waste_ratio": 0.833,
    "top_offenders": [
      {
        "endpoint": "POST /api/game/42/start",
        "service": "game",
        "io_intensity_score": 6.0,
        "io_ops_per_request": 6.0
      }
    ]
  },
  "quality_gate": {
    "passed": false,
    "rules": [
      { "rule": "n_plus_one_sql_critical_max", "threshold": 0.0, "actual": 0.0, "passed": true },
      { "rule": "n_plus_one_http_warning_max", "threshold": 3.0, "actual": 0.0, "passed": true },
      { "rule": "io_waste_ratio_max", "threshold": 0.3, "actual": 0.833, "passed": false }
    ]
  }
}
```

</details>

## Démarrage rapide

> Bientôt disponible.

## Feuille de route

| Phase | Description                                          | Statut     |
|-------|------------------------------------------------------|------------|
| **0** | Scaffolding : workspace compilable, CI, stubs        | ✅ Terminé  |
| **1** | Détection N+1 SQL + HTTP, normalisation, corrélation | ✅ Terminé  |
| **2** | Scoring GreenOps, ingestion OTLP, quality gate CI    | ✅ Terminé  |
| **3** | Polish, benchmarks, release v0.1.0                   | ⏳ En cours |

## Licence

Ce projet est sous licence [GNU Affero General Public License v3.0](LICENSE).

