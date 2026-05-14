# Attribution carbone par service

Notes de design pour l'attribution par service runtime-calibrated de l'énergie et du carbone, exposée dans `GreenSummary` et consommée par l'aggregator du rapport périodique. À lire avec `docs/FR/METHODOLOGY-FR.md` (côté opérateur) et `docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md` (aggregator + schéma).

## Pourquoi

La première version du rapport recomputait `aggregate.total_energy_kwh` via un proxy au moment de l'agrégation, même quand le daemon sous-jacent avait mesuré l'énergie via Scaphandre ou cloud SPECpower. Elle distribuait aussi le CO2 par fenêtre aux services proportionnellement à la part d'I/O ops, ignorant que deux services dans des régions différentes émettent à des intensités très différentes.

La correction consiste à calculer et sérialiser l'énergie + le carbone par service au moment du scoring, pour que l'aggregator puisse sommer directement. Les valeurs par service sont runtime-calibrated de bout en bout : le daemon voit la vraie région de chaque service et le vrai tag de backend énergétique.

## Algorithme

Le scoring tourne dans `score::compute_carbon_report`. La fonction boucle déjà une fois sur tous les spans du batch et accumule du carbone par région dans `RegionAccumulator`. La nouvelle implémentation ajoute en parallèle une `BTreeMap<String, ServiceCarbonAccumulator>` qui suit la même forme single-pass.

Pour chaque span, après calcul de l'énergie, de la région, de l'intensité et du PUE, la boucle interne exécute aussi :

```rust
let svc = state
    .per_service
    .entry(span.event.service.to_string())
    .or_insert_with(|| ServiceCarbonAccumulator {
        energy_kwh: 0.0,
        operational_gco2: 0.0,
        region: region_ctx.region_ref.to_string(),
    });
svc.energy_kwh += energy_kwh;
svc.operational_gco2 += op_co2;
```

Une fois la boucle terminée, `score_green` produit les maps du GreenSummary :

- `per_service_energy_kwh[svc] = acc.energy_kwh`
- `per_service_carbon_kgco2eq[svc] = acc.operational_gco2 / 1000.0`
- `per_service_region[svc] = acc.region` (ou sentinel `"unknown"` si vide)
- `energy_kwh = sum(per_service_energy_kwh.values())`
- `energy_model = select_co2_model_tag(window_flags)` si l'énergie est positive, chaîne vide sinon

La map per-service est indexée par nom de service (lowercased en amont par `CarbonContext.service_regions`). Le champ `region` de l'accumulateur est aussi lowercased avant stockage, pour s'aligner avec les clés de `per_region` et que les deux maps se collationnent. Une énergie nulle donne une chaîne `energy_model` vide, ce qui route la fenêtre vers le chemin fallback proxy de l'aggregator.

## Attribution de la région

La région enregistrée pour un service est celle du *premier* span observé pour ce service dans la fenêtre. Les spans suivants pour le même service conservent cette région même s'ils portent un attribut `cloud_region` différent. Deux conséquences :

- Un service déployé dans deux régions à l'intérieur de la même fenêtre de scoring est entièrement attribué à sa première région observée. La ligne par région dans `GreenSummary.regions` reflète quand même le split, donc les chiffres globaux restent corrects.
- Les services long-running avec une configuration `service_regions` stable ne sont pas affectés : chaque span résout vers la même région.

Ce compromis garde la map per-service simple. Une map plus granulaire `BTreeMap<(String, String), ServiceCarbonAccumulator>` indexée par `(service, region)` exposerait les splits multi-régions mais grossirait le payload wire et forcerait les consommateurs à fold les lignes eux-mêmes. La v1.0 préfère la forme simple.

## Précédence du tag de modèle

Le tag `energy_model` par fenêtre réutilise `select_co2_model_tag` existant dans `score::region_breakdown`, qui implémente déjà la précédence canonique :

```
electricity_maps_api > scaphandre_rapl > cloud_specpower > io_proxy_v3 > io_proxy_v2 > io_proxy_v1
```

avec le suffixe `+cal` optionnel quand les données de calibration sont actives. Le tag reflète le modèle de plus haute fidélité présent dans la fenêtre. Aucune répartition par service des tags n'est exposée : un tag global transparent est plus utile qu'une map par service que les consommateurs devraient fold de toute façon.

