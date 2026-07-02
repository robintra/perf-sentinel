# Rapport public périodique

Notes de design pour le pipeline de transparence : schéma (actuel v1.3), aggregator, validator, archive daemon, et la subcommand `disclose`. La doc opérateur vit dans `docs/FR/REPORTING-FR.md`, la chaîne de calcul dans `docs/FR/METHODOLOGY-FR.md`, la référence wire dans `docs/FR/SCHEMA-FR.md`. Ce document explique les décisions de design derrière chaque module.

## Disposition des modules

```
crates/sentinel-core/src/report/periodic/
  ├── mod.rs        // re-exports
  ├── schema.rs     // types wire v1.3
  ├── errors.rs     // ValidationError, HashError, AggregationError
  ├── hasher.rs     // JSON canonique + SHA-256 + binary_hash helper
  ├── validator.rs  // validate_official, validate_content_hash
  ├── aggregator.rs // lecteur archive NDJSON, attribution par service
  └── org_config.rs // loader TOML opérateur

crates/sentinel-core/src/daemon/archive.rs   // writer d'archive
crates/sentinel-cli/src/disclose.rs          // dispatcher CLI
```

La répartition reprend le pattern pipeline du reste de la crate : fonctions pures sur les données, traits uniquement aux frontières d'I/O (`std::fs` pour l'org-config et l'archive, `tokio::sync::mpsc` pour la tâche writer). Pas de nouvelle abstraction entre les étages.

## Déterminisme du schéma

Le content hash est un SHA-256 sur la forme JSON canonique du rapport avec `integrity.content_hash` mis à chaîne vide. Trois invariants rendent cela reproductible entre builds et entre consommateurs :

1. **L'ordre des champs est celui de la déclaration des structs.** `serde_json` préserve l'ordre des champs lors de la sérialisation. Réorganiser des champs dans `schema.rs` casse le hash et doit donc s'accompagner d'un bump de version de schéma.
2. **Chaque map est un `BTreeMap`.** `HashMap` itère dans un ordre non déterministe et défait le hash. Le schéma utilise `BTreeMap<String, String>` pour `notes.reference_urls`, et les buffers intermédiaires de l'aggregator (`per_service`, `anti_patterns`, `first_seen`, `last_seen`) suivent la même discipline.
3. **`Application::G1` et `Application::G2` sont en `#[serde(untagged)]`.** Pas de discriminateur, la dispatch se fait par présence de champ requis (`anti_patterns` pour G1, `anti_patterns_detected_count` pour G2). Le tableau applications est imposé homogène par le validator, donc le niveau type est permissif mais l'invariant runtime est strict.

L'implémentation du hasher (`hasher.rs`) lance ensuite `canonicalize(Value)` qui reconstruit chaque objet JSON via `BTreeMap<String, Value>` et récurse dans les tableaux. C'est défensif : `serde_json::Map` sans la feature `preserve_order` est déjà un `BTreeMap`, mais le passage explicite garde l'implémentation correcte si une dépendance future active la feature de manière transitive.

La sortie du hash est `"sha256:<64-hex>"`. L'encodage hexadécimal est fait à la main (`{byte:02x}`) pour éviter d'ajouter la crate `hex`, en cohérence avec le pattern existant dans `crate::acknowledgments`.

### Pourquoi vider la valeur plutôt que retirer la clé

Mettre `content_hash` à `""` (chaîne vide) préserve la clé dans la forme canonique. Les consommateurs qui vérifient le hash n'ont pas à savoir s'il faut ajouter ou retirer la clé, ils remplacent simplement la valeur lue par `""` et recompute. Le schéma accepte à la fois `^sha256:[0-9a-f]{64}$` et la chaîne vide pour ce champ, ce qui permet aux exemples d'être livrés avec un placeholder.

## Granularité G1 / G2

Les deux granularités existent parce qu'un rapport de transparence publiable ne doit pas exposer le détail par anti-pattern (qui se lit comme un runbook des faiblesses) tandis que les brouillons internes en bénéficient. Le validator impose :

