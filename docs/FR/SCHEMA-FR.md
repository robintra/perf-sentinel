# Référence schéma : perf-sentinel-report v1.3

Ce document décrit la forme JSON d'un rapport de transparence périodique en prose. Le JSON Schema lisible par machine se trouve dans `docs/schemas/perf-sentinel-report-v1.json` (draft 2020-12). Deux exemples remplis sont sous `docs/schemas/examples/`.

La v1.1 ajoute les tiers `canonical_waste` et `operational_waste` à `aggregate`. La v1.2 ajoute `aggregate.temporal_coverage` (un signal de continuité de mesure), `scope_manifest.coverage_basis` (un marqueur de provenance) et le hook réservé `integrity.cross_period_log`. La v1.3 ajoute `methodology.standard_crosswalk` (une correspondance interprétative vers les datapoints ESRS E1) et `applications[].anti_patterns[].rgesn_criteria` par pattern (critères RGESN 2024). Le schéma accepte `perf-sentinel-report/v1.0` jusqu'à `v1.3`, et chaque champ ajouté prend une valeur par défaut quand il est absent, donc les lecteurs et rapports plus anciens restent valides et le `content_hash` d'un rapport plus ancien reste identique quand il est rehashé sur un binaire plus récent.

## Clés racine

| clé               | type           | requise | notes                                                                             |
|-------------------|----------------|---------|-----------------------------------------------------------------------------------|
| `schema_version`  | string (enum)  | oui     | `"perf-sentinel-report/v1.3"` (accepte aussi `"…/v1.2"`, `"…/v1.1"`, `"…/v1.0"`)  |
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

Un rapport de divulgation porte trois axes orthogonaux à lire avant tout parsing des données : `intent` indique *à qui le rapport est destiné* (brouillon privé, publication externe, ou publication auditée par un tiers), `confidentiality_level` indique *le niveau de détail par service exposé* (breakdown anti-patterns G1 complet vs comptages G2 agrégés), et `integrity_level` indique *quelles primitives cryptographiques garantissent le document* (rien, content hash seul, signé Sigstore, signé avec attestation de build SLSA). Ensemble, ils permettent à un auditeur ou un journaliste de filtrer le corpus avant de faire confiance à un chiffre à l'intérieur.

`intent` vaut `internal | official | audited`. `audited` est réservé pour une release future : le schéma JSON accepte la valeur pour la compatibilité ascendante, mais la CLI le refuse aujourd'hui avec le code de sortie 2. `confidentiality_level` vaut `internal | public`. `integrity_level` vaut `none | hash-only | signed | signed-with-attestation | audited`. Par défaut, la CLI produit `hash-only`. `generated_at` est un timestamp UTC RFC 3339. `generated_by` vaut `daemon | cli-batch | ci`. `perf_sentinel_version` est la chaîne SemVer du binaire qui a écrit le fichier. `report_uuid` est un UUID v4 estampillé par run.

## Organisation

`name` est requis et non vide. `country` est en ISO 3166-1 alpha-2, majuscules. `identifiers` est un objet ouvert avec `siren`, `vat`, `lei`, `opencorporates_url`, `domain` optionnels. `sector` est un code NACE rev2 optionnel.

Le domaine de publication (par exemple `transparency.example.fr`) est traité comme un identifiant implicite quand `notes.reference_urls.project` est publié depuis cet hôte. Le schéma ne l'impose pas, par choix.

## Période

`from_date` et `to_date` sont des dates calendaires ISO 8601 (`YYYY-MM-DD`). `period_type` vaut `calendar-quarter | calendar-month | calendar-year | custom`. `days_covered` vaut `to_date - from_date + 1`. Le validator intent officiel impose `days_covered >= 30`.

## Manifeste de scope

`total_applications_declared` est la taille du portefeuille applicatif de l'organisation. `applications_measured` est le nombre de services pour lesquels le rapport porte des données. Chaque entrée de `applications_excluded` porte `service_name` et un `reason` non vide. `environments_measured` liste les environnements définis par l'opérateur et observés (par exemple `["prod"]`). `total_requests_in_period` est une estimation opérateur optionnelle, `requests_measured` est ce que perf-sentinel a effectivement vu. `coverage_percentage` vaut `requests_measured / total_requests_in_period * 100` quand le premier est renseigné.