## L'embarqué reste au niveau global

Le terme SCI `M` ne vit que dans `co2.total` et `aggregate.total_carbon_kgco2eq`. Les maps per-service ne portent que le terme opérationnel. Raisons :

- L'amortissement embarqué par requête est déjà une répartition arbitraire. Le découper par service exposerait une précision qui n'existe pas dans les données sources.
- L'embarqué n'est pas actionnable par l'optimisation logicielle. Supprimer des N+1 ne change pas `M`.
- Les consommateurs (auditeurs, dashboards publics) qui veulent le chiffre opérationnel par service bénéficient d'une valeur propre qui mappe directement vers des actions d'optimisation.

L'invariant `sum(per_service_carbon_kgco2eq) × 1000 ≈ co2.operational_gco2` (tolérance 1e-6) est testé.

## Branchement de l'aggregator

`report::periodic::aggregator::Builder::process_window` regarde deux prédicats :

1. `report.green_summary.per_service_carbon_kgco2eq.is_empty() && report.green_summary.per_service_energy_kwh.is_empty()` — maps runtime absentes.
2. `report.green_summary.energy_kwh > 0.0` — total énergie runtime présent.

Quand les deux maps runtime sont non vides, l'aggregator somme directement les valeurs per-service. Quand elles sont vides, il tombe sur le chemin proxy hérité de la première release (part d'I/O proportionnelle pour le carbone, `total_io_ops × ENERGY_PER_IO_OP_KWH` pour l'énergie). Les deux chemins coexistent dans un même répertoire d'archive : chaque fenêtre applique sa propre stratégie.

Un unique `tracing::warn!` par fichier d'archive signale l'usage du fallback pour que les opérateurs repèrent des archives anciennes. Les compteurs `runtime_windows` et `fallback_windows` sur `AggregateInputs` portent le split pour les diagnostics aval.

## Hardening à la frontière d'archive

Les lignes d'archive sont de l'état opérateur sur disque. L'aggregator traite chaque champ f64 lu depuis une archive comme non sûr :

- `energy_kwh`, `per_service_energy_kwh.values()` et `per_service_carbon_kgco2eq.values()` passent par `sanitize_f64` qui ramène `NaN`, `+/-Inf` et les valeurs négatives à `0.0`. Sans ce garde, une seule ligne empoisonnée propagerait `NaN` à toutes les sommes aval.
- La map `per_service` est cappée à `MAX_SERVICES = 4096` entrées. Une fois le cap atteint, les services additionnels distincts venant de l'archive sont silencieusement abandonnés. Les findings déjà routés vers un bucket connu continuent à accumuler.
- `energy_source_models` est cappé à `MAX_ENERGY_MODELS = 64` entrées et chaque chaîne `energy_model` est rejetée si plus longue que 64 octets. Les tags qui ne diffèrent que par le suffixe `+cal` fusionnent vers une seule entrée nue, donc le set ne porte jamais à la fois `scaphandre_rapl` et `scaphandre_rapl+cal`.

Ces caps reflètent le cap `MAX_REGIONS` côté runtime dans `score::carbon_compute`. Ils sont silencieux (pas d'erreur), l'aggregator les traite comme un folding best-effort.

## Compatibilité ascendante

Les cinq nouveaux champs `GreenSummary` portent tous `#[serde(default)]`. Une ligne d'archive écrite avant la livraison de cette fonctionnalité désérialise avec `energy_kwh = 0.0`, `energy_model = ""` et des maps vides. L'aggregator détecte ce cas et tombe sur le proxy.

Pas de bump de version de schéma. `perf-sentinel-report/v1.0` reste l'identifiant wire. Les consommateurs qui lisent uniquement l'ensemble v1.0 documenté continuent à fonctionner, ceux qui opt-in dans les nouveaux champs gagnent automatiquement les valeurs runtime-calibrated.

## Ce qu'on n'a pas fait

- Tag de modèle énergétique par service (`per_service_energy_model: BTreeMap<String, String>`). Possible mais inutile aujourd'hui : le tag par fenêtre porte assez de fidélité pour l'audit trail.
- Splits multi-régions par service. La forme wire reste simple au prix d'une attribution approximative pour les services qui changent de région en cours de fenêtre.
- Attribution de l'embarqué par service. Exclu volontairement.
- Bump de version de schéma. Le changement est strictement additif.
