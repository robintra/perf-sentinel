<p align="center">
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2Frobintra%2Fperf-sentinel%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&suffix=%20stable&label=rust%202024&color=D34516&logo=rust" alt="Rust" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/security-audit.yml/badge.svg" alt="Security Audit" /></a>
    <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=coverage" alt="Coverage" /></a>
    <a href="https://sonarcloud.io/summary/overall?id=robintrassard_perf-sentinel"><img src="https://sonarcloud.io/api/project_badges/measure?project=robintrassard_perf-sentinel&metric=alert_status" alt="Quality Gate" /></a>
    <a href="https://github.com/robintra/perf-sentinel/actions/workflows/release.yml"><img src="https://github.com/robintra/perf-sentinel/actions/workflows/release.yml/badge.svg" alt="Release" /></a>
    <a href="https://crates.io/crates/perf-sentinel"><img src="https://img.shields.io/crates/v/perf-sentinel?logo=rust&label=crates.io&color=D34516" alt="crates.io" /></a>
    <a href="https://docs.rs/perf-sentinel-core"><img src="https://img.shields.io/badge/docs.rs-perf--sentinel--core-66c2a5?logo=docsdotrs&logoColor=white" alt="docs.rs" /></a>
    <a href="https://github.com/robintra/perf-sentinel/pkgs/container/perf-sentinel"><img src="https://img.shields.io/badge/ghcr.io-perf--sentinel-2496ED?logo=docker&logoColor=white" alt="Container image" /></a>
    <a href="https://hub.docker.com/r/robintrassard/perf-sentinel"><img src="https://img.shields.io/badge/docker%20hub-perf--sentinel-2496ED?logo=docker&logoColor=white" alt="Docker Hub" /></a>
    <a href="https://artifacthub.io/packages/helm/perf-sentinel/perf-sentinel"><img src="https://img.shields.io/endpoint?url=https://artifacthub.io/badge/repository/perf-sentinel" alt="Artifact Hub" /></a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-dark-horizontal.svg">
  <img alt="perf-sentinel" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/logo/logo-horizontal.svg">
</picture>

