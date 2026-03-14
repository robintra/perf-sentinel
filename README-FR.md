# perf-sentinel

Détecteur léger et polyglotte d'anti-patterns de performance.

Analyse les traces d'exécution (requêtes SQL, appels HTTP) pour détecter les requêtes N+1, les appels redondants, et évalue l'intensité I/O par endpoint (GreenOps).

## Démarrage rapide

> Bientôt disponible.

## Feuille de route

| Phase | Description                                          | Statut       |
|-------|------------------------------------------------------|--------------|
| **0** | Scaffolding — workspace compilable, CI, stubs        | ✅ Terminé    |
| **1** | Détection N+1 SQL + HTTP, normalisation, corrélation | Non commencé |
| **2** | Scoring GreenOps, ingestion OTLP, quality gate CI    | Non commencé |
| **3** | Polish, benchmarks, release v0.1.0                   | Non commencé |

## Licence

Ce projet est sous licence [GNU Affero General Public License v3.0](LICENSE).
