# Méthodologie

Ce document explique comment perf-sentinel transforme des traces OpenTelemetry en champs `efficiency_score`, `energy_kwh` et `carbon_kgco2eq` exposés dans un rapport de transparence périodique. Il condense les notes de design par étage que l'on trouve dans `docs/design/` et `docs/ARCHITECTURE.md`. La cible est un auditeur ou un data scientist qui veut vérifier la chaîne de calcul de bout en bout sans lire tout l'arbre source.

## Pipeline en un coup d'œil

```
events -> normalize -> correlate -> detect -> score -> report
```

Chaque étage est une fonction pure sur les données, avec des traits uniquement aux frontières d'I/O (`IngestSource`, `ReportSink`). Un finding produit par `detect` est apparié avec une estimation green-impact issue de `score`, puis agrégé par l'aggregator de transparence périodique sur une période calendaire.

## Introduction : énergie et SCI v1.0

Si vous n'avez jamais implémenté un scoring carbone pour des workloads logiciels, cette introduction courte est un préalable pour les formules de la suite du document. Elle ne suppose pas de familiarité préalable avec les cadres réglementaires (CSRD, GHG Protocol, RGESN) ni avec la stack outillage énergie (SCI v1.0, RAPL, Scaphandre, SPECpower, Boavizta, API Electricity Maps). Chaque terme est défini en une ligne à sa première apparition. Les autres docs perf-sentinel renvoient ici pour les concepts de scoring vert, voir [docs/FR/CONFIGURATION-FR.md](CONFIGURATION-FR.md#green) et [docs/FR/SCHEMA-FR.md](SCHEMA-FR.md#agrégat).

**Les cadres réglementaires en jeu.** perf-sentinel aligne son modèle carbone sur trois cadres dont les lecteurs ont pu entendre parler, aucun n'est requis pour suivre la suite du document.

- **CSRD (Corporate Sustainability Reporting Directive)** est le régime obligatoire UE 2024 de reporting de durabilité. Les grandes entreprises européennes doivent publier des inventaires d'émissions audités selon trois scopes (directes, électricité achetée, chaîne de valeur). perf-sentinel peut alimenter une pipeline CSRD en données d'activité, mais n'est pas en soi un outil de reporting CSRD.
- **GHG Protocol (Greenhouse Gas Protocol)** est le standard international de comptabilité des émissions corporate publié par le WRI/WBCSD, la référence de facto derrière la CSRD et la plupart des régulations nationales. Le Scope 2 couvre l'électricité achetée, le Scope 3 couvre tout le reste en amont et aval, y compris le calcul logiciel acheté en cloud.
- **RGESN (Référentiel Général d'Écoconception de Services Numériques)** est le cadre français d'écoconception des services numériques publié par ARCEP, Arcom et ADEME en 2024. Il vérifie 78 critères couvrant architecture, contenu, hébergement et cycle de vie. perf-sentinel associe chaque détecteur aux critères qu'il sert, voir la [correspondance RGESN 2024](#correspondance-rgesn-2024) ci-dessous.

**Pourquoi SCI v1.0.** Software Carbon Intensity est le standard développé par la Green Software Foundation et publié comme ISO/IEC 21031:2024 (ISO/IEC JTC 1, mars 2024). L'artefact publié par la GSF est la SCI Specification, révision courante v1.1. Il définit un score carbone par unité fonctionnelle pour le logiciel, `SCI = (E * I) + M`, exprimé en gCO2eq par requête (ou par toute unité fonctionnelle choisie). Les trois termes correspondent à trois phénomènes physiques distincts mesurés chacun par un outillage différent. perf-sentinel utilise SCI v1.0 parce que (a) c'est la méthodologie la plus largement adoptée pour comparer les émissions logicielles entre organisations, (b) elle sépare proprement l'optimisation marginale/évitable de la comptabilité d'inventaire totale, (c) elle est référencée par RGESN et alignée sur les frontières GHG Protocol Scope 2/3.

**Les trois termes SCI.**

- **E (Énergie)** est l'électricité consommée par opération, en kWh. perf-sentinel substitue une des quatre sources de mesure au runtime : un proxy I/O (`io_proxy_v3`, environ `1e-7` kWh par opération d'I/O, directionnel uniquement), des lectures RAPL via Scaphandre, le pourcentage CPU fourni par le cloud projeté sur des tables SPECpower, ou des coefficients de calibration fournis par l'opérateur via `[green] calibration_file`. La source retenue est exposée dans `methodology.calibration.energy_source_models` pour qu'un auditeur vérifie quel chemin a produit E.
- **I (Intensité du réseau)** est le carbone émis par kWh par le réseau électrique local, en gCO2eq/kWh. perf-sentinel embarque une table statique rafraîchie annuellement (couvrant toutes les régions cloud majeures et les principaux réseaux nationaux) et accepte un override en direct via l'API Electricity Maps quand `[green.electricity_maps]` est configuré. La source est exposée dans `methodology.calibration.carbon_intensity_source` comme une des valeurs `static_tables`, `electricity_maps`, ou `mixed`.
- **M (Carbone embarqué)** couvre les émissions de fabrication du silicium sous-jacent (CPU, RAM, réseau, construction datacentre), amorties par requête. perf-sentinel utilise un coefficient par défaut dérivé de Boavizta plus le papier HotCarbon 2024, surchargeable via `[green] embodied_carbon_per_request_gco2`. M est indépendant de la région et s'ajoute après `E * I`.