`coverage_basis` (v1.2) rend la frontière de confiance explicite, en bande. Il liste quels champs de scope sont `operator_declared` (assertions non auditées que le binaire ne peut pas vérifier : les dénominateurs `total_applications_declared` et `total_requests_in_period`, plus les listes d'exclusion) versus `machine_derived` (calculés par l'agrégateur depuis les archives : `applications_measured`, `requests_measured`, `coverage_percentage`). Un lecteur de `coverage_percentage` doit traiter son dénominateur comme une assertion opérateur : un opérateur qui fixe `total_requests_in_period` bas peut présenter une couverture proche de 100 % d'un univers qu'il a lui-même défini. C'est inhérent à un modèle d'auto-déclaration, les garanties d'intégrité cryptographique lient le rapport publié, pas l'honnêteté de la taille de portefeuille déclarée. Voir [docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md](design/08-PERIODIC-DISCLOSURE-FR.md).

## Méthodologie

`sci_specification` référence la révision SCI (par exemple `"ISO/IEC 21031:2024"`). `perf_sentinel_version` reprend le champ des métadonnées pour les consommateurs qui n'indexent que le bloc méthodologie. `enabled_patterns` et `disabled_patterns` portent chacun des noms de patterns issus du set fermé défini par `FindingType::as_str()` (10 valeurs). `core_patterns_required` est la liste fermée des patterns dont la remédiation coupe directement de l'I/O et du carbone : `n_plus_one_sql`, `n_plus_one_http`, `redundant_sql`, `redundant_http`. `conformance` vaut `core-required | extended | partial`, `core-required` étant le seuil minimum pour un rapport `intent = "official"`. `calibration_inputs.carbon_intensity_source` vaut `electricity_maps | static_tables | mixed`. `specpower_table_version` est la version déclarée par l'opérateur de la table SPECpower / coefficients CCF embarquée, fixée dans le TOML de config org. `binary_specpower_vintage` (0.7.3+) est la chaîne de millésime que le binaire embarque au build, populée automatiquement par `perf-sentinel disclose`. Les consommateurs peuvent comparer les deux chaînes pour détecter une dérive entre la déclaration opérateur et les données embarquées. `scaphandre_used` indique si le proxy énergie temps réel vient de Scaphandre RAPL. C'est un champ historique spécifique à Scaphandre : il précède les autres backends d'énergie mesurée et n'a jamais été généralisé, il reste donc à `false` pour une période mesurée avec Alumet, Kepler ou Redfish. **`energy_source_models` est la source de vérité générale** pour savoir quelles sources d'énergie ont produit les chiffres d'une période, il est dérivé automatiquement des fenêtres archivées et porte l'étiquette de chaque backend (`alumet_rapl`, `scaphandre_rapl`, `kepler_ebpf`, `redfish_bmc`, `cloud_specpower`, `io_proxy_v*`). Les consommateurs doivent lire `energy_source_models` et traiter `scaphandre_used` comme un indice hérité. Généraliser le champ casserait un schéma publié, haché et attesté, c'est donc reporté à une révision ultérieure du schéma. `standard_crosswalk` (v1.3) est une correspondance interprétative des chiffres du rapport vers les datapoints ESRS E1 (`total_energy_kwh` vers E1-5, le terme carbone opérationnel vers E1-6 Scope 2 location-based, le carbone embarqué vers E1-6 Scope 3). Il porte son propre tableau `caveats`. C'est une aide à la correspondance, pas une certification : le chiffre location-based n'est pas la valeur Scope 2 market-based qu'ESRS exige aussi, et l'intervalle d'incertitude 2x s'applique toujours. Absent sur les rapports pré-v1.3.

`calibration_applied` (0.7.0+) vaut `true` si au moins une fenêtre de scoring de la période a appliqué des coefficients de calibration opérateur per-service à l'énergie proxy. Le flag est méthodologiquement distinct de `scaphandre_used` et `energy_source_models` : ceux-ci décrivent quelle source d'énergie a produit les chiffres, ce flag décrit si ces chiffres ont été ensuite ajustés par des coefficients opérateur.

