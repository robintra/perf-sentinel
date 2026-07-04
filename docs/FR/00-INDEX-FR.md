# Index de la documentation

Ce répertoire contient le miroir français de la documentation utilisateur de perf-sentinel. Pour la documentation de conception approfondie destinée aux contributeurs, voir le sous-répertoire [`design/`](design/00-INDEX-FR.md).

La version anglaise de chaque document se trouve dans le répertoire parent [`docs/`](../00-INDEX.md).

## Prise en main

| Document                                       | Description                                                                                |
|------------------------------------------------|--------------------------------------------------------------------------------------------|
| [ARCHITECTURE-FR.md](ARCHITECTURE-FR.md)       | Vue d'ensemble du pipeline, responsabilités des modules, types clés                        |
| [INSTRUMENTATION-FR.md](INSTRUMENTATION-FR.md) | Configuration OTLP par langage : Java, Quarkus, .NET, Go, Python, Node.js, Rust            |
| [CI-FR.md](CI-FR.md)                           | Mode CI, recettes GitHub Actions / GitLab CI / Jenkins, détection de régression sur PR     |

## Déploiement

| Document                                       | Description                                                                                |
|------------------------------------------------|--------------------------------------------------------------------------------------------|
| [INTEGRATION-FR.md](INTEGRATION-FR.md)         | Quatre topologies de déploiement (batch, sidecar, gateway, autonome) et démarrages rapides |
| [HELM-DEPLOYMENT-FR.md](HELM-DEPLOYMENT-FR.md) | Déploiement Kubernetes via le chart Helm, référence des values, TLS, RBAC                  |

## Référence

| Document                                   | Description                                                                                                               |
|--------------------------------------------|---------------------------------------------------------------------------------------------------------------------------|
| [CONFIGURATION-FR.md](CONFIGURATION-FR.md) | Référence complète `.perf-sentinel.toml` (seuils, détection, GreenOps, daemon)                                            |
| [CLI-FR.md](CLI-FR.md)                     | Référence des sous-commandes (`analyze`, `watch`, `report`, `diff`, `query`, `ack`, `inspect`, `disclose`, `verify-hash`) |
| [METRICS-FR.md](METRICS-FR.md)             | Métriques Prometheus exposées par le daemon sur `/metrics`                                                                |
| [QUERY-API-FR.md](QUERY-API-FR.md)         | API HTTP du daemon (`/api/findings`, `/api/correlations`, `/api/explain/{trace}`, `/api/status`)                          |
| [SARIF-FR.md](SARIF-FR.md)                 | Format de sortie SARIF v2.1.0 pour l'intégration IDE et GitHub Code Scanning                                              |
| [SCHEMA-FR.md](SCHEMA-FR.md)               | Schéma JSON du rapport de divulgation périodique (`perf-sentinel-report v1.3`)                                            |

## Fonctionnalités

| Document                                       | Description                                                                                                                           |
|------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| [HTML-REPORT-FR.md](HTML-REPORT-FR.md)         | Dashboard HTML autonome, mode live via `--daemon-url`, ack/revoke depuis le navigateur                                                |
| [INSPECT-FR.md](INSPECT-FR.md)                 | TUI interactif : drill-down Analyze/Inspect/Explain (`analyze --tui`, `inspect`, `explain --tui`), flèches ou touches vim, ack/revoke |
| [ACKNOWLEDGMENTS-FR.md](ACKNOWLEDGMENTS-FR.md) | Format `.perf-sentinel-acknowledgments.toml`, signatures SHA-256, règles de filtrage                                                  |
| [ACK-WORKFLOW-FR.md](ACK-WORKFLOW-FR.md)       | Workflow d'acquittement de bout en bout : API daemon, CLI, TUI et rapport HTML                                                        |
| [REPORTING-FR.md](REPORTING-FR.md)             | Divulgation publique périodique via `perf-sentinel disclose`, versionnement du schéma, vérification de hash                           |

## Exploitation

| Document                                       | Description                                                                         |
|------------------------------------------------|-------------------------------------------------------------------------------------|
| [RUNBOOK-FR.md](RUNBOOK-FR.md)                 | Runbook d'incident : dépannage orienté symptôme pour les déploiements en production |
| [METHODOLOGY-FR.md](METHODOLOGY-FR.md)         | Chaîne de calcul des traces vers `efficiency_score`, `energy_kwh`, `carbon_kgco2eq` |
| [LIMITATIONS-FR.md](LIMITATIONS-FR.md)         | Compromis connus, contraintes amont, limites de la détection                        |

## Chaîne d'approvisionnement et release

| Document                                           | Description                                                                                 |
|----------------------------------------------------|---------------------------------------------------------------------------------------------|
| [SUPPLY-CHAIN-FR.md](SUPPLY-CHAIN-FR.md)           | Épinglage des entrées de build, signature Sigstore, provenance SLSA, chaîne `verify-hash`   |
| [RELEASE-PROCEDURE-FR.md](RELEASE-PROCEDURE-FR.md) | Procédure de release de bout en bout depuis la 0.7.0, gate lab de simulation, lockstep Helm |

## Sous-répertoires

| Répertoire                         | Contenu                                                                            |
|------------------------------------|------------------------------------------------------------------------------------|
| [`design/`](design/00-INDEX-FR.md) | Documentation de conception approfondie (10 chapitres), destinée aux contributeurs |