- `confidentiality = "internal"` accepte G1 ou G2.
- `confidentiality = "public"` exige G2.
- Mélanger des entrées G1 et G2 dans le même tableau `applications` est refusé.

Le choix de `#[serde(untagged)]` plutôt qu'un discriminateur explicite a été fait pour ces raisons :

- La discrimination est structurelle (`anti_patterns` vs `anti_patterns_detected_count`) et le JSON Schema sait déjà l'exprimer avec `oneOf` plus des contraintes `not: { required }`.
- Le tableau applications est censé être homogène, donc un consommateur externe parsant le JSON n'a pas à gérer un tableau aux tags mixtes.
- Les appelants Rust internes travaillent aussi en pratique sur une slice homogène, donc le `match` sur `Application::G1(_)` / `Application::G2(_)` reste local à quelques sites du builder CLI.

## Validator collect-all

`validate_official` retourne `Result<(), Vec<ValidationError>>` et accumule toutes les violations en un seul passage plutôt que de quitter à la première. Raisons :

- Les opérateurs configurant un daemon `intent = "official"` corrigent l'org-config en un aller-retour au lieu de découvrir les champs manquants un par un à chaque redémarrage.
- Les relecteurs face à un échec CLI voient immédiatement la liste complète des problèmes structurels.

La fonction dispatche vers des helpers par section (`validate_organisation`, `validate_period`, `validate_scope_manifest`, `validate_methodology`, `validate_aggregate`, `validate_applications`). Chaque helper prend `&mut Vec<ValidationError>` et push. Les sous-règles à l'intérieur d'un helper continuent à s'exécuter après un push : par exemple, le helper méthodologie valide chaque entrée de `enabled_patterns` et `core_patterns_required` contre `KNOWN_PATTERNS` même si une entrée plus tôt a déjà été refusée.

`KNOWN_PATTERNS` est un `const &[&str]` dans `validator.rs` qui reflète les variants de `FindingType`. Un test (`known_patterns_matches_finding_type_count`) utilise un match exhaustif sur `FindingType` pour forcer un échec CI si un futur variant est ajouté sans mise à jour de la liste.

`intent = "internal"` est un no-op : un brouillon a le droit d'être incomplet. `intent = "audited"` court-circuite avec un unique `ValidationError::AuditedNotImplemented`, accepté par le JSON schema pour la compatibilité ascendante mais non implémenté au runtime.

## Aggregator et attribution par service

L'aggregator lit des fichiers NDJSON (ou des dossiers contenant des `*.ndjson`), où chaque ligne est une enveloppe :

```json
{"ts":"<RFC 3339 UTC>","report":{...Report complet...}}
```

Pour chaque enveloppe dans la période :

1. **Compteurs globaux** somment `total_io_ops`, `avoidable_io_ops`, `total.mid` (gCO2), `avoidable.mid` (gCO2). gCO2 est divisé par 1000 pour obtenir kgCO2eq.
2. **Distribution par service** lit `Report.per_endpoint_io_ops` pour l'ensemble des services qui ont produit des I/O dans la fenêtre. Chaque service reçoit une part de l'énergie/carbone de la fenêtre proportionnelle à sa part d'I/O ops.
3. **Attribution des findings** parcourt `Report.findings`. Chaque finding est rangé sous son `service` et son `finding_type.as_str()`. `first_seen` et `last_seen` suivent la plage de timestamps de fenêtre par `(service, pattern_type)`.

Quand une fenêtre a zéro entrée dans `per_endpoint_io_ops`, ses totaux globaux tombent dans le bucket `"_unattributed"` et le bucket apparaît dans le tableau applications. C'est un arbitrage assumé : ignorer silencieusement la fenêtre gonflerait les parts par service des fenêtres suivantes, refuser l'exécution sur une seule fenêtre creuse serait trop agressif pour beaucoup de déploiements réels. Le flag `--strict-attribution` (et la variante `AggregationError::UnattributedWindow` associée) est la porte de sortie pour les opérateurs qui préfèrent la posture stricte.