## Agrégat

> **Voir aussi.** L'[introduction énergie et SCI](METHODOLOGY-FR.md#introduction-énergie-et-sci-v10) dans la doc méthodologie définit les termes SCI v1.0 (E, I, M), `efficiency_score`, `io_waste_ratio`, Scaphandre, SPECpower et le vocabulaire associé utilisé par tous les champs ci-dessous.

Sommes sur toute la période et tout le tableau `applications`. `total_requests`, `total_energy_kwh`, `total_carbon_kgco2eq` et `estimated_optimization_potential_kgco2eq` sont des nombres finis non négatifs. `aggregate_waste_ratio` est dans `[0, 1]`. `aggregate_efficiency_score` est dans `[0, 100]` et vaut `clamp(100 - 100 * io_waste_ratio, 0, 100)`. `anti_patterns_detected_count` est la somme de toutes les occurrences par service, y compris les patterns non évitables.

### Tiers de gaspillage (1.1+)

Le rapport porte le gaspillage évitable, énergie et carbone, à deux seuils de détection N+1, côte à côte, pour rendre l'écart auditable :

- `canonical_waste` est calculé à un seuil N+1 fixe épinglé dans le binaire (`2`), pas la config de l'opérateur. C'est le chiffre non manipulable : un opérateur ne peut pas le réduire en relâchant son propre seuil. C'est le chiffre évitable de référence, et depuis la v1.1 les champs plats `estimated_optimization_potential_kgco2eq`, `aggregate_waste_ratio` et `aggregate_efficiency_score` sont des alias de ce tier (ils portaient la valeur opérationnelle en v1.0).
- `operational_waste` est calculé au seuil N+1 configuré par l'opérateur et enregistre ce seuil dans `n_plus_one_threshold`. Le comparer à `canonical_waste` montre combien de gaspillage évitable le seuil de l'opérateur masque.

Chaque tier porte `n_plus_one_threshold` (entier), `energy_kwh` et `carbon_kgco2eq` (non négatifs), `waste_ratio` (`[0, 1]`) et `efficiency_score` (`[0, 100]`). Pour `intent = "official"`, le validator exige que `canonical_waste.n_plus_one_threshold` égale le seuil canonique du binaire. Le seuil opérationnel est le choix enregistré de l'opérateur et n'est délibérément pas borné, puisqu'un seuil relâché est précisément ce que ce tier sert à exposer. L'énergie et le carbone totaux (`total_energy_kwh`, `total_carbon_kgco2eq`) sont dérivés des spans et indépendants des deux seuils.

### Signaux de qualité (0.7.0+)

L'agrégat porte quatre champs optionnels qui décrivent la qualité des archives sources, pas la charge applicative elle-même. Ils permettent à un auditeur d'évaluer quelle proportion de la période a été mesurée directement plutôt qu'inférée d'un proxy.

- `period_coverage` est dans `[0, 1]` et vaut `runtime_windows / (runtime_windows + fallback_windows)`. Une valeur de `1.0` signifie que toutes les fenêtres de scoring de la période portaient une énergie runtime-calibrated (Scaphandre ou cloud SPECpower). Une valeur de `0.0` signifie que toutes les fenêtres sont tombées sur le proxy I/O. Le validator refuse un rapport `intent = "official"` avec `period_coverage < 0.75`, voir `docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md` pour la justification du seuil.
- `runtime_windows_count` et `fallback_windows_count` portent les compteurs absolus derrière ce ratio, pour qu'un lecteur puisse distinguer "9 fenêtres sur 10 runtime-calibrated" de "900 sur 1000".
- `binary_versions` est l'ensemble des versions distinctes du binaire perf-sentinel qui ont produit les archives repliées dans cette période. Une période qui couvre plusieurs versions (upgrade de daemon en milieu de trimestre, releases asynchrones entre équipes) porte plus d'une entrée dans cet ensemble, ce que le disclaimer du rapport surface.

### Champs de qualité par service (0.7.0+)