**Mono-binaire auto-hébergé (`<20 Mo RSS`) qui détecte les anti-patterns d'I/O (N+1, appels redondants, SQL/HTTP lents, fanout) dans les traces OpenTelemetry de vos services et chiffre ces mêmes I/O en énergie et en carbone. S'utilise soit comme quality gate CI sur traces capturées (ou pour l'exploration locale et le post-mortem), soit comme daemon OTLP long-running (dashboard live, métriques Prometheus, API de query).**

> **À lire en premier**
> - **Prérequis :** vos services doivent émettre des **traces OpenTelemetry** (spans SQL + HTTP), **ou dd-trace via un pont Collector**, et ces spans doivent porter le texte de la requête (`db.statement` / `db.query.text`) et l'URL cible (`http.url` / `url.full`). Mise en place par langage (Java / C# / Rust / Go / Node.js / Python / Ruby / PHP) : [docs/FR/INSTRUMENTATION-FR.md](docs/FR/INSTRUMENTATION-FR.md). **Pas de SDK OpenTelemetry ?** Les équipes sur Datadog peuvent à la place faire le pont du trafic dd-trace via le `datadogreceiver` du Collector OTel, voir [Vous venez de Datadog](docs/FR/INTEGRATION-FR.md#vous-venez-de-datadog-dd-trace-sans-opentelemetry).
> - **Auditez votre tracing d'abord :** les spans qui ne portent pas ces attributs sont écartés en silence, sans avertissement, donc un rapport maigre ou vide peut signifier *aucun problème détecté* ou *aucune instrumentation exploitable*. `perf-sentinel inspect` montre ce qui a réellement été extrait de vos traces, un arbre de spans vide signifie que les attributs porteurs manquent en amont. Pour ce que la qualité d'instrumentation plafonne : [La qualité de l'instrumentation borne les findings](docs/FR/LIMITATIONS-FR.md#la-qualité-de-linstrumentation-borne-les-findings).
> - **Ce que ce n'est *pas* :** un APM complet, un profiler continu, ni (pour le moment) une plateforme de comptabilité carbone réglementaire standalone. Voir [Ce que perf-sentinel n'est pas](#ce-que-perf-sentinel-nest-pas).
> - **Maturité :** bêta, pré-1.0. La surface CLI, les clés de configuration et les formats sur disque peuvent encore changer d'une release à l'autre avant la 1.0, les ruptures de compatibilité étant signalées dans les [notes de version](https://github.com/robintra/perf-sentinel/releases). Les enums de sortie JSON sont la seule partie couverte par un contrat de stabilité explicite (voir [Formats d'entrée et de sortie](#formats-dentrée-et-de-sortie)).

---

## Aperçu rapide

Tableau de bord HTML (un seul fichier offline) :

```bash
perf-sentinel report --input traces.json --output report.html
```

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/dashboard_dark.gif">
  <img alt="tour du dashboard" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/dashboard_light.gif">
</picture>

...ou, si vous préférez votre terminal, TUI interactif pour parcourir les vues Analyze, Inspect et Explain en une seule session :

```bash
perf-sentinel analyze --tui --input traces.json
```

![TUI all-in-one : Analyze descend vers Inspect puis Explain, Esc remonte](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/demo.gif)

Vous préférez des images fixes à examiner panneau par panneau ? Allez aux [Captures](#captures). Les démos animées par commande sont repliées juste en dessous.

<details>
<summary>Plus de démos (analyze, explain, inspect, monitor, pg-stat, calibrate, disclose)</summary>

Rapport terminal (`perf-sentinel analyze`) :

![rapport terminal analyze](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/demo.gif)

Inspect, le TUI autonome à quatre panneaux (`perf-sentinel inspect`) :

![démo inspect : couleurs de sévérité et panneau detail scrollable](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/demo.gif)

Explain sur une trace (`perf-sentinel explain --trace-id <id>`) :

![démo explain : arbre de spans annoté et findings de niveau trace](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/explain/demo.gif)

Hotspots pg_stat_statements (`perf-sentinel pg-stat`) :

![démo pg-stat : SQL classé par temps total, appels et latence moyenne](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/pg-stat/demo.gif)

Calibration des facteurs énergie (`perf-sentinel calibrate`) :

![démo calibrate : facteurs per-service à partir de l'énergie mesurée](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/demo.gif)

Prévisualisation de divulgation périodique (`perf-sentinel disclose --tui`) :

![prévisualisation disclose : stepper calendaire, résumé agrégé, verdict du validateur officiel](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/demo.gif)

</details>

## Pourquoi perf-sentinel ?

Les anti-patterns de performance comme les N+1 existent dans toute application qui fait des I/O, monolithes comme microservices. Dans les architectures distribuées, un appel utilisateur cascade sur plusieurs services, chacun avec ses propres I/O, et personne n'a de visibilité sur le chemin complet.

Les outils existants résolvent chacun une partie du problème. Hypersistence Utils ne couvre que JPA, Datadog et New Relic sont des agents propriétaires lourds qu'on ne veut pas forcément déployer dans tous les pipelines, les détecteurs de Sentry sont solides mais liés à son SDK et son backend. Aucun ne propose un **détecteur d'anti-patterns au niveau protocole, auto-hébergeable**, exécutable soit comme quality gate CI sur des traces capturées (exit 1 si seuil dépassé, SARIF pour le code scanning) **soit** comme daemon OTLP long-running (ingestion gRPC + HTTP, Prometheus `/metrics`, dashboard HTML live, query API, workflow d'ack runtime) à poser à côté ou devant votre backend de tracing existant.

perf-sentinel observe les traces que votre application émet déjà (requêtes SQL, appels HTTP), quel que soit le langage ou l'ORM. Il n'a pas besoin de comprendre JPA, EF Core ou SeaORM : il voit les requêtes qu'ils génèrent.

Et il ne s'arrête pas à la détection. Chaque I/O évitable est traduit en énergie puis en CO₂ sur une méthode reconnue (alignée SCI), ce qui chiffre le gaspillage et le rend **attribuable au code**. Là où les outils carbone actuels estiment une empreinte globale par le haut, à partir de la facture cloud ou de ratios sectoriels, perf-sentinel **mesure par le bas**, requête par requête, un gisement directement actionnable.

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

Chaque finding embarque : type, sévérité, template normalisé, occurrences, endpoint source, suggestion, localisation source (quand les spans OTel portent les attributs `code.*`) et impact GreenOps (voir plus bas). Pour les règles de sévérité et seuils ajustables, voir [docs/FR/design/04-DETECTION-FR.md](docs/FR/design/04-DETECTION-FR.md).

## Installation

```bash
# depuis crates.io
cargo install perf-sentinel --locked

# ou télécharger un binaire pré-construit (Linux amd64/arm64, macOS arm64, Windows amd64)
curl -LO https://github.com/robintra/perf-sentinel/releases/latest/download/perf-sentinel-linux-amd64
chmod +x perf-sentinel-linux-amd64 && sudo mv perf-sentinel-linux-amd64 /usr/local/bin/perf-sentinel

# ou via Docker
docker run --rm -p 4317:4317 -p 4318:4318 \
  ghcr.io/robintra/perf-sentinel:latest watch --listen-address 0.0.0.0
```

Les binaires Linux ciblent musl (statiques, fonctionnent sur n'importe quelle distribution et dans les images `FROM scratch`). Un chart Helm est disponible sous [`charts/perf-sentinel/`](charts/perf-sentinel/). Voir [docs/FR/HELM-DEPLOYMENT-FR.md](docs/FR/HELM-DEPLOYMENT-FR.md).

## Démarrage rapide

```bash
# 1. Essayer la démo intégrée (aucune installation côté apps)
perf-sentinel demo                       # rapport terminal en couleurs
perf-sentinel demo --tui                 # rapport TUI interactif
perf-sentinel demo --html demo.html      # tableau de bord HTML

# 2. Analyser un fichier de traces capturées
perf-sentinel analyze --input traces.json

# 3. L'utiliser comme quality gate CI (exit 1 si seuil dépassé)
perf-sentinel analyze --input traces.json --ci --config .perf-sentinel.toml

# 4. Streamer les traces de vos apps (mode daemon)
perf-sentinel watch
```

`demo --html` est une vitrine complète : tous les onglets du tableau de bord sont remplis (Overview, Findings avec Explain en ligne, Carbon, pg_stat, Diff et corrélations inter-traces synthétisées). L'acquittement en direct reste réservé au daemon, voir `watch` puis `query --daemon <URL> monitor`.

Sur dd-trace ? Pas de fichier OpenTelemetry pour l'étape 2, faites le pont via le `datadogreceiver` du Collector : `watch` pour le daemon, ou un backend Tempo/Jaeger avec `tempo`/`jaeger-query` en batch, car `analyze` ne lit pas l'OTLP. Voir [Vous venez de Datadog](docs/FR/INTEGRATION-FR.md#vous-venez-de-datadog-dd-trace-sans-opentelemetry).

`.perf-sentinel.toml` minimal à la racine du repo :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0    # tolérance zéro N+1 SQL
io_waste_ratio_max = 0.30          # max 30% d'I/O évitables

[detection]
n_plus_one_min_occurrences = 5
slow_query_threshold_ms = 500
```

Référence complète des sous-commandes : `perf-sentinel <cmd> --help`, ou [docs/FR/CLI-FR.md](docs/FR/CLI-FR.md).

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
perf-sentinel man > perf-sentinel.1                                # page de manuel
perf-sentinel query findings --service order-svc                   # dialoguer avec un daemon
```

</details>

## Formats d'entrée et de sortie

<details>
<summary><b>Formats d'entrée</b></summary>

- **Fichiers de traces** (auto-détectés) : JSON natif perf-sentinel, export JSON Jaeger, Zipkin JSON v2. Pas de flag `--format` nécessaire, le format est détecté sur les premiers octets. Passés via `--input` sur `analyze`, `diff`, `explain`, `inspect`, `report`, `calibrate` (ou lus sur l'entrée standard pour `analyze`). Voir [docs/FR/INTEGRATION-FR.md#formats-dingestion](docs/FR/INTEGRATION-FR.md#formats-dingestion).
- **OTLP live** : gRPC sur `:4317` et HTTP sur `:4318`, ingérés par le daemon `watch` depuis votre OTel Collector ou SDK. Voir [docs/FR/INTEGRATION-FR.md](docs/FR/INTEGRATION-FR.md).
- **Datadog / dd-trace** (sans SDK OpenTelemetry) : faites le pont du trafic dd-trace via un OTel Collector équipé du `datadogreceiver`, qui réexporte de l'OTLP vers le daemon `watch`, ou vers un backend Tempo ou Jaeger pour les chemins batch `tempo`/`jaeger-query` ci-dessous (`analyze` ne lit pas l'OTLP). perf-sentinel lit le SQL depuis la ressource Datadog nativement, sans changement applicatif. Voir [docs/FR/INTEGRATION-FR.md#vous-venez-de-datadog-dd-trace-sans-opentelemetry](docs/FR/INTEGRATION-FR.md#vous-venez-de-datadog-dd-trace-sans-opentelemetry).
- **Grafana Tempo** : récupère les traces directement depuis un backend Tempo avec `perf-sentinel tempo`. Voir [docs/FR/INTEGRATION-FR.md#intégration-tempo](docs/FR/INTEGRATION-FR.md#intégration-tempo).
- **API Jaeger query** : récupère depuis un backend Jaeger ou Victoria Traces avec `perf-sentinel jaeger-query`. Voir [docs/FR/INTEGRATION-FR.md#intégration-api-jaeger-query-jaeger-et-victoria-traces](docs/FR/INTEGRATION-FR.md#intégration-api-jaeger-query-jaeger-et-victoria-traces).
- **`pg_stat_statements`** : classe les hotspots PostgreSQL depuis la vue catalogue avec `perf-sentinel pg-stat`. Voir [docs/FR/INTEGRATION-FR.md](docs/FR/INTEGRATION-FR.md).

</details>

<details>
<summary><b>Formats de sortie</b></summary>

- **`text`** (défaut) : sortie terminal colorée, regroupée par sévérité. Disponible sur `analyze`, `diff`, `pg-stat`, `query`, `explain`, `ack`.
- **`json`** : rapport structuré. Disponible sur `analyze`, `diff`, `pg-stat`, `query`, `explain`, `ack`. Schéma complet dans [docs/FR/SCHEMA-FR.md](docs/FR/SCHEMA-FR.md), exemples dans [docs/schemas/examples/](docs/schemas/examples/).
- **`sarif`** (SARIF v2.1.0) : code scanning GitHub/GitLab, annotations PR inline via `physicalLocations`. Disponible sur `analyze` et `diff`. Voir [docs/FR/SARIF-FR.md](docs/FR/SARIF-FR.md).
- **Dashboard HTML** : rapport offline en un seul fichier depuis `perf-sentinel report`, navigation dans les arbres de traces, thème clair/sombre, export CSV sur les onglets Findings / pg_stat / Diff / Correlations. Voir [docs/FR/HTML-REPORT-FR.md](docs/FR/HTML-REPORT-FR.md).
- **TUI interactif** : trois vues clavier en un seul drill-down (Analyze, Inspect, Explain) depuis `perf-sentinel analyze --tui`, `inspect` ou `explain --tui` (ou `query inspect` pour données live du daemon). Voir [docs/FR/INSPECT-FR.md](docs/FR/INSPECT-FR.md).
- **Daemon live** : findings NDJSON sur stdout, `/metrics` Prometheus avec Grafana Exemplars, sonde `/health`, API HTTP de query. Voir [docs/FR/METRICS-FR.md](docs/FR/METRICS-FR.md) et [docs/FR/QUERY-API-FR.md](docs/FR/QUERY-API-FR.md).
- **Disclosure périodique (optionnel)** : JSON `perf-sentinel-report/v1.0` vérifiable par hash depuis `perf-sentinel disclose`, signable via Sigstore. Voir [docs/FR/REPORTING-FR.md](docs/FR/REPORTING-FR.md).

Les valeurs d'enum `io_intensity_band` / `io_waste_ratio_band` (`healthy` / `moderate` / `high` / `critical`) sont stables entre versions, les seuils numériques sous-jacents peuvent évoluer. Tableau de référence et explication dans [docs/FR/LIMITATIONS-FR.md#interprétation-des-scores](docs/FR/LIMITATIONS-FR.md#interprétation-des-scores).

</details>

La sortie est déterministe : la même entrée produit un JSON et un SARIF identiques au bit près (les findings sont triés sur une clé stable, pas l'ordre d'itération d'une `HashMap`), si bien qu'un quality gate CI ne clignote jamais et que deux exécutions identiques ne produisent aucun diff de PR parasite.

## Déploiement

Quatre environnements, trois modèles de déploiement. Mise en place complète dans [docs/FR/INTEGRATION-FR.md](docs/FR/INTEGRATION-FR.md), recettes CI dans [docs/FR/CI-FR.md](docs/FR/CI-FR.md), métriques Prometheus dans [docs/FR/METRICS-FR.md](docs/FR/METRICS-FR.md), exemple sidecar dans [`examples/docker-compose-sidecar.yml`](examples/docker-compose-sidecar.yml).

Modèles : **batch CI** (`analyze --ci` sur traces capturées, exit 1 sur dépassement de seuil), **collector central** (un OTel Collector route vers le daemon `watch`, métriques Prometheus et API de query), **sidecar** (un daemon par service pour du debug isolé). Le collector central est un daemon unique avec état : les replicas horizontaux exigent un load balancing par `trace_id` et ne partagent pas l'état de corrélation, voir [Modèle d'état du daemon](docs/FR/LIMITATIONS-FR.md#modèle-détat-du-daemon-en-mémoire-mono-processus-sans-état-partagé).

Deux comportements à connaître avant de dimensionner : l'échantillonnage de traces en amont (head-based vs tail-based) comme le `[daemon] sampling_rate` sous-comptent les détecteurs basés sur la répétition, et sous surcharge soutenue le daemon déleste des lots d'analyse entiers plutôt que de bloquer l'ingestion, chaque délestage étant compté dans les métriques, jamais perdu en silence. Détails et dimensionnement des files bornées : [Échantillonnage en amont](docs/FR/LIMITATIONS-FR.md#échantillonnage-en-amont-et-précision-de-la-détection), [Échantillonnage en mode daemon](docs/FR/LIMITATIONS-FR.md#échantillonnage-en-mode-daemon) et [Contre-pression d'analyse et délestage de charge](docs/FR/LIMITATIONS-FR.md#contre-pression-danalyse-et-délestage-de-charge).

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

![Intégration GreenOps : sources externes temps réel (Scaphandre RAPL en kWh sur x86, Kepler eBPF en kWh sur ARM et x86, Redfish BMC en watts pour bare-metal, Electricity Maps en gCO₂/kWh) plus sources internes froides (Cloud SPECpower en kWh, carbone embarqué en gCO₂e/req via Boavizta + HotCarbon 2024, transport réseau en kWh/GB via Mytton 2024) alimentant perf-sentinel en mode batch ou daemon, émettant énergie et carbone en parallèle des traces](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/perf-sentinel-GreenOps.svg)

</details>

<details>
<summary>Vue d'ensemble : comment les quatre environnements s'articulent</summary>

![Intégration globale de perf-sentinel à travers dev local, CI, staging et prod](https://raw.githubusercontent.com/robintra/perf-sentinel-simulation-lab/main/docs/diagrams/svg/global-integration.svg)

</details>

Moniteur opérateur live sur un daemon en marche, pour les DevOps / SRE, quatre onglets cyclés par Tab (hints Advisor, mix énergie/carbone, courbes Trends, santé des Scrapers) via `perf-sentinel query --daemon <URL> monitor` :

![query monitor : quatre onglets live cyclés par Tab sur un daemon en marche](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/monitor/demo.gif)

### Traitement des données

perf-sentinel traite les traces sur place. Il ne fait aucun appel réseau sortant silencieux et n'embarque aucune télémétrie d'usage. Le contenu brut des spans (valeurs SQL littérales, URLs complètes) ne vit **qu'en mémoire**, dans la fenêtre de streaming : par défaut un TTL de 30 s et un cache LRU plafonné à 10 000 traces actives, tous deux ajustables sous `[daemon]`. Le daemon n'écrit jamais de spans bruts sur disque. Tout ce qu'il émet (rapports JSON / SARIF / HTML, API de query dont `/api/explain`, métriques Prometheus, archive NDJSON par fenêtre optionnelle) ne porte que le **template normalisé** : les littéraux SQL et les valeurs de chemin/query des URLs sont remplacés par des `?` et réduits à un *décompte* de paramètres distincts, jamais les valeurs elles-mêmes.

Le daemon écoute sur `127.0.0.1` par défaut. TLS, CORS et la clé d'API d'acquittement sont tous opt-in. Les endpoints `GET` en lecture seule **et les listeners d'ingestion OTLP** (gRPC `:4317`, HTTP `:4318`) ne sont pas authentifiés et font confiance à leurs émetteurs, gardez donc l'ingestion sur un réseau de confiance et placez un reverse proxy ou une network policy devant avant d'exposer quoi que ce soit au-delà de localhost. Réglages de rétention et d'écoute dans [docs/FR/CONFIGURATION-FR.md](docs/FR/CONFIGURATION-FR.md), surface d'API dans [docs/FR/QUERY-API-FR.md](docs/FR/QUERY-API-FR.md).

## Performance

`perf-sentinel bench` ne chronomètre que le pipeline d'analyse (`normalize -> correlate -> detect -> score`), mono-thread, sur des jeux de données synthétiques : c'est le coût pur du pipeline, pas un débit de bout en bout ni un benchmark du daemon sous charge. Le périmètre exact du chrono et la construction des jeux de données sont repliés sous le tableau.

| Jeu de données (44 043 évènements synthétiques) | Plateforme     | Débit pipeline       | p50 / p99 par évènement |
|-------------------------------------------------|----------------|----------------------|-------------------------|
| Motif répété                                    | x86 Xeon 8481C | ~576 k évts/s        | 1,72 / 1,88 µs          |
| Motif répété                                    | Apple M4 Pro   | ~1,23 M évts/s       | 0,81 / 0,89 µs          |
| SQL varié                                       | x86 Xeon 8481C | ~640 k évts/s        | 1,54 / 1,69 µs          |
| SQL varié                                       | Apple M4 Pro   | ~1,33 M évts/s       | 0,75 / 0,81 µs          |

- **x86**, mesuré en juin 2026 : GCP c3-standard-8 (Intel Xeon Platinum 8481C @ 2,70 GHz, 8 vCPU), release officielle 0.8.5 `x86_64-unknown-linux-musl` (allocateur mimalloc).
- **M4**, mesuré le 2026-06-08 : Mac mini M4 Pro (12 cœurs, 24 Go de mémoire unifiée, macOS 26.5.1), release officielle 0.8.5 `aarch64-apple-darwin` (allocateur système), exécutée nativement sur l'hôte.

Avec les artefacts natifs de chaque plateforme, le M4 Pro soutient environ 2,1x le débit d'un vCPU 8481C (2,14x sur le motif répété, 2,08x sur le varié). p50 / p99 sont la latence par évènement (durée d'une itération divisée par le nombre d'évènements), sur 10 itérations. Reproduire avec `perf-sentinel bench --help`. Édition Rust 2024, rustc 1.96.0 stable.

<details>
<summary><b>Méthodologie du bench (périmètre du chrono, jeux de données)</b></summary>

La lecture du fichier, le parsing JSON et l'ingestion ont tous lieu avant le démarrage du chrono, et les lots d'entrée sont clonés en amont. Le pipeline est mono-thread (pas de rayon), le nombre de cœurs ne change donc pas le débit. Les deux jeux de données comptent chacun 44 043 évènements synthétiques, construits en dupliquant la fixture de démo (`crates/sentinel-cli/src/demo_data.json`), l'un répétant le même motif et l'autre avec du SQL aléatoire par requête. Cela isole bien le débit du pipeline mais ne reflète pas la diversité d'une vraie production.

</details>

<details>
<summary><b>Détail par allocateur sur la même puce (macOS natif vs Docker musl)</b></summary>

L'artefact musl x86 est lié à mimalloc tandis que l'artefact macOS arm64 utilise l'allocateur système, les binaires cross-plateforme diffèrent donc par l'allocateur autant que par l'ISA. Pour isoler ce point, la release musl + mimalloc (l'artefact `linux/arm64`) a aussi été mesurée au bench sur le même M4 Pro, dans un conteneur Docker `linux/arm64`, sur les mêmes jeux de données :

| Build sur la même puce M4 Pro          | Motif répété         | SQL varié            |
|----------------------------------------|----------------------|----------------------|
| macOS arm64 natif, allocateur système  | ~1,23 M évts/s       | ~1,33 M évts/s       |
| Docker linux/arm64, musl + mimalloc    | ~1,39 M évts/s       | ~1,51 M évts/s       |

Même puce, mêmes jeux de données : le build musl + mimalloc tourne environ 13 % plus vite que l'allocateur natif de macOS, ce qui confirme l'allocateur comme cause principale du débit plus élevé en Docker. À build équivalent (les deux en musl + mimalloc), le M4 Pro fait alors environ 2,4x le x86 8481C (2,41x sur le motif répété, 2,36x sur le varié).

</details>

<details>
<summary><b>Mémoire : rss_peak_bytes du bench vs empreinte du daemon</b></summary>

`bench` affiche aussi `rss_peak_bytes`, mais cette valeur est dominée par les lots d'entrée pré-clonés gardés en mémoire (10 itérations x 44 043 évènements), ce n'est donc pas l'empreinte mémoire du daemon. Elle n'est pas non plus comparable d'un OS à l'autre : `rss_peak_bytes` lit le RSS courant via `/proc` sous Linux mais le RSS de pic via `getrusage` sous macOS.

Séparément, la mémoire du daemon long-running a été profilée sur le même M4 Pro avec le build musl + mimalloc dans une VM Docker Desktop `linux/arm64` (15,6 Go). Il tourne à **~17 Mo** au repos (le chiffre `<20 Mo RSS` cité dans le TL;DR et le tableau comparatif, apples-to-apples avec les chiffres "agent idle" des autres outils). Le build natif tourne à ~10 Mo, mimalloc échange un peu de RSS contre de la vitesse d'allocation. Sous une charge d'ingestion soutenue de ~1,0 M évts/s, le même daemon culmine à **~190 Mo** (contre 237 Mo sur 0.6.1, sous le plafond de 250 Mo).

</details>

## GreenOps : score d'intensité I/O (directionnel)

Chaque finding embarque un **score d'intensité I/O (IIS)**, total des ops I/O d'un endpoint divisé par le nombre d'invocations, et un **ratio de gaspillage I/O** (ops évitables / ops totales). Réduire les N+1 et appels redondants améliore les temps de réponse *et* la consommation d'énergie. Ces deux objectifs ne s'opposent pas.

`co2.total` est reporté comme le numérateur [Software Carbon Intensity v1.0 / ISO/IEC 21031:2024](https://github.com/Green-Software-Foundation/sci) `(E × I) + M`, sommé sur les traces analysées. Le scoring multi-régions est automatique quand les spans OTel portent l'attribut `cloud.region`. En mode daemon, l'estimation énergie peut être affinée via plusieurs sources mesurées (Scaphandre RAPL sur x86, Kepler eBPF sur ARM et x86, Redfish BMC pour la puissance murale en bare-metal, ou CPU% + SPECpower cloud-natif), et l'intensité du réseau électrique récupérée en temps réel via Electricity Maps.

> **Le volet carbone de perf-sentinel chiffre les I/O détectées avec la rigueur d'un calculateur carbone spécialisé émissions logicielles / compute** : méthodologie activity-based, intensité grid horaire par région (Electricity Maps, ENTSO-E, RTE, National Grid ESO, EIA, ...), carbone embarqué bottom-up (Boavizta + HotCarbon 2024) et disclosures signées Sigstore vérifiables par hash.
>
> Il convient comme **source primaire de données** pour une plateforme de comptabilité carbone horizontale, ou comme **outil de contrôle interne** pour les KPI d'émissions logicielles et la conformité RGESN.
>
> Il n'est **pas encore vérifié par tiers-partie** pour un reporting CSRD / GHG Protocol Scope 2/3 standalone, qui exige un audit par un organisme qualifié et l'intégration des scopes non-IT. Les chiffres CO₂ portent un encadrement `~2×` en mode proxy par défaut (plus serré avec une source d'énergie mesurée : Scaphandre RAPL, Kepler eBPF, Redfish BMC ou SPECpower cloud + calibration). Méthodologie, sources et bornes : [docs/FR/LIMITATIONS-FR.md#précision-des-estimations-carbone](docs/FR/LIMITATIONS-FR.md#précision-des-estimations-carbone) et [docs/FR/METHODOLOGY-FR.md](docs/FR/METHODOLOGY-FR.md).

Couplages concrets : passer les comptes I/O et estimations énergie par région à **Watershed**, **Sweep**, **Greenly** ou **Persefoni** comme activity data, ou utiliser perf-sentinel directement pour démontrer la conformité **RGESN** (Référentiel Général d'Écoconception de Services Numériques, ARCEP/Ademe/DINUM 2024) sur les critères d'optimisation logicielle, où détection de N+1, appels redondants, caching et réduction du fanout correspondent aux critères concernés.

Pour les organisations qui souhaitent malgré tout publier une *disclosure périodique non-réglementaire* d'efficacité logicielle (JSON trimestriel/annuel, signature Sigstore optionnelle), le workflow optionnel `perf-sentinel disclose` est documenté dans [docs/FR/REPORTING-FR.md](docs/FR/REPORTING-FR.md). Il est volontairement écarté du chemin de démarrage principal.

## Comment ça se compare ?

La niche de perf-sentinel : être **léger, agnostique du protocole, natif CI/CD et carbon-aware**, pas remplacer une suite d'observabilité complète.

| Capacité                              | [Hypersistence Optimizer](https://vladmihalcea.com/hypersistence-optimizer/) | [Datadog APM + DBM](https://www.datadoghq.com/product/apm/) | [New Relic APM](https://newrelic.com/platform/application-monitoring) | [Sentry](https://sentry.io/for/performance/) | [Digma](https://digma.ai/)  | [Grafana Pyroscope](https://grafana.com/oss/pyroscope/) | [OTJAE](https://github.com/RETIT/opentelemetry-javaagent-extension) | **perf-sentinel**                        |
|---------------------------------------|------------------------------------------------------------------------------|-------------------------------------------------------------|-----------------------------------------------------------------------|----------------------------------------------|-----------------------------|---------------------------------------------------------|---------------------------------------------------------------------|------------------------------------------|
| Détection N+1 SQL                     | JPA uniquement, test-time                                                    | Oui, automatique (DBM)                                      | Oui, automatique                                                      | Oui, automatique OOTB                        | Oui, IDE-centric (JVM/.NET) | Non (profiler CPU/mémoire, pas analyseur de requêtes)   | Non                                                                 | Oui, niveau protocole, tout runtime OTel |
| Détection N+1 HTTP                    | Non                                                                          | Oui, service maps                                           | Oui, corrélation de traces                                            | Oui, détecteur N+1 API Call                  | Partiel                     | Non                                                     | Non                                                                 | Oui                                      |
| Support polyglotte                    | Java uniquement                                                              | Agents par langage                                          | Agents par langage                                                    | Par SDK, plupart des langages                | JVM + .NET (Rider beta)     | eBPF host-wide + SDKs par langage                       | JVM (extension de l'agent OTel Java)                                | Tout runtime instrumenté OTel            |
| Corrélation cross-service             | Non                                                                          | Oui                                                         | Oui                                                                   | Oui                                          | Limité (IDE local)          | Trace-to-profile via exemplars OTel                     | Intra-JVM uniquement, pas d'attribution cross-service documentée    | Via trace ID                             |
| Attribution carbone/énergie par span  | Non                                                                          | Non                                                         | Non                                                                   | Non                                          | Non                         | Non                                                     | Oui, par span et par transaction (méthodologie CCF)                 | Oui, par span (aligné SCI, directionnel) |
| Score GreenOps (IIS, waste ratio)     | Non                                                                          | Non                                                         | Non                                                                   | Non                                          | Non                         | Non                                                     | Non                                                                 | Intégré (directionnel)                   |
| Empreinte runtime                     | Bibliothèque (sans overhead)                                                 | Agent (~100-150 Mo RSS)                                     | Agent (~100-150 Mo RSS)                                               | SDK + backend                                | Backend local (Docker)      | Agent + backend (~50-100 Mo RSS selon le langage)       | Agent JVM (overhead non publié)                                     | Binaire autonome (<20 Mo RSS)            |
| Quality gate CI/CD natif              | Assertions manuelles dans les tests                                          | Alertes, pas de gate de build                               | Alertes, pas de gate de build                                         | Alertes, pas de gate de build                | Non                         | Non                                                     | Non                                                                 | Oui (exit 1 sur dépassement de seuil)    |
| Licence                               | Commerciale (Optimizer)                                                      | SaaS propriétaire                                           | SaaS propriétaire                                                     | FSL (devient Apache-2 après 2 ans)           | Freemium, propriétaire      | AGPL-3.0                                                | Apache-2.0                                                          | AGPL-3.0                                 |
| Tarification / auto-hébergeable       | Licence one-time                                                             | SaaS à l'usage (pas d'auto-hébergement)                     | SaaS à l'usage (pas d'auto-hébergement)                               | Free tier + SaaS (pas d'auto-hébergement)    | SaaS freemium (idem)        | Gratuit, entièrement auto-hébergeable                   | Gratuit, entièrement auto-hébergeable                               | Gratuit, entièrement auto-hébergeable    |

Les empreintes d'agent des APMs commerciaux sont des estimations d'ordre de grandeur tirées de retours de déploiements publics. L'overhead réel dépend du périmètre d'instrumentation.

### Ce que perf-sentinel n'est pas

Une comparaison honnête nécessite de nommer ce que perf-sentinel **ne fait pas** :

- **Pas un remplacement d'APM complet.** Pas de dashboards, pas d'UI d'alerting, pas de RUM, pas d'agrégation de logs, pas de profiling distribué. Si vous avez besoin de ça, Datadog, New Relic et Sentry restent les bons outils.
- **Pas un profiler continu.** L'outil observe les patterns d'I/O au niveau protocole, il ne fait pas de sampling on-CPU, d'allocations ni de stack traces. Pour les flame graphs et le profiling CPU/mémoire par langage, [Grafana Pyroscope](https://grafana.com/oss/pyroscope/) est l'équivalent open-source et se marie bien : pyroscope dit où passe le temps CPU, perf-sentinel dit quels patterns d'I/O le déclenchent.
- **Pas une solution de monitoring temps réel.** Le mode daemon stream les findings, mais le centre de gravité du projet ce sont les quality gates CI et l'analyse post-hoc, pas l'observabilité prod live.
- **Pas une plateforme de comptabilité carbone réglementaire standalone.** Un reporting CSRD ou GHG Protocol Scope 2/3 standalone exige une vérification tiers-partie et des scopes non-IT qu'il ne couvre pas. Périmètre exact, couplages (Watershed, Sweep, Greenly, Persefoni) et cas RGESN : voir [GreenOps](#greenops--score-dintensité-io-directionnel).
- **Pas un substitut à la mesure énergétique.** Le modèle I/O-vers-énergie est certes une mesure, mais approximative. Pour une puissance mesurée plus précise, brancher Scaphandre (RAPL x86), Kepler (eBPF, compatible ARM) ou Redfish (puissance murale BMC bare-metal), les trois sont supportés en entrée, ou utiliser les APIs énergie du fournisseur cloud. Pour ce qu'une attribution purement logicielle peut et ne peut pas couvrir sur un serveur typique, voir [docs/FR/LIMITATIONS-FR.md § Ce que couvre une attribution purement logicielle](docs/FR/LIMITATIONS-FR.md#ce-que-couvre-une-attribution-purement-logicielle).
- **Pas zéro-config.** La détection au niveau protocole exige une instrumentation OTel dans vos apps. Si votre stack n'émet pas de traces, perf-sentinel n'a rien à analyser.
- **Pas un plugin IDE.** Pour du feedback in-IDE en JVM/.NET au fil de la frappe, [Digma](https://digma.ai/) offre une expérience JetBrains bien intégrée.

## Acquittement de findings connus

Déposer `.perf-sentinel-acknowledgments.toml` à la racine du repo pour supprimer les findings que l'équipe a acceptés. Ils sont filtrés de `analyze` / `report` / `inspect` / `diff` et ne comptent pas dans le quality gate. Les acks runtime contre un daemon live sont exposés via le CLI `ack`, le dashboard HTML live et le TUI. Référence complète : [docs/FR/ACKNOWLEDGMENTS-FR.md](docs/FR/ACKNOWLEDGMENTS-FR.md) et [docs/FR/ACK-WORKFLOW-FR.md](docs/FR/ACK-WORKFLOW-FR.md).

## Captures

La section [Aperçu rapide](#aperçu-rapide) en haut de page affiche les GIFs animés. Les images figées ci-dessous permettent de zoomer sur chaque panneau pour en lire les détails.

<details>
<summary>Captures (TUI, analyze, explain, inspect, pg-stat, calibrate, disclose, report)</summary>

**Configuration** (`.perf-sentinel.toml`) :

![config](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/config.png)

**TUI all-in-one** (`perf-sentinel analyze --tui`). Une seule session parcourt Analyze, Inspect et Explain, Enter descend d'un niveau, Esc remonte, la barre d'onglets suit la vue active :

![Vue Analyze : le tableau de bord de synthèse GreenOps sous la barre d'onglets](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/analyze.png)

![Vue Inspect : le navigateur à quatre panneaux, traces, findings, corrélations et detail](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/inspect.png)

![Vue Explain : l'arbre de spans annoté plein écran d'une trace](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/tui/explain.png)

**Rapport d'analyse** (`perf-sentinel analyze`) page par page, avec un léger recouvrement pour que chaque finding apparaisse en entier sur au moins une page :

![page 1 : N+1 SQL, N+1 HTTP, SQL redondant](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-1.png)

![page 2 : HTTP redondant, SQL lent, HTTP lent](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-2.png)

![page 3 : fanout excessif, service bavard, saturation du pool](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-3.png)

![page 4 : appels sérialisés, résumé GreenOps, quality gate](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/analyze/report-4.png)

**Mode explain** (`perf-sentinel explain --trace-id <id>`). Les findings rattachés à un span (N+1, redondant, lent, fanout) sont affichés inline à côté du span concerné, les findings de niveau trace (service bavard, saturation du pool, appels sérialisés) sont remontés dans une section dédiée au-dessus de l'arbre :

![vue en arbre explain avec annotation de fanout excessif sur le span parent](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/explain/tree.png)

![header trace-level explain avec warning de service bavard](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/explain/trace-level.png)

**Mode inspect** (`perf-sentinel inspect`). Le header du panneau findings colore chaque finding selon sa sévérité, les cinq images ci-dessous parcourent la fixture démo à travers les trois niveaux de sévérité plus une vue du panneau détail avec sa fonction de scroll :

![TUI inspect, vue initiale : service bavard warning (jaune)](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/main.png)

![TUI inspect, panneau détail actif : haut de l'arbre de spans fanout excessif](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/detail.png)

![TUI inspect, panneau détail scrollé : moitié basse de l'arbre fanout](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/detail-scrolled.png)

![TUI inspect, N+1 SQL critical (rouge) : 10 occurrences, suggestion de batch](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/critical.png)

![TUI inspect, HTTP redondant info (cyan) : 3 validations de token identiques](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/info.png)

`inspect --input` accepte aussi un Report JSON pré-calculé (par exemple un snapshot daemon issu de `/api/export/report`). Les panels Findings et Correlations s'allument complètement, le panel Detail affiche un message qui pointe vers les deux chemins qui portent les vrais spans :

![TUI inspect, mode Report : 4 panels avec corrélations cross-trace et message Detail](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/inspect/report-mode.png)

**Moniteur opérateur live** (`perf-sentinel query --daemon <URL> monitor`). Lecture seule, adossé au daemon, quatre onglets cyclés par Tab. Les données qu'il expose (hints de config, provenance des sources, intensités par région) sont catégorielles et à haute cardinalité, exactement ce que la règle des labels bornés garde hors du `/metrics` Prometheus :

![Onglet Advisor : les hints du conseiller de réglages du daemon, ici une fenêtre de traces proche de son plafond](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/monitor/advisor.png)

![Onglet Energy : le mix énergie/carbone effectif par service et par région, sources froides vs chaudes](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/monitor/energy.png)

![Onglet Trends : courbes d'énergie et de carbone sur l'historique de sondage, gauges runtime en part de leur plafond sous le seuil advisor](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/monitor/trends.png)

![Onglet Scrapers : santé live des backends énergie via /api/energy](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/monitor/scrapers.png)

**Mode pg-stat** (`perf-sentinel pg-stat --input <pg_stat_statements.csv>`) : classe les requêtes SQL par temps d'exécution total, par nombre d'appels, par latence moyenne. Cross-référence avec tes traces via `--traces` pour repérer les requêtes qui dominent la DB sans apparaître dans ton instrumentation :

![pg-stat : top hotspots par temps total, appels et latence moyenne](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/pg-stat/hotspots.png)

**Mode calibrate** (`perf-sentinel calibrate --traces <traces.json> --measured-energy <energy.csv>`) :

![entrée calibrate : CSV avec mesures de puissance par service](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/csv.png)

![exécution calibrate : warnings et facteurs par service affichés](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/run.png)

![sortie calibrate : TOML généré avec les facteurs de calibration](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/calibrate/output.png)

**Prévisualisation disclose** (`perf-sentinel disclose --tui`). Une prévisualisation en lecture seule de la divulgation périodique : un stepper calendaire sur la période, des toggles intent et confidentialité en direct, et la commande équivalente à copier. Elle n'écrit ni ne hache jamais de rapport :

![prévisualisation disclose, vue mois : en-tête des réglages, résumé agrégé, commande équivalente](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/preview.png)

![prévisualisation disclose, vue trimestre : le stepper g élargit la période au trimestre entier](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/quarter.png)

![prévisualisation disclose, intent official : le validateur explique pourquoi le rapport n'est pas encore publiable](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/official.png)

**Dashboard report** (`perf-sentinel report`), une capture par onglet. Chaque `<picture>` sert la variante sombre quand le navigateur annonce `prefers-color-scheme: dark` :

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/findings-dark.png">
  <img alt="dashboard report : Findings avec chips Warning + order-svc actifs" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/findings.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/explain-dark.png">
  <img alt="dashboard report : arbre de trace Explain avec cinq SELECT N+1 surlignés et une suggestion Java JPA" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/explain.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/pg-stat-dark.png">
  <img alt="dashboard report : classement pg_stat par Calls, 15 lignes" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/pg-stat.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/diff-dark.png">
  <img alt="dashboard report : onglet Diff, un finding flaggé en régression" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/diff.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/correlations-dark.png">
  <img alt="dashboard report : onglet Correlations, trois paires cross-trace avec confiance et lag médian" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/correlations.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/greenops-dark.png">
  <img alt="dashboard report : onglet GreenOps avec breakdown CO₂ multi-région sur eu-west-3, us-east-1 et eu-central-1" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/greenops.png">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/cheatsheet-dark.png">
  <img alt="dashboard report : modal cheatsheet listant la table complète des raccourcis clavier" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/report/cheatsheet.png">
</picture>

</details>

## Documentation

| Sujet                                            | Document                                                                                                       |
|--------------------------------------------------|----------------------------------------------------------------------------------------------------------------|
| Sommaire                                         | [docs/FR](docs/FR/00-INDEX-FR.md)                                                                              |
| Référence des sous-commandes CLI                 | [docs/FR/CLI-FR.md](docs/FR/CLI-FR.md)                                                                         |
| Architecture et pipeline                         | [docs/FR/ARCHITECTURE-FR.md](docs/FR/ARCHITECTURE-FR.md)                                                       |
| Topologies d'intégration (CI / prod / sidecar)   | [docs/FR/INTEGRATION-FR.md](docs/FR/INTEGRATION-FR.md)                                                         |
| Instrumentation OTel par langage                 | [docs/FR/INSTRUMENTATION-FR.md](docs/FR/INSTRUMENTATION-FR.md)                                                 |
| Recettes CI et diff de régression PR             | [docs/FR/CI-FR.md](docs/FR/CI-FR.md)                                                                           |
| Référence complète de configuration              | [docs/FR/CONFIGURATION-FR.md](docs/FR/CONFIGURATION-FR.md)                                                     |
| Schéma JSON du rapport                           | [docs/FR/SCHEMA-FR.md](docs/FR/SCHEMA-FR.md)                                                                   |
| Sortie SARIF                                     | [docs/FR/SARIF-FR.md](docs/FR/SARIF-FR.md)                                                                     |
| Dashboard HTML                                   | [docs/FR/HTML-REPORT-FR.md](docs/FR/HTML-REPORT-FR.md)                                                         |
| TUI interactif                                   | [docs/FR/INSPECT-FR.md](docs/FR/INSPECT-FR.md)                                                                 |
| API HTTP de query du daemon                      | [docs/FR/QUERY-API-FR.md](docs/FR/QUERY-API-FR.md)                                                             |
| Workflow d'acquittement                          | [docs/FR/ACKNOWLEDGMENTS-FR.md](docs/FR/ACKNOWLEDGMENTS-FR.md)                                                 |
| Méthodologie et limites GreenOps                 | [docs/FR/METHODOLOGY-FR.md](docs/FR/METHODOLOGY-FR.md), [docs/FR/LIMITATIONS-FR.md](docs/FR/LIMITATIONS-FR.md) |
| Disclosures périodiques d'efficacité (optionnel) | [docs/FR/REPORTING-FR.md](docs/FR/REPORTING-FR.md)                                                             |
| Déploiement Helm                                 | [docs/FR/HELM-DEPLOYMENT-FR.md](docs/FR/HELM-DEPLOYMENT-FR.md)                                                 |
| Runbook opérationnel                             | [docs/FR/RUNBOOK-FR.md](docs/FR/RUNBOOK-FR.md)                                                                 |
| Provenance supply-chain (SLSA, Sigstore)         | [docs/FR/SUPPLY-CHAIN-FR.md](docs/FR/SUPPLY-CHAIN-FR.md)                                                       |
| Notes de design (deep dive)                      | [docs/FR/design/](docs/FR/design/00-INDEX-FR.md)                                                               |

## Supply chain

Chaque GitHub Action est figée sur un SHA de commit de 40 caractères, l'image de prod est `FROM scratch`, `Cargo.lock` est committé et audité quotidiennement par `cargo audit`, les permissions `GITHUB_TOKEN` des workflows sont par défaut `contents: read`. Dependabot ouvre des PRs groupées chaque semaine. Les binaires de release embarquent une provenance SLSA Build L3 (Sigstore + Rekor) et les données de dépendances `cargo-auditable` (`cargo audit bin`), et chaque release publie un SBOM SPDX attesté sous le prédicat SPDX. Politique complète et commandes de vérification : [docs/FR/SUPPLY-CHAIN-FR.md](docs/FR/SUPPLY-CHAIN-FR.md).

## Publication des versions

Les publications suivent une procédure documentée. Le dépôt compagnon [perf-sentinel-simulation-lab](https://github.com/robintra/perf-sentinel-simulation-lab/blob/main/docs/SCENARIOS.md) est le palier de validation obligatoire avant tag : 36 scénarios de bout en bout sur un cluster Kubernetes local (k3d), couvrant neuf modes de déploiement plus les templates CI, les modes de défaillance et les limites de charge, chacun avec un diagramme Mermaid, les entrées/sorties exactes et les pièges rencontrés lors de la validation. Pas-à-pas dans [docs/FR/RELEASE-PROCEDURE-FR.md](docs/FR/RELEASE-PROCEDURE-FR.md).

## Licence

[GNU Affero General Public License v3.0](LICENSE).

Faire tourner perf-sentinel ne place pas vos propres services sous AGPL. C'est un processus autonome : vos applications lui envoient seulement des traces OpenTelemetry par le réseau (OTLP), une communication à distance et non un lien de compilation, qui ne crée donc aucune œuvre dérivée et n'impose aucune obligation de licence sur votre code. L'AGPL couvre le code source de perf-sentinel lui-même. Si vous le modifiez et proposez la version modifiée à des tiers via un réseau, l'article 13 vous oblige à mettre cette source modifiée à disposition de ces utilisateurs. Utiliser les binaires ou l'image officiels non modifiés n'entraîne aucune obligation de ce type. Ceci est un résumé pratique et non un avis juridique, consultez votre service juridique en cas de doute.

## Crédits

Logo et bannière par [Gwendoline MEIGNEN](https://www.linkedin.com/in/gwendoline-meignen-b0224873/).
