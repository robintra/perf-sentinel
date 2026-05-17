<p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frobintra%2Fperf-sentinel%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&suffix=%20stable&label=rust%202024&color=D34516&logo=rust" alt="Rust" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml/badge.svg" alt="Security Audit" /></a>
    <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
    <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/release.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/release.yml/badge.svg" alt="Release" /></a>
    <a href="https://artifacthub.io/packages/helm/perf-sentinel/perf-sentinel"><img src="https://img.shields.io/endpoint?url=https://artifacthub.io/badge/repository/perf-sentinel" alt="Artifact Hub" /></a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-dark-horizontal.svg">
  <img alt="perf-sentinel" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-horizontal.svg">
</picture>

**Détecte les anti-patterns d'I/O (N+1, appels redondants, SQL/HTTP lents, fanout) dans les traces OpenTelemetry. S'utilise comme quality gate CI sur traces capturées, ou comme daemon OTLP long-running avec métriques Prometheus et API de query.**

> **À lire en premier**
> - **Prérequis :** vos services doivent émettre des **traces OpenTelemetry** (spans SQL + HTTP). Sinon, perf-sentinel n'a rien à analyser. Voir [docs/INSTRUMENTATION.md](docs/INSTRUMENTATION.md) pour la mise en place (Java/Quarkus/.NET/Rust).
> - **Ce que c'est :** un détecteur d'anti-patterns auto-hébergé, mono-binaire (`<15 Mo RSS`), utilisable en batch sur traces capturées (exploration locale, post-mortem, ou quality gate CI avec exit 1 sur dépassement de seuil) ou en mode daemon long-running (ingestion OTLP, API de query, dashboard live, métriques Prometheus).
> - **Ce que ce n'est *pas* :** un APM complet, un profiler continu, ni une plateforme de comptabilité carbone réglementaire standalone. Voir [Ce que perf-sentinel n'est pas](#ce-que-perf-sentinel-nest-pas).

---

## Aperçu rapide

Terminal :

```bash
perf-sentinel analyze --input traces.json
```

![demo](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/demo.gif)

Tableau de bord HTML (un seul fichier offline) :

```bash
perf-sentinel report --input traces.json --output report.html
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/dashboard_dark.gif">
  <img alt="tour du dashboard" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/dashboard_light.gif">
</picture>

## Pourquoi perf-sentinel ?

Les anti-patterns de performance comme les N+1 existent dans toute application qui fait des I/O, monolithes comme microservices. Dans les architectures distribuées, un appel utilisateur cascade sur plusieurs services, chacun avec ses propres I/O, et personne n'a de visibilité sur le chemin complet.

Les outils existants résolvent chacun une partie du problème : Hypersistence Utils ne couvre que JPA ; Datadog et New Relic sont des agents propriétaires lourds qu'on ne veut pas forcément déployer dans tous les pipelines ; les détecteurs de Sentry sont solides mais liés à son SDK et son backend. Aucun ne propose un **quality gate CI au niveau protocole, auto-hébergeable**.

perf-sentinel observe les traces que votre application émet déjà (requêtes SQL, appels HTTP), quel que soit le langage ou l'ORM. Il n'a pas besoin de comprendre JPA, EF Core ou SeaORM : il voit les requêtes qu'ils génèrent.

## Ce qui est détecté

Dix types de findings, plus la corrélation cross-trace en mode daemon :

| Pattern            | Déclencheur                                                         |
|--------------------|---------------------------------------------------------------------|
| N+1 SQL            | Même template de requête tiré ≥ N fois dans une trace               |
| N+1 HTTP           | Même template d'URL appelé ≥ N fois dans une trace                  |
| SQL redondant      | Requête identique avec paramètres identiques, même trace            |
| HTTP redondant     | Appel identique avec paramètres identiques, même trace              |
| SQL lent           | Durée de requête au-dessus du seuil configuré                       |
| HTTP lent          | Durée de requête au-dessus du seuil configuré                       |
| Fanout excessif    | Un span démarre ≥ N enfants en parallèle                            |
| Service bavard     | Service A → B de manière répétée dans une seule requête utilisateur |
| Saturation de pool | Requêtes concurrentes en vol au-dessus de la taille du pool         |
| Appels sérialisés  | I/O séquentiels qui pourraient être parallélisés                    |

Chaque finding embarque : type, sévérité, template normalisé, occurrences, endpoint source, suggestion, localisation source (quand les spans OTel portent les attributs `code.*`) et impact GreenOps (voir plus bas). Pour les règles de sévérité et seuils ajustables, voir [docs/design/04-DETECTION.md](docs/design/04-DETECTION.md).

## Formats de sortie

- **`text`** (défaut) : sortie terminal colorée, regroupée par sévérité. Disponible sur `analyze`, `diff`, `pg-stat`, `query`, `explain`, `ack`.
- **`json`** : rapport structuré. Disponible sur `analyze`, `diff`, `pg-stat`, `query`, `explain`, `ack`. Schéma complet dans [docs/SCHEMA.md](docs/SCHEMA.md), exemples dans [docs/schemas/examples/](docs/schemas/examples/).
- **`sarif`** (SARIF v2.1.0) : code scanning GitHub/GitLab, annotations PR inline via `physicalLocations`. Disponible sur `analyze` et `diff`. Voir [docs/SARIF.md](docs/SARIF.md).
- **Dashboard HTML** : rapport offline en un seul fichier depuis `perf-sentinel report`, navigation dans les arbres de traces, thème clair/sombre, export CSV sur les onglets Findings / pg_stat / Diff / Correlations. Voir [docs/HTML-REPORT.md](docs/HTML-REPORT.md).
- **TUI interactif** : vue 3 panneaux pilotée au clavier depuis `perf-sentinel inspect` (ou `query inspect` pour données live du daemon). Voir [docs/INSPECT.md](docs/INSPECT.md).
- **Daemon live** : findings NDJSON sur stdout, `/metrics` Prometheus avec Grafana Exemplars, sonde `/health`, API HTTP de query. Voir [docs/METRICS.md](docs/METRICS.md) et [docs/QUERY-API.md](docs/QUERY-API.md).
- **Disclosure périodique (optionnel)** : JSON `perf-sentinel-report/v1.0` vérifiable par hash depuis `perf-sentinel disclose`, signable via Sigstore. Voir [docs/REPORTING.md](docs/REPORTING.md).

Les valeurs d'enum `io_intensity_band` / `io_waste_ratio_band` (`healthy` / `moderate` / `high` / `critical`) sont stables entre versions, les seuils numériques sous-jacents peuvent évoluer. Tableau de référence et explication dans [docs/LIMITATIONS.md#score-interpretation](docs/LIMITATIONS.md#score-interpretation).

## Performance

| Métrique                                | Résultat (v0.6.1)              |
|-----------------------------------------|--------------------------------|
| Débit pic pipeline                      | **> 1,8 M évènements / sec**   |
| Débit soutenu end-to-end                | **≈ 960 k évènements / sec**   |
| Mémoire résidente sous charge soutenue  | **< 250 Mo**                   |

Le chiffre `<15 Mo RSS` cité dans le TL;DR et le tableau comparatif correspond à l'**empreinte daemon stationnaire à faible trafic** (apples-to-apples avec les chiffres "agent idle" listés pour les autres outils). Sous la charge soutenue de ~960 k évts/s ci-dessus, le même daemon reste **sous 250 Mo**.

Mesuré sur un Mac Mini M4 Pro (12 cœurs, 24 Go de mémoire unifiée, macOS 26.4.1), build release `aarch64-unknown-linux-musl` avec `mimalloc`, dans un conteneur Docker Desktop `linux/arm64` (VM 15,6 Go). Édition Rust 2024, rustc 1.95.0 stable. Reproduire avec `perf-sentinel bench --help`.

## Installation

```bash
# depuis crates.io
cargo install perf-sentinel

# ou télécharger un binaire pré-construit (Linux amd64/arm64, macOS arm64, Windows amd64)
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64 && sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel

# ou via Docker
docker run --rm -p 4317:4317 -p 4318:4318 \
  ghcr.io/robintra/perf-sentinel:latest watch --listen-address 0.0.0.0
```

Les binaires Linux ciblent musl (statiques, fonctionnent sur n'importe quelle distribution et dans les images `FROM scratch`). Un chart Helm est disponible sous [`charts/perf-sentinel/`](charts/perf-sentinel/). Voir [docs/HELM-DEPLOYMENT.md](docs/HELM-DEPLOYMENT.md).

## Déploiement

Quatre environnements, trois modèles de déploiement. Mise en place complète dans [docs/INTEGRATION.md](docs/INTEGRATION.md) ; recettes CI dans [docs/CI.md](docs/CI.md) ; métriques Prometheus dans [docs/METRICS.md](docs/METRICS.md) ; exemple sidecar dans [`examples/docker-compose-sidecar.yml`](examples/docker-compose-sidecar.yml).

Modèles : **batch CI** (`analyze --ci` sur traces capturées, exit 1 sur dépassement de seuil), **collector central** (un OTel Collector route vers le daemon `watch`, métriques Prometheus et API de query), **sidecar** (un daemon par service pour du debug isolé).

<details>
<summary><b>Dev local</b></summary>

![Zoom dev local : batch sur trace capturée, daemon local sur 127.0.0.1, TUI inspect, rapport HTML](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-local-dev.svg)

</details>

<details>
<summary><b>CI/CD</b></summary>

![Zoom CI : tests d'intégration perf + quality gate analyze --ci, SARIF pour code scanning, Tempo / jaeger-query nightly optionnel](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-CI.svg)

</details>

<details>
<summary><b>Staging</b></summary>

![Zoom staging : pod focus-service avec daemon sidecar, /api/findings interrogé par QA / SRE](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-staging.svg)

</details>

<details>
<summary><b>Production</b></summary>

![Zoom production : daemon centralisé ingérant via OTel Collector et OTLP direct, /api/* + /metrics + NDJSON](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-production.svg)

</details>

<details>
<summary><b>GreenOps (transversal)</b></summary>

![Intégration GreenOps : sources externes temps réel (Scaphandre en kWh, Electricity Maps en gCO2/kWh) plus sources internes froides (Cloud SPECpower en kWh, carbone embarqué en gCO2e/req via Boavizta + HotCarbon 2024, transport réseau en kWh/GB via Mytton 2024) alimentant perf-sentinel en mode batch ou daemon, émettant énergie et carbone en parallèle des traces](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-GreenOps.svg)

</details>

<details>
<summary>Vue d'ensemble : comment les quatre environnements s'articulent</summary>

![Intégration globale de perf-sentinel à travers dev local, CI, staging et prod](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/global-integration.svg)

</details>

Le dépôt compagnon [perf-sentinel-simulation-lab](https://github.com/robintra/perf-sentinel-simulation-lab/blob/main/docs/SCENARIOS.md) valide huit modes opérationnels de bout en bout sur un vrai cluster Kubernetes, chacun avec un diagramme Mermaid, les entrées/sorties exactes et les pièges rencontrés lors de la validation.

## Démarrage rapide

```bash
# 1. Essayer la démo intégrée (aucune installation côté apps)
perf-sentinel demo

# 2. Analyser un fichier de traces capturées
perf-sentinel analyze --input traces.json

# 3. L'utiliser comme quality gate CI (exit 1 si seuil dépassé)
perf-sentinel analyze --input traces.json --ci --config .perf-sentinel.toml

# 4. Streamer les traces de vos apps (mode daemon)
perf-sentinel watch
```

`.perf-sentinel.toml` minimal à la racine du repo :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # tolérance zéro N+1 SQL
io_waste_ratio_max = 0.30          # max 30% d'I/O évitables

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500
```

Référence complète des sous-commandes : `perf-sentinel <cmd> --help`, ou [docs/CLI.md](docs/CLI.md).

<details>
<summary>Carte des sous-commandes perf-sentinel et des artefacts consommés ou produits</summary>

<img alt="Vue d'ensemble des commandes CLI" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/cli-commands.svg">

</details>

<details>
<summary>Cheat sheet en une ligne pour le reste de la surface</summary>

```bash
perf-sentinel explain --input traces.json --trace-id abc123        # vue arbre d'une trace
perf-sentinel inspect --input traces.json                          # TUI interactif
perf-sentinel diff --before base.json --after head.json            # diff de régression PR
perf-sentinel pg-stat --input pg_stat.csv --traces traces.json     # hotspots PostgreSQL
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id <id>   # récup depuis Grafana Tempo
perf-sentinel jaeger-query --endpoint http://jaeger:16686 --service order-svc
perf-sentinel calibrate --traces traces.json --measured-energy rapl.csv
perf-sentinel completions zsh > ~/.zfunc/_perf-sentinel            # complétion shell
perf-sentinel query findings --service order-svc                   # dialoguer avec un daemon
```

</details>

## GreenOps : score d'intensité I/O (directionnel)

Chaque finding embarque un **score d'intensité I/O (IIS)**, total des ops I/O d'un endpoint divisé par le nombre d'invocations, et un **ratio de gaspillage I/O** (ops évitables / ops totales). Réduire les N+1 et appels redondants améliore les temps de réponse *et* la consommation d'énergie ; ces deux objectifs ne s'opposent pas.

`co2.total` est reporté comme le numérateur [Software Carbon Intensity v1.0 / ISO/IEC 21031:2024](https://github.com/Green-Software-Foundation/sci) `(E × I) + M`, sommé sur les traces analysées. Le scoring multi-régions est automatique quand les spans OTel portent l'attribut `cloud.region`. En mode daemon, l'estimation énergie peut être affinée via Scaphandre RAPL ou CPU% + SPECpower cloud-natif, et l'intensité du réseau électrique récupérée en temps réel via Electricity Maps.

> **perf-sentinel est un calculateur carbone spécialisé pour les émissions logicielles / compute**, méthodologie activity-based, intensité grid horaire par région (Electricity Maps, ENTSO-E, RTE, National Grid ESO, EIA, ...), carbone embarqué bottom-up (Boavizta + HotCarbon 2024) et disclosures signées Sigstore vérifiables par hash.
>
> Il convient comme **source primaire de données** pour une plateforme de comptabilité carbone horizontale, ou comme **outil de contrôle interne** pour les KPI d'émissions logicielles et la conformité RGESN.
>
> Il n'est **pas encore vérifié par tiers-partie** pour un reporting CSRD / GHG Protocol Scope 2/3 standalone, qui exige un audit par un organisme qualifié et l'intégration des scopes non-IT. Les chiffres CO₂ portent un encadrement `~2×` en mode proxy par défaut (plus serré avec Scaphandre RAPL ou SPECpower cloud + calibration). Méthodologie, sources et bornes : [docs/LIMITATIONS.md#carbon-estimates-accuracy](docs/LIMITATIONS.md#carbon-estimates-accuracy) et [docs/METHODOLOGY.md](docs/METHODOLOGY.md).

Couplages concrets : passer les comptes I/O et estimations énergie par région à **Watershed**, **Sweep**, **Greenly** ou **Persefoni** comme activity data ; ou utiliser perf-sentinel directement pour démontrer la conformité **RGESN** (Référentiel Général d'Écoconception de Services Numériques, ARCEP/Ademe/DINUM 2024) sur les critères d'optimisation logicielle, où détection de N+1, appels redondants, caching et réduction du fanout correspondent aux critères concernés.

Pour les organisations qui souhaitent malgré tout publier une *disclosure périodique non-réglementaire* d'efficacité logicielle (JSON trimestriel/annuel, signature Sigstore optionnelle), le workflow optionnel `perf-sentinel disclose` est documenté dans [docs/REPORTING.md](docs/REPORTING.md). Il est volontairement écarté du chemin de démarrage principal.

## Comment ça se compare ?

La niche de perf-sentinel : être **léger, agnostique du protocole, natif CI/CD et carbon-aware**, pas remplacer une suite d'observabilité complète.

| Capacité                        | [Hypersistence Optimizer](https://vladmihalcea.com/hypersistence-optimizer/) | [Datadog APM + DBM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Sentry](https://sentry.io/for/performance/) | [Digma](https://digma.ai/)  | [Grafana Pyroscope](https://grafana.com/oss/pyroscope/) | **perf-sentinel**                        |
|---------------------------------|------------------------------------------------------------------------------|-------------------------------------------------------------|-----------------------------------------------------------------------|----------------------------------------------|-----------------------------|---------------------------------------------------------|------------------------------------------|
| Détection N+1 SQL               | JPA uniquement, test-time                                                    | Oui, automatique (DBM)                                      | Oui, automatique                                                      | Oui, automatique OOTB                        | Oui, IDE-centric (JVM/.NET) | Non (profiler CPU/mémoire, pas analyseur de requêtes)   | Oui, niveau protocole, tout runtime OTel |
| Détection N+1 HTTP              | Non                                                                          | Oui, service maps                                           | Oui, corrélation de traces                                            | Oui, détecteur N+1 API Call                  | Partiel                     | Non                                                     | Oui                                      |
| Support polyglotte              | Java uniquement                                                              | Agents par langage                                          | Agents par langage                                                    | Par SDK, plupart des langages                | JVM + .NET (Rider beta)     | eBPF host-wide + SDKs par langage                       | Tout runtime instrumenté OTel            |
| Corrélation cross-service       | Non                                                                          | Oui                                                         | Oui                                                                   | Oui                                          | Limité (IDE local)          | Trace-to-profile via exemplars OTel                     | Via trace ID                             |
| Scoring GreenOps / SCI v1.0     | Non                                                                          | Non                                                         | Non                                                                   | Non                                          | Non                         | Non                                                     | Intégré (directionnel)                   |
| Empreinte runtime               | Bibliothèque (sans overhead)                                                 | Agent (~100-150 Mo RSS)                                     | Agent (~100-150 Mo RSS)                                               | SDK + backend                                | Backend local (Docker)      | Agent + backend (~50-100 Mo RSS selon le langage)       | Binaire autonome (<15 Mo RSS)            |
| Quality gate CI/CD natif        | Assertions manuelles dans les tests                                          | Alertes, pas de gate de build                               | Alertes, pas de gate de build                                         | Alertes, pas de gate de build                | Non                         | Non                                                     | Oui (exit 1 sur dépassement de seuil)    |
| Licence                         | Commerciale (Optimizer)                                                      | SaaS propriétaire                                           | SaaS propriétaire                                                     | FSL (devient Apache-2 après 2 ans)           | Freemium, propriétaire      | AGPL-3.0                                                | AGPL-3.0                                 |
| Tarification / auto-hébergeable | Licence one-time                                                             | SaaS à l'usage (pas d'auto-hébergement)                     | SaaS à l'usage (pas d'auto-hébergement)                               | Free tier + SaaS (pas d'auto-hébergement)    | SaaS freemium (idem)        | Gratuit, entièrement auto-hébergeable                   | Gratuit, entièrement auto-hébergeable    |

Les empreintes d'agent des APMs commerciaux sont des estimations d'ordre de grandeur tirées de retours de déploiements publics ; l'overhead réel dépend du périmètre d'instrumentation.

### Ce que perf-sentinel n'est pas

Une comparaison honnête nécessite de nommer ce que perf-sentinel **ne fait pas** :

- **Pas un remplacement d'APM complet.** Pas de dashboards, pas d'UI d'alerting, pas de RUM, pas d'agrégation de logs, pas de profiling distribué. Si vous avez besoin de ça, Datadog, New Relic et Sentry restent les bons outils.
- **Pas un profiler continu.** L'outil observe les patterns d'I/O au niveau protocole ; il ne fait pas de sampling on-CPU, d'allocations ni de stack traces. Pour les flame graphs et le profiling CPU/mémoire par langage, [Grafana Pyroscope](https://grafana.com/oss/pyroscope/) est l'équivalent open-source et se marie bien : pyroscope dit où passe le temps CPU, perf-sentinel dit quels patterns d'I/O le déclenchent.
- **Pas une solution de monitoring temps réel.** Le mode daemon stream les findings, mais le centre de gravité du projet ce sont les quality gates CI et l'analyse post-hoc, pas l'observabilité prod live.
- **Pas une plateforme de comptabilité carbone réglementaire standalone.** perf-sentinel calcule des émissions logicielles activity-based à partir de sources audit-grade, mais un reporting CSRD ou GHG Protocol Scope 2/3 standalone exige une vérification tiers-partie et l'intégration des scopes non-IT qu'il ne couvre pas. À coupler à une plateforme carbone horizontale (Watershed, Sweep, Greenly, Persefoni, ...) ou à utiliser directement pour la conformité RGESN et les KPI d'émissions logicielles internes.
- **Pas un substitut à la mesure énergétique.** Le modèle I/O-vers-énergie est une approximation. Pour une puissance précise par process, utiliser Scaphandre (supporté en entrée) ou les APIs énergie du cloud provider.
- **Pas zéro-config.** La détection au niveau protocole exige une instrumentation OTel dans vos apps. Si votre stack n'émet pas de traces, perf-sentinel n'a rien à analyser.
- **Pas un plugin IDE.** Pour du feedback in-IDE en JVM/.NET au fil de la frappe, [Digma](https://digma.ai/) offre une expérience JetBrains bien intégrée.

## Acquittement de findings connus

Déposer `.perf-sentinel-acknowledgments.toml` à la racine du repo pour supprimer les findings que l'équipe a acceptés ; ils sont filtrés de `analyze` / `report` / `inspect` / `diff` et ne comptent pas dans le quality gate. Les acks runtime contre un daemon live sont exposés via le CLI `ack`, le dashboard HTML live et le TUI. Référence complète : [docs/ACKNOWLEDGMENTS.md](docs/ACKNOWLEDGMENTS.md) et [docs/ACK-WORKFLOW.md](docs/ACK-WORKFLOW.md).

## Documentation

| Sujet                                            | Document                                                                               |
|--------------------------------------------------|----------------------------------------------------------------------------------------|
| Référence des sous-commandes CLI                 | [docs/CLI.md](docs/CLI.md)                                                             |
| Architecture et pipeline                         | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)                                           |
| Topologies d'intégration (CI / prod / sidecar)   | [docs/INTEGRATION.md](docs/INTEGRATION.md)                                             |
| Instrumentation OTel par langage                 | [docs/INSTRUMENTATION.md](docs/INSTRUMENTATION.md)                                     |
| Recettes CI et diff de régression PR             | [docs/CI.md](docs/CI.md)                                                               |
| Référence complète de configuration              | [docs/CONFIGURATION.md](docs/CONFIGURATION.md)                                         |
| Schéma JSON du rapport                           | [docs/SCHEMA.md](docs/SCHEMA.md)                                                       |
| Sortie SARIF                                     | [docs/SARIF.md](docs/SARIF.md)                                                         |
| Dashboard HTML                                   | [docs/HTML-REPORT.md](docs/HTML-REPORT.md)                                             |
| TUI interactif                                   | [docs/INSPECT.md](docs/INSPECT.md)                                                     |
| API HTTP de query du daemon                      | [docs/QUERY-API.md](docs/QUERY-API.md)                                                 |
| Workflow d'acquittement                          | [docs/ACKNOWLEDGMENTS.md](docs/ACKNOWLEDGMENTS.md)                                     |
| Méthodologie et limites GreenOps                 | [docs/METHODOLOGY.md](docs/METHODOLOGY.md), [docs/LIMITATIONS.md](docs/LIMITATIONS.md) |
| Disclosures périodiques d'efficacité (optionnel) | [docs/REPORTING.md](docs/REPORTING.md)                                                 |
| Déploiement Helm                                 | [docs/HELM-DEPLOYMENT.md](docs/HELM-DEPLOYMENT.md)                                     |
| Runbook opérationnel                             | [docs/RUNBOOK.md](docs/RUNBOOK.md)                                                     |
| Provenance supply-chain (SLSA, Sigstore)         | [docs/SUPPLY-CHAIN.md](docs/SUPPLY-CHAIN.md)                                           |
| Notes de design (deep dive)                      | [docs/design/](docs/design/00-INDEX.md)                                                |

## Supply chain

Chaque GitHub Action est figée sur un SHA de commit de 40 caractères ; l'image de prod est `FROM scratch` ; `Cargo.lock` est committé et audité quotidiennement par `cargo audit` ; les permissions `GITHUB_TOKEN` des workflows sont par défaut `contents: read`. Dependabot ouvre des PRs groupées chaque semaine. Les binaires de release embarquent une provenance SLSA Build L3 (Sigstore + Rekor). Politique complète et commandes de vérification : [docs/SUPPLY-CHAIN.md](docs/SUPPLY-CHAIN.md).

## Releasing

Les releases suivent une procédure documentée avec un gate obligatoire de validation simulation-lab. Pas-à-pas dans [docs/RELEASE-PROCEDURE.md](docs/RELEASE-PROCEDURE.md) ([FR](docs/FR/RELEASE-PROCEDURE-FR.md)).

## Licence

[GNU Affero General Public License v3.0](LICENSE).
