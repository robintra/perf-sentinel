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

`intent` vaut `internal | official | audited`. `audited` est réservé pour une release future : le schéma JSON accepte la valeur pour la compatibilité ascendante, mais la CLI le refuse aujourd'hui avec le code de sortie 2. `confidentiality_level` vaut `internal | public`. `integrity_level` vaut `none | hash-only | signed | audited`. Le schéma v1.0 produit `hash-only`. `generated_at` est un timestamp UTC RFC 3339. `generated_by` vaut `daemon | cli-batch | ci`. `perf_sentinel_version` est la chaîne SemVer du binaire qui a écrit le fichier. `report_uuid` est un UUID v4 estampillé par run.

## Organisation

`name` est requis et non vide. `country` est en ISO 3166-1 alpha-2, majuscules. `identifiers` est un objet ouvert avec `siren`, `vat`, `lei`, `opencorporates_url`, `domain` optionnels. `sector` est un code NACE rev2 optionnel.

Le domaine de publication (par exemple `transparency.example.fr`) est traité comme un identifiant implicite quand `notes.reference_urls.project` est publié depuis cet hôte. Le schéma ne l'impose pas, par choix.

## Période

`from_date` et `to_date` sont des dates calendaires ISO 8601 (`YYYY-MM-DD`). `period_type` vaut `calendar-quarter | calendar-month | calendar-year | custom`. `days_covered` vaut `to_date - from_date + 1`. Le validator intent officiel impose `days_covered >= 30`.

## Manifeste de scope

`total_applications_declared` est la taille du portefeuille applicatif de l'organisation. `applications_measured` est le nombre de services pour lesquels le rapport porte des données. Chaque entrée de `applications_excluded` porte `service_name` et un `reason` non vide. `environments_measured` liste les environnements définis par l'opérateur et observés (par exemple `["prod"]`). `total_requests_in_period` est une estimation opérateur optionnelle ; `requests_measured` est ce que perf-sentinel a effectivement vu. `coverage_percentage` vaut `requests_measured / total_requests_in_period * 100` quand le premier est renseigné.

## Méthodologie

`sci_specification` référence la révision SCI (par exemple `"ISO/IEC 21031:2024"`). `perf_sentinel_version` reprend le champ des métadonnées pour les consommateurs qui n'indexent que le bloc méthodologie. `enabled_patterns` et `disabled_patterns` portent chacun des noms de patterns issus du set fermé défini par `FindingType::as_str()` (10 valeurs). `core_patterns_required` est la liste fermée des patterns dont la remédiation coupe directement de l'I/O et du carbone : `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`. `conformance` vaut `core-required | extended | partial`, `core-required` étant le seuil minimum pour un rapport `intent = "official"`. `calibration_inputs.carbon_intensity_source` vaut `electricity_maps | static_tables | mixed`. `specpower_table_version` est la version déclarée par l'opérateur de la table SPECpower / coefficients CCF embarquée, fixée dans le TOML de config org. `binary_specpower_vintage` (0.7.3+) est la chaîne de millésime que le binaire embarque au build, populée automatiquement par `perf-sentinel disclose`. Les consommateurs peuvent comparer les deux chaînes pour détecter une dérive entre la déclaration opérateur et les données embarquées. `scaphandre_used` indique si le proxy énergie temps réel vient de Scaphandre RAPL.

`calibration_applied` (0.7.0+) vaut `true` si au moins une fenêtre de scoring de la période a appliqué des coefficients de calibration opérateur per-service à l'énergie proxy. Le flag est méthodologiquement distinct de `scaphandre_used` et `energy_source_models` : ceux-ci décrivent quelle source d'énergie a produit les chiffres, ce flag décrit si ces chiffres ont été ensuite ajustés par des coefficients opérateur.

## Agrégat