**Qui lit quelle valeur.** Un *auditeur durabilité* qui prépare une soumission CSRD scope 2 se soucie de `total_carbon_kgco2eq` et du bloc `methodology.*` qui prouve la source de chaque terme. Un *SRE qui optimise le système* se soucie de `estimated_optimization_potential_kgco2eq`, qui est le terme opérationnel évitable (`avoidable_io_ops * ENERGY_PER_IO_OP_KWH * I`) et exclut M parce qu'on ne dé-fabrique pas du silicium en corrigeant un N+1. L'`efficiency_score` (0-100) est le résumé opérateur dérivé uniquement de `io_waste_ratio`, pas des émissions absolues.

**Limite connue : intervalle d'incertitude `2x`.** L'estimation carbone est livrée avec une fourchette multiplicative `2x` explicite. C'est un signal délibéré que le modèle directionnel (en particulier le proxy I/O et les tables statiques) ne convient pas au reporting d'émissions à valeur réglementaire. Resserrer la fourchette demande Scaphandre RAPL ou SPECpower cloud pour le terme E, et Electricity Maps en direct pour le terme I. La discussion complète sur l'incertitude vit dans [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md).

**Termes connexes que vous rencontrerez dans les sections ci-dessous.** Des one-liners seulement, les définitions complètes sont dans les références liées.