Les lignes malformées (échecs de parse) sont sautées avec un `tracing::warn!` et comptées dans `malformed_lines_skipped`. L'aggregator ne refuse pas de continuer sur des erreurs de parse isolées. La motivation est l'archive daemon : une ligne partiellement écrite pendant un crash ne doit pas empoisonner toute la période.

## Writer d'archive daemon

Le writer est une tâche `tokio::spawn` alimentée par un `tokio::sync::mpsc::Sender<OwnedArchive>` borné, capacité 256. Côté producteur (dans `process_traces`, sur le worker d'analyse), `archive::try_send(tx, OwnedArchive { ts, report })` évite que la boucle de scoring par fenêtre ne bloque sur l'I/O disque. Envoyer un `OwnedArchive` typé (et non une chaîne pré-sérialisée) sort le coût `serde_json::to_string` du chemin chaud et laisse la tâche writer l'amortir contre l'I/O disque.

Le canal borné applique une politique drop-on-full : quand le writer prend du retard, les nouvelles fenêtres sont jetées avec un `tracing::warn!`. La capacité 256 est dimensionnée pour que l'état d'un writer bloqué en régime permanent remonte en quelques secondes plutôt qu'un canal unbounded qui ferait OOM le daemon.

La rotation se déclenche quand `bytes_written` dépasse `max_size_mb * 1_048_576`. Le fichier actif est renommé en `<stem>-<UTC-timestamp>.ndjson` d'abord, puis un nouveau fichier est ouvert via `OpenOptions::create_new(true).append(true)` pour fermer la course TOCTOU où un attaquant co-résident pourrait planter un symlink entre le rename et la réouverture. `prune` retire les plus anciens fichiers tournés jusqu'à n'en conserver au plus que `max_files`. Le pruning trie par `mtime` décroissant et valide que le suffixe timestamp correspond à la forme `is_rotation_stamp`, ainsi un fichier sans rapport dans le répertoire d'archive (par exemple `archive-evil.ndjson`) n'est jamais supprimé.

`metadata_len` lit la taille du fichier existant au démarrage pour que le writer reprenne correctement après un redémarrage du daemon sans tourner immédiatement un fichier presque plein.

### Pourquoi archiver des `Report` plutôt que des findings

L'aggregator a besoin de `green_summary` (pour énergie/carbone) et de `per_endpoint_io_ops` (pour l'attribution par service). Un flux de `findings` seul ne porte pas ces données. Le daemon construit un `Report` depuis `findings + green_summary + per_endpoint_io_ops + analysis` juste après `emit_findings_and_update_metrics`, puis envoie l'enveloppe sérialisée. Le coût est un `Vec<Finding>::clone` et un `serde_json::to_string` par fenêtre quand l'archive est activée.

`per_endpoint_io_ops` était auparavant lié à `_` dans `process_traces` (la valeur était déjà calculée par `score_green` mais jetée). La garder pour l'archive est un changement sans coût dans le chemin chaud.

### Tier évitable canonique à l'archivage (1.1+)

