# Méthodologie

Ce document explique comment perf-sentinel transforme des traces OpenTelemetry en champs `efficiency_score`, `energy_kwh` et `carbon_kgco2eq` exposés dans un rapport de transparence périodique. Il condense les notes de design par étage que l'on trouve dans `docs/design/` et `docs/ARCHITECTURE.md`. La cible est un auditeur ou un data scientist qui veut vérifier la chaîne de calcul de bout en bout sans lire tout l'arbre source.

## Pipeline en un coup d'œil

```
events -> normalize -> correlate -> detect -> score -> report
```

Chaque étage est une fonction pure sur les données, avec des traits uniquement aux frontières d'I/O (`IngestSource`, `ReportSink`). Un finding produit par `detect` est apparié avec une estimation green-impact issue de `score`, puis agrégé par l'aggregator de transparence périodique sur une période calendaire.

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

La chaîne d'intégrité de traces dans `integrity.trace_integrity_chain` et la signature Sigstore dans `integrity.signature` sont réservées pour une révision future et toujours `null` dans le schéma v1.0.