- `per_service_energy_models` mappe chaque service à l'ensemble des tags de modèle énergétique observés sur la période (`scaphandre_rapl`, `cloud_specpower`, `io_proxy_v3`, etc.). Le suffixe `+cal` est strippé avant insertion, le flag period-wide `calibration_applied` dans `methodology.calibration_inputs` porte cette information à la place.
- `per_service_measured_ratio` est la moyenne par service de la fraction par fenêtre des spans dont l'énergie a été résolue par Scaphandre ou cloud SPECpower. Une valeur proche de `1.0` signifie que le service est entièrement mesuré sur la période, `0.0` qu'il s'appuie sur le proxy fallback. C'est une moyenne arithmétique simple des ratios par fenêtre, pas pondérée par le nombre de spans : une fenêtre de 10 spans et une fenêtre de 10000 spans contribuent à part égale à la moyenne.

### Couverture temporelle (v1.2)

`temporal_coverage` est un signal de continuité : quelle part de la période déclarée a réellement porté des mesures. C'est un objet avec `temporal_coverage` (dans `[0, 1]`, égal à `observed_days / days_in_period`), `observed_days` (jours calendaires UTC distincts portant au moins une fenêtre archivée), `days_in_period` (reflète `period.days_covered`) et `largest_gap_days` (la plus longue suite de jours consécutifs de la période sans fenêtre).

À lire comme une borne basse de l'activité, pas comme l'uptime du daemon. L'archivage du daemon est déclenché par le trafic : une fenêtre sans trafic n'écrit rien, donc les jours légitimement calmes (nuits, week-ends, services peu sollicités) abaissent le chiffre. Pour cette raison ce n'est **jamais** une barrière dure `official`. La CLI `disclose` publie la valeur, émet un warning sur stderr sous un seuil informatif, et ajoute un disclaimer en bande portant la même mise en garde. Il existe pour qu'un lecteur distingue une période mesurée en continu d'une période où le daemon n'a tourné que quelques jours, ce que le `days_covered` calendaire seul ne peut pas révéler.

## Applications

Deux granularités, homogènes par rapport. Le validator refuse un rapport qui mélangerait les deux.

### G1 (intent `internal`)