Sommes sur toute la période et tout le tableau `applications`. `total_requests`, `total_energy_kwh`, `total_carbon_kgco2eq` et `estimated_optimization_potential_kgco2eq` sont des nombres finis non négatifs. `aggregate_waste_ratio` est dans `[0, 1]`. `aggregate_efficiency_score` est dans `[0, 100]` et vaut `clamp(100 - 100 * io_waste_ratio, 0, 100)`. `anti_patterns_detected_count` est la somme de toutes les occurrences par service, y compris les patterns non évitables.

### Signaux de qualité (0.7.0+)

L'agrégat porte quatre champs optionnels qui décrivent la qualité des archives sources, pas la charge applicative elle-même. Ils permettent à un auditeur d'évaluer quelle proportion de la période a été mesurée directement plutôt qu'inférée d'un proxy.

- `period_coverage` est dans `[0, 1]` et vaut `runtime_windows / (runtime_windows + fallback_windows)`. Une valeur de `1.0` signifie que toutes les fenêtres de scoring de la période portaient une énergie runtime-calibrated (Scaphandre ou cloud SPECpower). Une valeur de `0.0` signifie que toutes les fenêtres sont tombées sur le proxy I/O. Le validator refuse un rapport `intent = "official"` avec `period_coverage < 0.75`, voir `docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md` pour la justification du seuil.
- `runtime_windows_count` et `fallback_windows_count` portent les compteurs absolus derrière ce ratio, pour qu'un lecteur puisse distinguer "9 fenêtres sur 10 runtime-calibrated" de "900 sur 1000".
- `binary_versions` est l'ensemble des versions distinctes du binaire perf-sentinel qui ont produit les archives repliées dans cette période. Une période qui couvre plusieurs versions (upgrade de daemon en milieu de trimestre, releases asynchrones entre équipes) porte plus d'une entrée dans cet ensemble, ce que le disclaimer du rapport surface.

### Champs de qualité par service (0.7.0+)

- `per_service_energy_models` mappe chaque service à l'ensemble des tags de modèle énergétique observés sur la période (`scaphandre_rapl`, `cloud_specpower`, `io_proxy_v3`, etc.). Le suffixe `+cal` est strippé avant insertion, le flag period-wide `calibration_applied` dans `methodology.calibration_inputs` porte cette information à la place.
- `per_service_measured_ratio` est la moyenne par service de la fraction par fenêtre des spans dont l'énergie a été résolue par Scaphandre ou cloud SPECpower. Une valeur proche de `1.0` signifie que le service est entièrement mesuré sur la période, `0.0` qu'il s'appuie sur le proxy fallback. C'est une moyenne arithmétique simple des ratios par fenêtre, pas pondérée par le nombre de spans : une fenêtre de 10 spans et une fenêtre de 10000 spans contribuent à part égale à la moyenne.

## Applications

Deux granularités, homogènes par rapport. Le validator refuse un rapport qui mélangerait les deux.

### G1 (intent `internal`)

Chaque entrée porte les totaux au niveau service plus un tableau `anti_patterns: [...]`. Chaque détail anti-pattern a `type` (un des 10 patterns connus), `occurrences`, `estimated_waste_kwh`, `estimated_waste_kgco2eq`, `first_seen`, `last_seen`. Les timestamps sont UTC RFC 3339. `display_name` et `service_version` sont des hints optionnels.

### G2 (intent `official` avec confidentiality `public`)

Chaque entrée porte les mêmes totaux au niveau service mais remplace le tableau par un seul entier `anti_patterns_detected_count`. Le schéma impose que les entrées G2 ne portent pas de champ `anti_patterns`, et inversement.

Les deux granularités sont encodées dans le JSON Schema avec des clauses `not: { required: [...] }` mutuellement exclusives pour rendre la discrimination explicite aux validateurs de schéma.

## Intégrité

> **Voir aussi.** L'[introduction à Sigstore](SUPPLY-CHAIN-FR.md#introduction-à-sigstore) dans la doc supply-chain définit Cosign, Fulcio, Rekor, in-toto, OIDC et SLSA utilisés dans cette section.