Le `n_plus_one_threshold` de l'opérateur décide quels patterns N+1 deviennent des findings, donc un seuil relâché réduit l'énergie/carbone évitable que la disclosure déclarerait. Comme `disclose` ne fait que sommer des chiffres déjà archivés et ne peut pas re-détecter (les findings supprimés par un seuil élevé sont absents de l'archive), le chiffre non manipulable doit être produit là où les traces brutes existent encore : le chemin d'archivage du daemon.

`score::canonical::compute_disclosure_waste` exécute une passe N+1 + redondant supplémentaire au seuil épinglé `DISCLOSURE_N_PLUS_ONE_THRESHOLD` (`2`) et rééchelonne l'énergie/carbone évitable depuis `operational_gco2` et `accounted_io_ops` du résumé opérationnel (pas de second calcul carbone complet). Il renvoie les deux tiers, archivés sur `Report.disclosure_waste` : `canonical` au seuil épinglé et `operational` à celui de l'opérateur. Le tableau de bord live et `findings_store` gardent la sémantique opérationnelle, donc seule l'archive de disclosure porte le tier canonique. L'aggregator replie les deux tiers dans `aggregate.canonical_waste` / `operational_waste`, les champs plats évitables étant des alias du tier canonique. La passe supplémentaire n'est payée que quand l'archivage est activé.

Une amélioration différée estamperait le seuil canonique par fenêtre et réconcilierait à travers un parc de binaires hétérogène à l'agrégation. Aujourd'hui l'aggregator réconcilie les seuils par `max` et expose les binaires producteurs via `aggregate.binary_versions`.

Le validator authentifie le *label* canonique (`canonical_waste.n_plus_one_threshold == 2`), pas la *magnitude* des chiffres archivés : une ligne NDJSON falsifiée peut porter le seuil 2 avec des compteurs dégonflés et passer quand même. C'est inhérent à un modèle d'auto-déclaration. Le `content_hash` (et l'attestation cosign optionnelle) lie l'intégrité du *rapport publié*. L'honnêteté des archives sources repose sur le `binary_hash` du binaire et la provenance SLSA, pas sur l'aggregator. Les compteurs issus des archives sont sommés avec `saturating_add`, pour qu'une valeur proche de `u64::MAX` forgée ne puisse pas faire déborder un total de période vers un petit nombre (sous-déclaré).

## TOML org-config

Le TOML fourni par l'opérateur est un blueprint partiel pour les champs statiques d'un `PeriodicReport`. Il porte `organisation`, `methodology`, `scope_manifest` (sans les chiffres runtime) et `notes` optionnel. L'aggregator remplit les sections runtime (`aggregate`, `applications`, `integrity`).

`load_from_path` retourne `OrgConfig` ou `OrgConfigError` (`Io` ou `Parse`). `validate_for_official` retourne `Vec<String>` plutôt que des erreurs typées parce que le daemon les aplatit dans `DaemonError::ReportingValidation { errors: Vec<String> }` pour des logs de démarrage lisibles. La subcommand `disclose` côté CLI appelle le typé `validate_official` sur le rapport entièrement assemblé, ce qui lui permet de remonter aussi les violations au niveau agrégat (par exemple `applications` vide, ratio hors plage).

Les champs TOML reflètent verbatim le schéma wire. C'est délibéré : un opérateur qui lit le JSON Schema peut écrire le TOML sans consulter un deuxième document, et un mainteneur qui renomme un champ wire doit le renommer aux deux endroits.

## Garde-fou au démarrage du daemon

`daemon::run` appelle `validate_official_reporting` avant d'allouer la moindre ressource. Le helper :

1. Retourne `Ok` quand `[reporting] intent != "official"`.
2. Charge l'org-config depuis `[reporting] org_config_path`. Chemin manquant ou fichier illisible devient une entrée dans le vec d'erreurs.
3. Appelle `org_config::validate_for_official` et fold son `Vec<String>` dans le même vec.
4. Retourne `Err(DaemonError::ReportingValidation { errors })` si quoi que ce soit échoue, avec un `Display` qui produit une ligne indentée par erreur pour que journalctl / kubectl logs rendent proprement.

Les listeners ne démarrent pas quand la validation échoue, le daemon sort avec un statut non zéro. Les opérateurs qui préfèrent un mode souple fixent `intent = "internal"` (ou omettent la section).

## Dispatcher CLI

`Commands::Disclose` a été choisi plutôt qu'une extension de `Commands::Report` existant pour ne pas casser la surface CLI (`Report` est déjà la subcommand du dashboard HTML/JSON). Le verbe `disclose` correspond au vocabulaire opérateur de publication de transparence et se lit bien dans des scripts shell.

Le dispatcher (`disclose.rs::cmd_disclose`) retourne `i32` pour que l'appelant puisse faire `std::process::exit(code)` directement. Le contrat :

- `0` : succès, fichier écrit.
- `1` : échec I/O ou parse (org-config illisible, output non écrivable, erreur hash).
- `2` : échec de validation ou court-circuit `audited`. La liste d'erreurs est imprimée sur stderr.

`audited` est intercepté en premier, avant toute I/O, pour que l'utilisateur reçoive le message « not yet implemented » quel que soit l'état de l'org-config.

`generated_by` vaut `"ci"` quand `$CI` est dans l'environnement, `"cli-batch"` sinon. Le chemin daemon utilisera `"daemon"` quand les disclosures planifiées seront ajoutées, c'est un placeholder pour les trois valeurs documentées du champ.

## Commandes de vérification

Un consommateur recompute le content hash avec :

```bash
jq -c '.integrity.content_hash = ""' perf-sentinel-report.json \
  | jq -cS '.' \
  | shasum -a 256
```

L'étape `jq -cS` canonicalise les clés d'objet via le flag `S` intégré à jq, ce qui correspond à l'étape `canonicalize` de `hasher.rs`. Le formatage des nombres peut différer sur des entrées avec des représentations JSON non par défaut des flottants, le schéma n'utilise que des `f64` que `serde_json` émet sous la forme la plus courte qui round-trip, ce qui est aussi ce que jq émet, donc en pratique les deux produisent les mêmes octets.

## Hooks de configuration

Deux nouvelles sections dans `.perf-sentinel.toml` :

- `[reporting]` porte `intent`, `confidentiality_level`, `org_config_path`, `disclose_output_path`, `disclose_period`. Validée au load.
- `[daemon.archive]` porte `path`, `max_size_mb` (défaut 100), `max_files` (défaut 12). Validée au load et à l'ouverture d'archive.

Les deux sections sont optionnelles. Leur absence laisse perf-sentinel dans son comportement antérieur : NDJSON sur stdout, pas d'archive, pas de garde-fou de reporting.

## Limitations v1.0 portées en disclaimers

- **Énergie + carbone par service runtime-calibrated quand l'archive les porte.** `Builder::process_window` lit `green_summary.energy_kwh` et les maps `per_service_carbon_kgco2eq` / `per_service_energy_kwh` / `per_service_region` de la fenêtre source quand elles sont peuplées, et tombe sur le proxy I/O + part proportionnelle quand elles ne le sont pas (archives proxy-only). L'aggregator expose les tags `energy_model` observés sous `methodology.calibration_inputs.energy_source_models`. Voir `docs/FR/design/09-CARBON-ATTRIBUTION-FR.md`.
- **Le potentiel d'optimisation exclut l'embarqué.** `estimated_optimization_potential_kgco2eq` ne somme que `co2.avoidable.mid`. `total_carbon_kgco2eq` est le `co2.total.mid` complet (opérationnel + embarqué). Les disclaimers par défaut le précisent.
- **`_unattributed` co-route les findings.** Une fenêtre sans `per_endpoint_io_ops` et sans maps runtime per-service range son énergie/carbone ET ses findings sous `_unattributed`. Sans ce routage, un service avec des findings N+1 pourrait être publié à `efficiency_score = 100` si son `total_io_ops` se trouve à zéro dans la même fenêtre.

## Le seuil de 75% de calibration runtime

La constante `MIN_PERIOD_COVERAGE_FOR_OFFICIAL` dans `report::periodic::validator` gate une disclosure d'intent `official` à `period_coverage >= 0.75`. Les rapports en deçà sont refusés avec un message qui invite l'opérateur à raccourcir la période ou à retomber sur `intent = internal`.

### Pourquoi 75% et pas une autre valeur

Le seuil équilibre deux modes de défaillance.

- **Trop strict** (par exemple 95%) : rejette les migrations légitimes. Un opérateur qui déploie Scaphandre en milieu de trimestre ne produirait jamais un rapport officiel pour ce trimestre, même si trois quarts des données sont correctement calibrées.
- **Trop permissif** (par exemple 50%) : autorise des rapports où la moitié des données vient du proxy fallback. L'énergie agrégée et l'attribution par service sous-estimeraient ou déformeraient silencieusement le total de la période pour la moitié des fenêtres.

### Justification empirique

Le choix de 75% reflète trois observations.

- Une migration opérateur typique (déploiement de Scaphandre sur une flotte, passage de on-prem à cloud SPECpower, redéploiement du daemon avec une nouvelle config) prend une à deux semaines. Sur un trimestre civil de 90 jours, cela représente 11 à 22% de la période. Un seuil à 75% accommode ce type de migration sans rejeter le rapport produit.
- En dessous de 75%, le proxy fallback contribue à plus d'un quart de l'estimation énergétique totale. Le proxy est uniforme entre services et régions, sa part dilue donc à la fois le total runtime-calibrated et l'attribution par service. Un rapport où le proxy porte plus d'un quart du signal n'est pas honnêtement décrit comme "runtime-calibrated".
- Le seuil de 75% s'aligne avec `MIN_DAYS_COVERED = 30` de manière heuristique. Sur un trimestre, une fenêtre de 30 jours avec couverture complète représente un tiers de la période. Combiné à l'exigence que le reste soit majoritairement calibré pour rester au-dessus de 75%, le gate dessine une forme cohérente de "assez de données, assez de calibration".

### Quand le reconsidérer

Ce seuil n'est pas normatif. Si un retour terrain d'opérateurs ou d'auditeurs montre qu'il est trop strict (rapports `internal` qui atterrissent régulièrement juste sous 75% et qui auraient été utiles en `official`) ou trop permissif (un audit identifie qu'un quart de proxy suffit à masquer une régression), il doit être ajusté. La constante vit dans `crates/sentinel-core/src/report/periodic/validator.rs` et est ré-exportée via le module `report::periodic`.

## Couverture temporelle (v1.2)

`period_coverage` (ci-dessus) répond à "quelle part de la période était runtime-calibrated", pas à "quelle part de la période a été mesurée tout court". Les deux sont indépendants : un daemon qui n'a tourné que trois jours sur 90 déclarés peut quand même rapporter `period_coverage = 1.0` si ces trois jours étaient pleinement calibrés. Rien dans le schéma v1.1 ne révélait ce trou. `days_covered` est de l'arithmétique calendaire pure (`(to - from) + 1`), il décrit la fenêtre déclarée par l'opérateur, pas l'activité réelle du daemon.

`aggregate.temporal_coverage` comble ce trou. L'agrégateur suit l'ensemble des jours calendaires UTC distincts portant au moins une fenêtre foldée (`Builder.observed_days`, inséré dans `process_window` juste après que la fenêtre est validée, pour rester aligné avec `windows_aggregated`). `finalize` divise ce décompte par `period.days_covered` et enregistre aussi `observed_days`, `days_in_period` et `largest_gap_days` (la plus longue suite de jours consécutifs de la période sans fenêtre).

### Pourquoi un warning publié, pas une barrière

L'archivage du daemon est **déclenché par le trafic**, pas par une minuterie. `process_traces` retourne tôt sur un lot vide et le `try_send` d'archive est après ce garde, donc une fenêtre sans trafic n'écrit aucune ligne NDJSON. Par conséquent `temporal_coverage` mesure les *jours avec trafic observé*, une borne basse de l'activité, pas l'uptime du daemon. Les jours légitimement calmes (nuits, week-ends, services peu sollicités, un service sans requête un jour férié) l'abaissent. Une barrière dure `official` rejetterait donc des rapports honnêtes de déploiements intermittents ou peu sollicités. Donc `validate_official` ne fait que vérifier la plage du champ (`[0, 1]`, fini) et ne bloque jamais dessus. La CLI `disclose` publie la valeur, affiche un warning sur stderr sous `LOW_TEMPORAL_COVERAGE_WARN_THRESHOLD`, et ajoute un disclaimer en bande (couvert par le hash) portant la mise en garde du traffic-gating. Le lecteur juge.

### Ce qu'il adresse et ce qu'il n'adresse pas

C'est le signal in-binary le plus proche de l'échappatoire d'auto-déclaration "il suffit d'arrêter perf-sentinel une partie de la période". L'extinction partielle se voit maintenant comme un `temporal_coverage` bas et un `largest_gap_days` grand. Il n'adresse **pas** la non-participation totale (ne jamais lancer l'outil ne laisse aucun rapport) ni un dénominateur malhonnête (`total_requests_in_period` fixé bas), tous deux irréductibles sans infrastructure externe, voir Révisions futures. Deux vérifications de cohérence bon marché l'accompagnent : `days_covered` doit valoir `(to_date - from_date) + 1` (rejet dur, seul un fichier édité à la main peut échouer) et `requests_measured` ne doit pas dépasser un `total_requests_in_period` déclaré par l'opérateur (rejet dur).

## Crosswalk standard et critères RGESN (v1.3)

La v1.3 ajoute deux champs de correspondance interprétative, dont aucun n'est une barrière. `methodology.standard_crosswalk` est un crosswalk vers les datapoints ESRS E1 : une aide de correspondance qui pointe chaque chiffre de la disclosure vers le datapoint CSRD / ESRS E1 le plus proche, avec un disclaimer explicite indiquant qu'il ne remplace pas un inventaire audité. `applications[].anti_patterns[].rgesn_criteria` tague chaque anti-pattern détecté avec les critères RGESN 2024 (Référentiel général d'écoconception de services numériques) auxquels il se rattache, pour qu'un auditeur écoconception puisse relier un finding au référentiel. Les deux sont des extensions additives `#[serde(default)]` : les lecteurs plus anciens les ignorent, et un rapport écrit sans eux se rehashe à l'identique. La référence wire des deux vit dans `docs/FR/SCHEMA-FR.md`.

## Révisions futures

- **Signature Sigstore** : `integrity.signature` est réservé. Ajouter une vraie signature est un bump mineur SemVer du schéma (champ additif passant non null dans certains fichiers).
- **Intent `audited`** : la troisième valeur d'intent demandera une attestation d'audit externe. La forme vivra sous `integrity` ou dans une section voisine, pas encore tranché.
- **Chaîne d'intégrité de traces** : `integrity.trace_integrity_chain` est réservé pour une racine de Merkle sur les traces sources alimentant la disclosure. Hors scope du schéma actuel.
- **Journal inter-périodes** : `integrity.cross_period_log` (réservé en v1.2) est le hook pour un journal externe en ajout seul ou de type Rekor chaînant les `content_hash` successifs entre périodes. C'est ce qui rend la non-participation totale (un opérateur qui arrête de publier) détectable par un tiers, le trou qu'aucune garantie d'intégrité par rapport ne peut combler. Il ne sera renseigné que sous `intent = "audited"`. Comme c'est du contenu disclosé (toujours `None` aujourd'hui, omis du wire), il n'est volontairement pas dans `POST_SIGN_FIELDS`, donc les hashs des rapports actuels ne changent pas.
- **Intégration Boavizta** : `methodology.calibration_inputs` gagnera un champ `boavizta_version` quand l'intégration sera livrée. Les consommateurs de schéma doivent tolérer des champs de calibration inconnus, ce qu'ils font déjà parce que `additionalProperties` n'est pas posé.

## Mapping des fichiers source

| Fichier source                                         | Sujet                                          |
|--------------------------------------------------------|------------------------------------------------|
| `report/periodic/schema.rs`                            | types wire, invariants de déterminisme         |
| `report/periodic/hasher.rs`                            | JSON canonique + SHA-256, binary hash          |
| `report/periodic/validator.rs`                         | validator collect-all, KNOWN_PATTERNS          |
| `report/periodic/aggregator.rs`                        | folding NDJSON, attribution par service        |
| `report/periodic/org_config.rs`                        | loader TOML opérateur                          |
| `report/periodic/errors.rs`                            | enums d'erreur                                 |
| `daemon/archive.rs`                                    | writer NDJSON non bloquant avec rotation/prune |
| `daemon/mod.rs` (`validate_official_reporting`)        | garde-fou de démarrage                         |
| `daemon/event_loop.rs`                                 | hook archive dans `process_traces`             |
| `config/` (`ReportingConfig`, `DaemonArchiveConfig`) | sections TOML + validators                     |
| `sentinel-cli/src/disclose.rs`                         | dispatcher CLI, value enums, build_report      |