Chaque entrée porte les totaux au niveau service plus un tableau `anti_patterns: [...]`. Chaque détail anti-pattern a `type` (un des 10 patterns connus), `occurrences`, `estimated_waste_kwh`, `estimated_waste_kgco2eq`, `first_seen`, `last_seen`. Les timestamps sont UTC RFC 3339. `rgesn_criteria` (v1.3) est la liste interprétative des critères RGESN 2024 auxquels le pattern se rapporte (voir [docs/FR/METHODOLOGY-FR.md](METHODOLOGY-FR.md#correspondance-rgesn-2024)), vide pour `slow_*` et absente sur les rapports pré-v1.3. `display_name` et `service_version` sont des hints optionnels.

### G2 (intent `official` avec confidentiality `public`)

Chaque entrée porte les mêmes totaux au niveau service mais remplace le tableau par un seul entier `anti_patterns_detected_count`. Le schéma impose que les entrées G2 ne portent pas de champ `anti_patterns`, et inversement.

Les deux granularités sont encodées dans le JSON Schema avec des clauses `not: { required: [...] }` mutuellement exclusives pour rendre la discrimination explicite aux validateurs de schéma.

## Intégrité

> **Voir aussi.** L'[introduction à Sigstore](SUPPLY-CHAIN-FR.md#introduction-à-sigstore) dans la doc supply-chain définit Cosign, Fulcio, Rekor, in-toto, OIDC et SLSA utilisés dans cette section.

`content_hash` est `"sha256:<64-hex>"` sur la forme JSON canonique du document avec le champ `content_hash` mis à chaîne vide. Le schéma accepte aussi une chaîne vide pour ce champ afin que les exemples puissent être livrés sans valeur cuite. `binary_hash` est `"sha256:<64-hex>"` du binaire perf-sentinel qui a produit le fichier. `binary_verification_url` pointe vers l'artefact de release où les consommateurs récupèrent le même binaire. `trace_integrity_chain` est réservé pour une révision future et vaut `null` aujourd'hui.

`signature` (0.7.0+) vaut soit `null` (rapport hash-only) soit un objet typé avec `format` (`"sigstore-cosign-intoto-v1"`), `bundle_url`, `signer_identity`, `signer_issuer`, `rekor_url`, `rekor_log_index`, et `signed_at`. Les champs permettent collectivement à un vérifieur de localiser le bundle cosign et la preuve d'inclusion Rekor.

`binary_attestation` (0.7.0+) est optionnel et, quand présent, porte un `format` (`"slsa-provenance-v1"`), `attestation_url`, `builder_id`, `git_tag`, `git_commit`, et `slsa_level` (`"L2"` pour 0.7.0, `"L3"` à partir de 0.7.1 puisque le workflow de release est passé à `actions/attest-build-provenance` qui produit une attestation niveau 3 par construction). Les consommateurs vérifient le binaire téléchargé depuis `binary_verification_url` avec `gh attestation verify <binary> --owner robintra --repo perf-sentinel` pour les releases 0.7.1+, ou avec `slsa-verifier verify-artifact --provenance-path multiple.intoto.jsonl ...` pour la 0.7.0 legacy.

`cross_period_log` (v1.2) est réservé et absent aujourd'hui. C'est le hook de schéma pour un journal externe en ajout seul ou de type Rekor qui chaîne les rapports périodiques successifs, afin qu'un tiers puisse détecter un opérateur qui aurait arrêté de publier sur plusieurs périodes, le seul trou que les garanties d'intégrité par rapport ne peuvent pas combler. Il ne sera renseigné que sous un futur `intent = "audited"`, aux côtés de l'attestation d'audit externe.

`integrity_level` dans `report_metadata` vaut `none`, `hash-only`, `signed`, `signed-with-attestation` (0.7.0+), `audited`. Le lecteur peut l'utiliser comme filtre rapide avant de parser le bloc integrity.

## Notes

`disclaimers` porte huit déclarations par défaut : les deux avertissements standard d'incertitude (estimation directionnelle, fourchette multiplicative ~2x, la spécification SCI elle-même ne définit aucune disposition d'incertitude), la précision sur le scope embarqué (exclu du potentiel d'optimisation), la note embarqué par service (opérationnel uniquement au niveau service, total au niveau agrégat), le caveat sur l'attribution runtime (les archives runtime-calibrated portent des données per-service, les plus anciennes retombent sur la part d'I/O), deux lignes de fitness réglementaire (inadapté à CSRD / GHG Scope 3, référence méthodologique), et la note de crosswalk ESRS E1 (la correspondance `standard_crosswalk` est une aide, pas un substitut à un inventaire CSRD audité). Les opérateurs peuvent surcharger la liste dans leur TOML org-config. `reference_urls` est un objet ouvert qui mappe des clés courtes (`methodology`, `schema`, `project`) à des URLs. Les opérateurs peuvent ajouter des clés personnalisées.

## Boavizta et autres champs omis

`boavizta_version` a été envisagé pour `calibration_inputs` mais ne fait pas partie du schéma actuel parce que perf-sentinel ne consomme pas de données Boavizta aujourd'hui. Le champ reviendra quand l'intégration sera livrée. Les consommateurs de schéma DOIVENT tolérer des champs inconnus gracieusement parce que perf-sentinel en ajoutera dans des révisions mineures.

## Versionnement

Un changement incompatible incrémente la version majeure dans `schema_version` (`v2.0`, `v3.0`). Les changements additifs (nouveaux champs optionnels, nouvelles valeurs d'énumération que les consommateurs peuvent traiter comme inconnues) incrémentent la partie mineure (`v1.1`, `v1.2`). L'URL `$id` du JSON Schema ne contient que la version majeure.

## Renvois

- `docs/FR/REPORTING-FR.md` est le guide d'utilisation côté opérateur.
- `docs/FR/METHODOLOGY-FR.md` couvre la chaîne de calcul qui remplit `aggregate` et les champs énergie/carbone par application.
- `docs/schemas/perf-sentinel-report-v1.json` est le JSON Schema autoritaire.
- `docs/schemas/examples/example-internal-G1.json` et `example-official-public-G2.json` sont des exemples remplis.