`content_hash` est `"sha256:<64-hex>"` sur la forme JSON canonique du document avec le champ `content_hash` mis à chaîne vide. Le schéma accepte aussi une chaîne vide pour ce champ afin que les exemples puissent être livrés sans valeur cuite. `binary_hash` est `"sha256:<64-hex>"` du binaire perf-sentinel qui a produit le fichier. `binary_verification_url` pointe vers l'artefact de release où les consommateurs récupèrent le même binaire. `trace_integrity_chain` est réservé pour une révision future et vaut `null` en v1.0.

`signature` (0.7.0+) vaut soit `null` (rapport hash-only) soit un objet typé avec `format` (`"sigstore-cosign-intoto-v1"`), `bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, et `signed_at`. Les champs permettent collectivement à un vérifieur de localiser le bundle cosign et la preuve d'inclusion Rekor.

`binary_attestation` (0.7.0+) est optionnel et, quand présent, porte un `format` (`"slsa-provenance-v1"`), `attestation_url`, `builder_id`, `git_tag`, `git_commit`, et `slsa_level` (`"L2"` pour 0.7.0, `"L3"` à partir de 0.7.1 puisque le workflow de release est passé à `actions/attest-build-provenance` qui produit une attestation niveau 3 par construction). Les consommateurs vérifient le binaire téléchargé depuis `binary_verification_url` avec `gh attestation verify <binary> --owner robintra --repo perf-sentinel` pour les releases 0.7.1+, ou avec `slsa-verifier verify-artifact --provenance-path multiple.intoto.jsonl ...` pour la 0.7.0 legacy.

`integrity_level` dans `report_metadata` vaut `none`, `hash-only`, `signed`, `signed-with-attestation` (0.7.0+), `audited`. Le lecteur peut l'utiliser comme filtre rapide avant de parser le bloc integrity.

## Notes

`disclaimers` porte sept déclarations par défaut : les deux lignes standard d'incertitude SCI (estimation directionnelle, fourchette ~2x), la précision sur le scope embarqué (exclu du potentiel d'optimisation), la note embarqué par service (opérationnel uniquement au niveau service, total au niveau agrégat), le caveat sur l'attribution runtime (les archives runtime-calibrated portent des données per-service, les plus anciennes retombent sur la part d'I/O), et deux lignes de fitness réglementaire (inadapté à CSRD / GHG Scope 3, référence méthodologique). Les opérateurs peuvent surcharger la liste dans leur TOML org-config. `reference_urls` est un objet ouvert qui mappe des clés courtes (`methodology`, `schema`, `project`) à des URLs. Les opérateurs peuvent ajouter des clés personnalisées.

## Boavizta et autres champs omis

`boavizta_version` a été envisagé pour `calibration_inputs` mais ne fait pas partie de la v1.0 parce que perf-sentinel ne consomme pas de données Boavizta aujourd'hui. Le champ reviendra quand l'intégration sera livrée. Les consommateurs de schéma DOIVENT tolérer des champs inconnus gracieusement parce que perf-sentinel en ajoutera dans des révisions mineures.

## Versionnement

Un changement incompatible incrémente la version majeure dans `schema_version` (`v2.0`, `v3.0`). Les changements additifs (nouveaux champs optionnels, nouvelles valeurs d'énumération que les consommateurs peuvent traiter comme inconnues) incrémentent la partie mineure (`v1.1`, `v1.2`). L'URL `$id` du JSON Schema ne contient que la version majeure.

## Renvois

- `docs/FR/REPORTING-FR.md` est le guide d'utilisation côté opérateur.
- `docs/FR/METHODOLOGY-FR.md` couvre la chaîne de calcul qui remplit `aggregate` et les champs énergie/carbone par application.
- `docs/schemas/perf-sentinel-report-v1.json` est le JSON Schema autoritaire.
- `docs/schemas/examples/example-internal-G1.json` et `example-official-public-G2.json` sont des exemples remplis.
