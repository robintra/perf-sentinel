# Référence schéma : perf-sentinel-report v1.0

Ce document décrit la forme JSON d'un rapport de transparence périodique en prose. Le JSON Schema lisible par machine se trouve dans `docs/schemas/perf-sentinel-report-v1.json` (draft 2020-12). Deux exemples remplis sont sous `docs/schemas/examples/`.

## Clés racine

| clé               | type           | requise | notes                                                                             |
|-------------------|----------------|---------|-----------------------------------------------------------------------------------|
| `schema_version`  | string (const) | oui     | `"perf-sentinel-report/v1.0"`                                                     |
| `report_metadata` | object         | oui     | voir [Métadonnées de rapport](#métadonnées-de-rapport)                            |
| `organisation`    | object         | oui     | voir [Organisation](#organisation)                                                |
| `period`          | object         | oui     | voir [Période](#période)                                                          |
| `scope_manifest`  | object         | oui     | voir [Manifeste de scope](#manifeste-de-scope)                                    |
| `methodology`     | object         | oui     | voir [Méthodologie](#méthodologie)                                                |
| `aggregate`       | object         | oui     | voir [Agrégat](#agrégat)                                                          |
| `applications`    | array          | oui     | homogène : toutes les entrées G1 ou toutes G2, voir [Applications](#applications) |
| `integrity`       | object         | oui     | voir [Intégrité](#intégrité)                                                      |
| `notes`           | object         | oui     | voir [Notes](#notes)                                                              |

Le schéma ne fixe pas `additionalProperties: false` ; de nouveaux champs peuvent être ajoutés dans un bump mineur SemVer sans casser les consommateurs qui ne lisent que l'ensemble documenté.

## Métadonnées de rapport

`intent` vaut `internal | official | audited`. `audited` est réservé (la CLI le refuse aujourd'hui). `confidentiality_level` vaut `internal | public`. `integrity_level` vaut `none | hash-only | signed | audited` ; le sprint 1 produit `hash-only`. `generated_at` est un timestamp UTC RFC 3339. `generated_by` vaut `daemon | cli-batch | ci`. `perf_sentinel_version` est la chaîne SemVer du binaire qui a écrit le fichier. `report_uuid` est un UUID v4 estampillé par run.

## Organisation

`name` est requis et non vide. `country` est en ISO 3166-1 alpha-2, majuscules. `identifiers` est un objet ouvert avec `siren`, `vat`, `lei`, `opencorporates_url`, `domain` optionnels. `sector` est un code NACE rev2 optionnel.

Le domaine de publication (par exemple `transparency.example.fr`) est traité comme un identifiant implicite quand `notes.reference_urls.project` est publié depuis cet hôte. Le schéma ne l'impose pas, par choix.

## Période

`from_date` et `to_date` sont des dates calendaires ISO 8601 (`YYYY-MM-DD`). `period_type` vaut `calendar-quarter | calendar-month | calendar-year | custom`. `days_covered` vaut `to_date - from_date + 1`. Le validator intent officiel impose `days_covered >= 30`.

## Manifeste de scope

`total_applications_declared` est la taille du portefeuille applicatif de l'organisation. `applications_measured` est le nombre de services pour lesquels le rapport porte des données. Chaque entrée de `applications_excluded` porte `service_name` et un `reason` non vide. `environments_measured` liste les environnements définis par l'opérateur et observés (par exemple `["prod"]`). `total_requests_in_period` est une estimation opérateur optionnelle ; `requests_measured` est ce que perf-sentinel a effectivement vu. `coverage_percentage` vaut `requests_measured / total_requests_in_period * 100` quand le premier est renseigné.

## Méthodologie

`sci_specification` référence la révision SCI (par exemple `"ISO/IEC 21031:2024"`). `perf_sentinel_version` reprend le champ des métadonnées pour les consommateurs qui n'indexent que le bloc méthodologie. `enabled_patterns` et `disabled_patterns` portent chacun des noms de patterns issus du set fermé défini par `FindingType::as_str()` (10 valeurs). `core_patterns_required` est la liste fermée des patterns dont la remédiation coupe directement de l'I/O et du carbone : `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`. `conformance` vaut `core-required | extended | partial` ; `core-required` est le seuil minimum pour un rapport `intent = "official"`. `calibration_inputs.carbon_intensity_source` vaut `electricity_maps | static_tables | mixed`. `specpower_table_version` est la version de la table SPECpower embarquée ; le binaire embarque la seule copie autoritaire. `scaphandre_used` indique si le proxy énergie temps réel vient de Scaphandre RAPL.

## Agrégat

Sommes sur toute la période et tout le tableau `applications`. `total_requests`, `total_energy_kwh`, `total_carbon_kgco2eq` et `estimated_optimization_potential_kgco2eq` sont des nombres finis non négatifs. `aggregate_waste_ratio` est dans `[0, 1]`. `aggregate_efficiency_score` est dans `[0, 100]` et vaut `clamp(100 - 100 * io_waste_ratio, 0, 100)`. `anti_patterns_detected_count` est la somme de toutes les occurrences par service, y compris les patterns non évitables.

## Applications

Deux granularités, homogènes par rapport. Le validator refuse un rapport qui mélangerait les deux.

### G1 (intent `internal`)

Chaque entrée porte les totaux au niveau service plus un tableau `anti_patterns: [...]`. Chaque détail anti-pattern a `type` (un des 10 patterns connus), `occurrences`, `estimated_waste_kwh`, `estimated_waste_kgco2eq`, `first_seen`, `last_seen`. Les timestamps sont UTC RFC 3339. `display_name` et `service_version` sont des hints optionnels.

### G2 (intent `official` avec confidentiality `public`)

Chaque entrée porte les mêmes totaux au niveau service mais remplace le tableau par un seul entier `anti_patterns_detected_count`. Le schéma impose que les entrées G2 ne portent pas de champ `anti_patterns`, et inversement.

Les deux granularités sont encodées dans le JSON Schema avec des clauses `not: { required: [...] }` mutuellement exclusives pour rendre la discrimination explicite aux validateurs de schéma.

## Intégrité

`content_hash` est `"sha256:<64-hex>"` sur la forme JSON canonique du document avec le champ `content_hash` mis à chaîne vide. Le schéma accepte aussi une chaîne vide pour ce champ afin que les exemples puissent être livrés sans valeur cuite. `binary_hash` est `"sha256:<64-hex>"` du binaire perf-sentinel qui a produit le fichier. `binary_verification_url` pointe vers l'artefact de release où les consommateurs récupèrent le même binaire. `trace_integrity_chain` et `signature` sont réservés pour une révision future et valent `null` en v1.0.

## Notes

`disclaimers` porte sept déclarations par défaut : les deux lignes standard d'incertitude SCI (estimation directionnelle, fourchette ~2x), la précision sur le scope embarqué (exclu du potentiel d'optimisation), la note embarqué par service (opérationnel uniquement au niveau service, total au niveau agrégat), le caveat sur l'attribution runtime (les archives sprint-2+ portent des données per-service runtime-calibrated, les plus anciennes retombent sur la part d'I/O), et deux lignes de fitness réglementaire (inadapté à CSRD / GHG Scope 3, référence méthodologique). Les opérateurs peuvent surcharger la liste dans leur TOML org-config. `reference_urls` est un objet ouvert qui mappe des clés courtes (`methodology`, `schema`, `project`) à des URLs. Les opérateurs peuvent ajouter des clés personnalisées.

## Boavizta et autres champs omis

`boavizta_version` a été envisagé pour `calibration_inputs` mais ne fait pas partie de la v1.0 parce que perf-sentinel ne consomme pas de données Boavizta aujourd'hui. Le champ reviendra quand l'intégration sera livrée. Les consommateurs de schéma DOIVENT tolérer des champs inconnus gracieusement parce que perf-sentinel en ajoutera dans des révisions mineures.

## Versionnement

Un changement incompatible incrémente la version majeure dans `schema_version` (`v2.0`, `v3.0`). Les changements additifs (nouveaux champs optionnels, nouvelles valeurs d'énumération que les consommateurs peuvent traiter comme inconnues) incrémentent la partie mineure (`v1.1`, `v1.2`). L'URL `$id` du JSON Schema ne contient que la version majeure.

## Renvois

- `docs/FR/REPORTING-FR.md` est le guide d'utilisation côté opérateur.
- `docs/FR/METHODOLOGY-FR.md` couvre la chaîne de calcul qui remplit `aggregate` et les champs énergie/carbone par application.
- `docs/schemas/perf-sentinel-report-v1.json` est le JSON Schema autoritaire.
- `docs/schemas/examples/example-internal-G1.json` et `example-official-public-G2.json` sont des exemples remplis.