- **RAPL (Running Average Power Limit)** est une fonctionnalité CPU Intel qui expose un compteur d'énergie matériel lisible via `/sys/class/powercap/intel-rapl/`. Il donne la consommation électrique par package à granularité milliseconde, sans instrumentation côté application. Les CPU AMD exposent une interface similaire sous un MSR différent. RAPL est ce que Scaphandre lit.
- **Scaphandre** est un profileur d'énergie open source qui interroge les compteurs RAPL et expose les lectures de puissance par processus comme un endpoint Prometheus. perf-sentinel scrape Scaphandre et attribue les lectures aux services instrumentés OTel via correspondance PID. [Projet](https://github.com/hubblo-org/scaphandre).
- **SPECpower (`SPECpower_ssj2008`)** est une suite de benchmarks qui projette le pourcentage d'utilisation CPU sur la consommation électrique pour un SKU serveur publié. La méthodologie Cloud Carbon Footprint utilise les courbes SPECpower comme proxy quand la mesure directe n'est pas disponible. perf-sentinel embarque une table SPECpower pour les principaux SKU cloud. [Benchmark](https://www.spec.org/power_ssj2008/).
- **CCF (Cloud Carbon Footprint)** est la méthodologie open source qu'Etsy a publiée en 2020, qui combine tables SPECpower, intensités de réseau par région cloud, et amortissement embarqué. Le chemin cloud-energy de perf-sentinel est compatible CCF, mêmes entrées, mêmes coefficients. [Projet](https://www.cloudcarbonfootprint.org/).
- **Boavizta** est l'association française qui publie des méthodologies ouvertes et des données de référence pour l'analyse de cycle de vie des équipements numériques, en particulier les coefficients de carbone embarqué pour CPU et serveurs. Le terme M par défaut dans perf-sentinel dérive de Boavizta plus le papier HotCarbon 2024. [Projet](https://boavizta.org/).
- **API Electricity Maps** est le service commercial (avec offre gratuite côté API) qui publie l'intensité carbone horaire par zone (gCO2eq/kWh) pour plus de 250 zones dans le monde. perf-sentinel l'appelle à la demande quand `[green.electricity_maps]` est configuré. Chaque requête retourne soit un facteur `direct` (génération opérationnelle uniquement) soit un facteur `lifecycle` (opérationnel plus fabrication des actifs de production). perf-sentinel enregistre celui qui a été utilisé. [Doc API](https://api-portal.electricitymaps.com/).
- **gCO2eq / kgCO2eq** signifie "grammes (ou kilogrammes) de CO2 équivalent". Équivalent parce que les gaz à effet de serre autres que le CO2 (méthane, protoxyde d'azote, ...) sont pondérés par leur pouvoir de réchauffement global ramené à une base CO2. Unité standard utilisée dans CSRD, GHG Protocol, SCI.
- **Émissions marginales vs moyennes.** L'émission moyenne est l'intensité moyenne du réseau sur une fenêtre (ce que donnent les tables statiques et la plupart des réponses Electricity Maps). L'émission marginale est l'intensité du prochain kWh consommé (souvent une centrale fossile de pointe), pertinente pour des décisions de *décalage de charge* mais pas pour le reporting d'inventaire. perf-sentinel reporte la moyenne : SCI v1.1 (2024) autorise l'intensité marginale court terme, marginale long terme ou moyenne (le texte SCI v1.0 / ISO exigeait les taux marginaux). Le scoring marginal est une amélioration future.

## Ancrage dans la littérature

Le choix méthodologique d'exposer un score directionnel (`efficiency_score` calculé sur `io_waste_ratio`) et de classer les endpoints par impact relatif, plutôt que de rapporter une valeur absolue en watts, s'appuie sur une littérature indépendante autour de la mesure énergétique logicielle.

- **Les compteurs énergétiques matériels sont précis pour leur périmètre.** Khan, Hirki, Niemi, Nurminen et Ou ([*RAPL in Action : Experiences in Using RAPL for Power Measurements*, ACM TOMPECS 3(2):1-26, 2018](https://doi.org/10.1145/3177754)) caractérisent RAPL comme une source d'énergie fiable pour les packages CPU et DRAM qu'il couvre, avec la réserve connue qu'il n'inclut pas la périphérie, le stockage ni les pertes d'alimentation.
- **Les compteurs logiciels suivent fidèlement le signal matériel.** Jay, Ostapenco, Lefèvre, Trystram, Orgerie et Fichel ([*An experimental comparison of software-based power meters : focus on CPU and GPU*, IEEE/ACM CCGrid 2023](https://doi.org/10.1109/CCGrid57682.2023.00020)) rapportent une forte corrélation entre les compteurs logiciels (dont Scaphandre) et un wattmètre externe, tout en montrant que l'écart résiduel entre matériel et logiciel est significatif et non constant selon les workloads. Les compteurs logiciels sont de bons porteurs de signal, pas des substituts à la mesure absolue.
- **Le relatif l'emporte sur l'absolu, et les déterminants principaux sont les patterns de requêtes.** Ruch (*Towards Greener Software : Measuring Performance and Energy Efficiency of Enterprise Applications*, Project Thesis MSE, OST Eastern Switzerland University of Applied Sciences, superviseur Prof. Dr. Olaf Zimmermann, 2025) établit que les valeurs absolues d'énergie ne sont pas comparables entre systèmes d'exploitation, applications et jeux d'instructions, alors que la *distribution relative* de la consommation l'est, à travers OS, applications et ensembles d'opérations. Ce même travail identifie les patterns d'accès base de données (nombre de requêtes, volume d'enregistrements lus, technologie d'accès) comme les déterminants énergétiques majeurs des applications enterprise.

perf-sentinel se situe dans cette tradition. Le pipeline classe les endpoints par IIS relatif, compare les runs par deltas d'`io_waste_ratio`, et détecte les patterns d'accès base de données et inter-services que la littérature identifie comme déterminants énergétiques majeurs (N+1 SQL, N+1 HTTP, redundant SQL, redundant HTTP, fetch-all, fanout, chatty services, appels sérialisés). Il ne revendique pas une précision absolue de niveau wattmètre. La fourchette multiplicative `2x` sur l'estimation carbone, le positionnement explicite comme compteur de gaspillage directionnel, et la discussion complète du périmètre et de la précision vivent dans [docs/FR/LIMITATIONS-FR.md](LIMITATIONS-FR.md).

## I/O Intensity Score (IIS)

Le proxy de base pour l'énergie est le nombre d'opérations d'I/O par couple `(service, endpoint)`. perf-sentinel compte chaque span SQL ou HTTP sortant comme une opération d'I/O.

- `total_io_ops` : nombre de spans d'I/O sur l'ensemble des traces de la fenêtre analysée.
- `avoidable_io_ops` : nombre de spans d'I/O attribués à des anti-patterns évitables. Les quatre patterns évitables sont N+1 SQL, N+1 HTTP, redundant SQL, redundant HTTP, tous énumérés par `FindingType::is_avoidable_io()` et listés dans `core_patterns_required` de chaque rapport officiel.
- `io_waste_ratio = avoidable_io_ops / total_io_ops`, dans `[0, 1]`.

## Énergie par opération

L'énergie opérationnelle est approximée par un proxy à coefficient unique :

```
energy_kwh = total_io_ops * ENERGY_PER_IO_OP_KWH
```

`ENERGY_PER_IO_OP_KWH = 1e-7 kWh` est documenté dans `score/carbon.rs` et étiqueté comme modèle `io_proxy_v3`. Le coefficient est une estimation directionnelle, pas une mesure.

Lorsque l'opérateur branche le scraper Scaphandre RAPL optionnel ou un scraper cloud SPECpower, perf-sentinel substitue une énergie mesurée par service et bascule le tag de modèle vers `scaphandre_rapl` ou `cloud_specpower`. La section méthodologie d'un rapport expose `scaphandre_used` et `specpower_table_version` pour que les consommateurs sachent quel chemin a produit les chiffres.

## CO2 opérationnel

Le terme opérationnel SCI (Software Carbon Intensity) est `O = E * I`, où `E` est l'énergie par fenêtre en kWh et `I` est l'intensité carbone du réseau en gCO2eq/kWh pour la région de la charge.

perf-sentinel embarque une table d'intensité réseau statique rafraîchie annuellement et accepte une surcharge temps réel via l'API Electricity Maps quand `[green.electricity_maps]` est configurée. Le champ `methodology.calibration.carbon_intensity_source` d'un rapport vaut `electricity_maps`, `static_tables` ou `mixed` pour qu'un auditeur puisse vérifier quel chemin a produit le CO2 opérationnel.

## CO2 embarqué

Le terme SCI `M` couvre les émissions du silicium fabriqué amorti par requête. perf-sentinel utilise un coefficient par défaut fixe documenté dans `config.rs::DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2`, surchargeable via `[green] embodied_carbon_per_request_gco2`. Le CO2 embarqué est indépendant de la région, ajouté au CO2 opérationnel avant que le total par fenêtre ne soit sommé sur la période du rapport.

## Agrégation sur une période

`perf-sentinel disclose` lit les enveloppes `Report` archivées par fenêtre (`{ts, report}`) et les replie en trois étapes.

1. Chaque enveloppe est filtrée pour ne garder que celles tombant dans la période calendaire demandée.
2. Les compteurs globaux additionnent `total_io_ops`, `avoidable_io_ops`, `total.mid` (gCO2), `avoidable.mid` (gCO2). gCO2 est divisé par 1000 pour obtenir `kgCO2eq`.
3. L'attribution par service utilise les maps runtime-calibrated `per_service_*` quand la fenêtre source les porte. Sinon les totaux globaux sont distribués proportionnellement à la part d'I/O par service lue depuis `Report.per_endpoint_io_ops`. Une fenêtre sans offenders par service est rangée sous `_unattributed` sauf si `--strict-attribution` a été passé.

`efficiency_score = clamp(100 - 100 * io_waste_ratio, 0, 100)`. L'efficacité par service utilise la même formule sur le ratio évitable / total propre au service.

Les signaux de qualité (0.7.0+) résument quelle part de la période a été mesurée directement plutôt qu'inférée du proxy.

- `period_coverage = runtime_windows / total_windows`, dans `[0, 1]`, avec `runtime_windows_count` et `fallback_windows_count` qui portent les compteurs absolus derrière le ratio.
- `binary_versions` est l'ensemble des versions du binaire perf-sentinel observées sur la période, une mise à jour de daemon en milieu de période faisant porter plus d'une entrée à cet ensemble.
- `calibration_applied` sur `methodology.calibration_inputs` bascule à `true` quand au moins une fenêtre a appliqué les coefficients de calibration opérateur à l'énergie proxy.
- `per_service_energy_models` et `per_service_measured_ratio` (à la fois dans `GreenSummary` par fenêtre et dans `Aggregate` sur la période) surfacent la vue de fidélité par service : quel modèle énergétique a alimenté chaque service et quelle fraction de ses spans a effectivement été mesurée.

Les définitions wire-format de ces champs vivent dans les sections "Agrégat" et "Méthodologie" de `docs/FR/SCHEMA-FR.md`.

## Correspondance RGESN 2024

Le [RGESN 2024](https://www.arcep.fr/uploads/tx_gspublication/referentiel_general_ecoconception_des_services_numeriques_version_2024.pdf) (ARCEP, Arcom, ADEME) définit 78 critères d'écoconception répartis en neuf familles, numérotés `famille.critère`. La table ci-dessous associe chaque détecteur de perf-sentinel aux critères dont il sert l'intention.

C'est une **correspondance interprétative, pas une certification de conformité**. Les titres des critères RGESN ne nomment pas "requête N+1" ni "requête lente". Ce sont les critères qu'une détection aide à satisfaire, exposés pour qu'un auditeur puisse relier un finding au référentiel. La forme exploitable par machine est `FindingType::rgesn_criteria()` dans le code et le champ `rgesn_criteria` par pattern dans les détails d'anti-pattern du rapport de transparence.

| Détecteur | Critères RGESN | Intention du critère |
|---|---|---|
| `n_plus_one_sql`, `n_plus_one_http` | 7.1, 6.1 | Cache serveur pour les données les plus utilisées, budget de requêtes par écran |
| `redundant_sql`, `redundant_http` | 7.1, 6.5 | Cache serveur, éviter le chargement de ressources inutiles |
| `chatty_service` | 4.9, 4.10, 6.1 | Limiter et éviter les requêtes serveur inutiles, budget de requêtes par écran |
| `excessive_fanout`, `pool_saturation` | 3.2 | Architecture qui adapte les ressources à la demande réelle |
| `serialized_calls` | 8.10 | Minimiser l'impact des calculs et transferts de données asynchrones |
| `slow_sql`, `slow_http` | (aucun) | Le RGESN n'a pas de critère sur la latence d'une opération unique. La famille 9 "Algorithmie" cible les charges de machine learning, pas la latence des requêtes. |

## Limitations connues du schéma v1.0

- **L'énergie et le carbone par service sont runtime-calibrated quand l'archive source les porte.** Chaque fenêtre du `GreenSummary` expose maintenant `energy_kwh`, `energy_model`, `per_service_energy_kwh`, `per_service_carbon_kgco2eq` et `per_service_region`. L'aggregator somme directement ces valeurs. Les archives écrites avant la livraison de cette fonctionnalité ne portent pas les champs : l'aggregator tombe alors sur une énergie proxy (`total_io_ops × ENERGY_PER_IO_OP_KWH`) et une part d'I/O proportionnelle pour le carbone, en émettant un unique `tracing::warn!` par fichier d'archive concerné. L'ensemble des tags `energy_model` observés est exposé sous `methodology.calibration_inputs.energy_source_models`.
- **Le potentiel d'optimisation exclut le carbone embarqué.** `estimated_optimization_potential_kgco2eq` ne couvre que le terme opérationnel évitable (on ne peut pas dé-fabriquer du silicium en corrigeant des N+1). L'agrégat `total_carbon_kgco2eq` inclut à la fois les termes opérationnel et embarqué. Le disclaimer dans `notes.disclaimers` le précise explicitement.
- **Le carbone par service exclut l'embarqué.** Le terme embarqué (`M` au sens SCI) ne vit qu'au niveau agrégat. `sum(per_service_carbon_kgco2eq) × 1000` approxime `co2.operational_gco2`, pas `co2.total.mid`.
- **Bucket `_unattributed`.** Les fenêtres dont `Report.per_endpoint_io_ops` est vide (et qui n'ont pas non plus de maps runtime per-service) tombent dans le service `_unattributed`. `disclose --strict-attribution` refuse ces fenêtres. Les findings de ces fenêtres sont aussi rangés sous `_unattributed` pour qu'un service ne soit jamais publié avec `efficiency_score = 100` et des anti-patterns non nuls.
- **Couverture de la période et seuil de 75% (0.7.0+).** Chaque rapport porte `aggregate.period_coverage`, la fraction des fenêtres de scoring qui ont utilisé l'énergie runtime-calibrated contre le proxy fallback. Un rapport `intent = "official"` avec une couverture sous 0.75 est refusé par le validator. Un rapport `intent = "internal"` sous ce seuil porte un disclaimer explicite dans `notes.disclaimers`. La justification empirique de 0.75 vit dans `docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md`.
- **Ratio mesuré par service en moyenne arithmétique de fenêtres (0.7.0+).** `per_service_measured_ratio` dans `GreenSummary` est la fraction des spans d'un service dont l'énergie a été résolue par Scaphandre ou cloud SPECpower dans cette fenêtre. La valeur period-level dans `Aggregate.per_service_measured_ratio` est la moyenne arithmétique simple des ratios par fenêtre, pas pondérée par le nombre de spans : une fenêtre de 10 spans et une fenêtre de 10000 spans contribuent à part égale. Un service dont `per_service_energy_model` indique `scaphandre_rapl` avec `per_service_measured_ratio` de `0.05` a eu une seule observation Scaphandre contre 95% de proxy fallback dans la fenêtre : le tag indique la meilleure source observée, le ratio décrit la fidélité.
- **Flag de calibration binaire, period-wide (0.7.0+).** `methodology.calibration_inputs.calibration_applied` vaut `true` dès qu'au moins une fenêtre de la période a eu une calibration opérateur active, même si 89 fenêtres sur 90 ne l'avaient pas. Le texte du disclaimer dans `notes.disclaimers` reprend cette formulation exacte pour qu'un lecteur ne puisse pas confondre le flag avec "toutes les fenêtres étaient calibrées".
- **Versions du binaire sur la période (0.7.0+).** `aggregate.binary_versions` liste les versions du binaire perf-sentinel qui ont produit les archives sources. Une période qui couvre plusieurs versions porte un disclaimer qui invite le consommateur à vérifier la compatibilité de version avant de comparer ce rapport à des baselines historiques. L'ensemble est capé à 256 entrées, dans le cas improbable où un trimestre en couvrirait davantage les entrées en surplus sont silencieusement abandonnées.

## Intervalle d'incertitude

Chaque rapport est livré avec un intervalle multiplicatif `2x` sur l'estimation carbone. C'est un signal délibéré que la sortie est directionnelle et inadaptée à un reporting d'émissions réglementaire (CSRD, GHG Protocol Scope 3). Le bloc `notes.disclaimers` du rapport le rappelle en clair pour l'opérateur, y compris les limitations spécifiques à la v1.0 ci-dessus.

## Vérifier un rapport

Un rapport porte :

- `integrity.content_hash` : SHA-256 sur la forme JSON canonique (clés d'objets triées, sérialisation compacte, UTF-8) avec `content_hash` mis à chaîne vide. Un consommateur recompute en posant `content_hash` à `""` sur sa propre copie puis en hashant.
- `integrity.binary_hash` : SHA-256 du binaire perf-sentinel qui a produit le fichier, lu via `std::env::current_exe()`. À coupler avec `binary_verification_url` pour vérifier que le binaire correspond à une release publiée.

La chaîne d'intégrité de traces dans `integrity.trace_integrity_chain` est réservée pour une révision future et toujours `null` dans le schéma v1.0.

## Intégrité cryptographique (0.7.0+)

> **Voir aussi.** L'[introduction à Sigstore](SUPPLY-CHAIN-FR.md#introduction-à-sigstore) dans la doc supply-chain définit Cosign, Fulcio, Rekor, in-toto, OIDC et SLSA utilisés dans cette section.

Deux primitives optionnelles s'ajoutent au content hash pour ancrer un rapport publié dans une infrastructure publique.

- **Signature Sigstore** (`integrity.signature`). Quand l'opérateur signe l'attestation in-toto v1 du rapport via `cosign attest`, le rapport porte des métadonnées (`bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, `signed_at`) qui permettent à un consommateur de récupérer le bundle et de le vérifier via Rekor public. `verify-hash` refuse les bundles sans preuve d'inclusion Rekor, la transparence est une propriété du format, pas un opt-in.
- **Provenance SLSA du binaire** (`integrity.binary_attestation`). Les binaires de release perf-sentinel officiels portent une attestation SLSA Build L3 produite par le workflow GitHub Actions (`actions/attest-build-provenance` à partir de 0.7.1, `slsa-framework/slsa-github-generator` SLSA L2 sur la 0.7.0). Le rapport enregistre les métadonnées de locator pour qu'un consommateur vérifie l'attestation contre le binaire référencé par `integrity.binary_verification_url`, via `gh attestation verify <binary> --owner robintra --repo perf-sentinel` pour 0.7.1+ ou `slsa-verifier verify-artifact` contre l'asset legacy `multiple.intoto.jsonl` pour la 0.7.0.

Combinées, les deux primitives forment la chaîne `source -> SLSA -> binaire -> rapport -> signature Sigstore`. `verify-hash` chaîne le recompute content hash, la signature cosign, et le hint de vérification SLSA en une commande. La méthodologie, les modes d'échec, et les considérations privacy Rekor public vivent dans `docs/FR/design/10-SIGSTORE-ATTESTATION-FR.md`.
