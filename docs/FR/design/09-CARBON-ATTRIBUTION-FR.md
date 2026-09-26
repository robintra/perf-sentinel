# Attribution carbone par service

Notes de design pour l'attribution par service, calibrée au runtime, de l'énergie et du carbone, exposée dans `GreenSummary` et consommée par l'agrégateur du rapport périodique. À lire avec `docs/FR/METHODOLOGY-FR.md` (côté opérateur) et `docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md` (agrégateur + schéma).

## Pourquoi

La première version du rapport recalculait `aggregate.total_energy_kwh` via un proxy au moment de l'agrégation, même quand le daemon sous-jacent avait mesuré l'énergie via Scaphandre ou cloud SPECpower. Elle distribuait aussi le CO2 par fenêtre aux services proportionnellement à la part d'I/O ops, ignorant que deux services dans des régions différentes émettent à des intensités très différentes.

La correction consiste à calculer et sérialiser l'énergie + le carbone par service au moment du scoring, pour que l'agrégateur puisse sommer directement. Les valeurs par service sont calibrées au runtime de bout en bout : le daemon voit la vraie région de chaque service et le vrai tag de backend énergétique.

## Algorithme

Le scoring tourne dans `score::compute_carbon_report`. La fonction boucle déjà une fois sur tous les spans du batch et accumule du carbone par région dans `RegionAccumulator`. L'attribution par service ajoute en parallèle une `BTreeMap<String, ServiceCarbonAccumulator>` qui suit la même forme en une seule passe.

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
- `per_service_region[svc] = acc.region` (ou la sentinelle `"unknown"` si vide)
- `energy_kwh = sum(per_service_energy_kwh.values())`
- `energy_model = select_co2_model_tag(window_flags)` si l'énergie est positive, chaîne vide sinon

La map par service est indexée par nom de service (mis en minuscules en amont par `CarbonContext.service_regions`). Le champ `region` de l'accumulateur est aussi mis en minuscules avant stockage, pour s'aligner sur les clés de `per_region` et pour que les deux maps se recoupent. Une énergie nulle donne une chaîne `energy_model` vide, ce qui route la fenêtre vers le chemin de repli proxy de l'agrégateur.

## Attribution de la région

La région enregistrée pour un service est celle du *premier* span observé pour ce service dans la fenêtre. Les spans suivants pour le même service conservent cette région même s'ils portent un attribut `cloud_region` différent. Deux conséquences :

- Un service déployé dans deux régions à l'intérieur de la même fenêtre de scoring est entièrement attribué à sa première région observée. La ligne par région dans `GreenSummary.regions` reflète quand même la répartition, donc les chiffres globaux restent corrects.
- Les services qui tournent en continu avec une configuration `service_regions` stable ne sont pas affectés : chaque span résout vers la même région.

Ce compromis garde la map par service simple. Une map plus granulaire `BTreeMap<(String, String), ServiceCarbonAccumulator>` indexée par `(service, region)` exposerait les répartitions multi-régions mais grossirait le payload sur le fil et forcerait les consommateurs à agréger les lignes eux-mêmes. La v1.0 préfère la forme simple.

## Précédence du tag de modèle

Le tag `energy_model` par fenêtre réutilise `select_co2_model_tag` existant dans `score::region_breakdown`, qui implémente déjà la précédence canonique :

```
electricity_maps_api > alumet_rapl > scaphandre_rapl > kepler_ebpf > redfish_bmc > cloud_specpower > io_proxy_v3 > io_proxy_v2 > io_proxy_v1
```

avec le suffixe `+cal` optionnel quand les données de calibration sont actives. Le tag reflète le modèle de plus haute fidélité présent dans la fenêtre. Aucune répartition par service des tags n'est exposée : un tag global transparent est plus utile qu'une map par service que les consommateurs devraient agréger de toute façon.

## L'embarqué reste au niveau global

Le terme SCI `M` ne vit que dans `co2.total` et `aggregate.total_carbon_kgco2eq`. Les maps par service ne portent que le terme opérationnel. Raisons :

- L'amortissement embarqué par requête est déjà une répartition arbitraire. Le découper par service exposerait une précision qui n'existe pas dans les données sources.
- L'embarqué n'est pas actionnable par l'optimisation logicielle. Supprimer des N+1 ne change pas `M`.
- Les consommateurs (auditeurs, dashboards publics) qui veulent le chiffre opérationnel par service bénéficient d'une valeur propre qui correspond directement à des actions d'optimisation.

L'invariant `sum(per_service_carbon_kgco2eq) × 1000 ≈ co2.operational_gco2` (tolérance 1e-6) est testé.

## Branchement de l'aggregator

`report::periodic::aggregator::Builder::process_window` regarde deux prédicats :

1. `report.green_summary.per_service_carbon_kgco2eq.is_empty() && report.green_summary.per_service_energy_kwh.is_empty()` : maps runtime absentes.
2. `report.green_summary.energy_kwh > 0.0` : total énergie runtime présent.

Quand les deux maps runtime sont non vides, l'agrégateur somme directement les valeurs par service. Quand elles sont vides, il se rabat sur le chemin proxy hérité de la première version (part d'I/O proportionnelle pour le carbone, `total_io_ops × ENERGY_PER_IO_OP_KWH` pour l'énergie). Les deux chemins peuvent coexister dans un même répertoire d'archive : chaque fenêtre applique sa propre stratégie.

Un unique `tracing::warn!` par fichier d'archive signale l'usage du repli pour que les opérateurs repèrent des archives anciennes. Les compteurs `runtime_windows` et `fallback_windows` sur `AggregateInputs` portent la répartition pour les diagnostics en aval.

## Hardening à la frontière d'archive

Les lignes d'archive sont un état sur disque contrôlé par l'opérateur. L'agrégateur traite chaque champ f64 lu depuis une archive comme non sûr :

- `energy_kwh`, `per_service_energy_kwh.values()` et `per_service_carbon_kgco2eq.values()` passent par `sanitize_f64` qui ramène `NaN`, `+/-Inf` et les valeurs négatives à `0.0`. Sans ce garde-fou, une seule ligne empoisonnée propagerait `NaN` à toutes les sommes aval.
- La map `per_service` est plafonnée à `MAX_SERVICES = 4096` entrées. Une fois le plafond atteint, les services distincts supplémentaires venant de l'archive sont silencieusement abandonnés. Les findings déjà routés vers un bucket connu continuent à accumuler.
- `energy_source_models` est plafonné à `MAX_ENERGY_MODELS = 64` entrées et chaque chaîne `energy_model` est rejetée si plus longue que 64 octets. Les tags qui ne diffèrent que par le suffixe `+cal` fusionnent vers une seule entrée nue, donc l'ensemble ne porte jamais à la fois `scaphandre_rapl` et `scaphandre_rapl+cal`.

Ces plafonds reflètent le plafond `MAX_REGIONS` côté runtime dans `score::carbon_compute`. Ils sont silencieux (pas d'erreur). L'agrégateur les traite comme une agrégation au mieux.

## Compatibilité ascendante

Les sept nouveaux champs d'attribution `GreenSummary` portent tous `#[serde(default)]` : `energy_kwh` et `energy_model` au niveau fenêtre, plus les maps par service `per_service_carbon_kgco2eq`, `per_service_energy_kwh`, `per_service_region`, `per_service_energy_model` et `per_service_measured_ratio`. Une ligne d'archive écrite sans attribution énergétique runtime désérialise avec `energy_kwh = 0.0`, `energy_model = ""` et des maps vides. L'agrégateur détecte ce cas et se rabat sur le proxy.

Ce changement n'a pas incrémenté à lui seul la version de schéma. Les champs ajoutés sont des extensions `#[serde(default)]`, donc les consommateurs qui lisent uniquement l'ensemble documenté de base continuent à fonctionner, et ceux qui adoptent les nouveaux champs obtiennent automatiquement les valeurs calibrées au runtime.

## Ce qu'on n'a pas fait

- Répartitions multi-régions par service. La forme sur le fil reste simple au prix d'une attribution approximative pour les services qui changent de région en cours de fenêtre.
- Attribution de l'embarqué par service. Voir § "L'embarqué reste au niveau global".
- Incrémenter la version de schéma pour ce seul changement. Les champs ajoutés sont strictement additifs (le schéma a atteint la v1.3 plus tard via d'autres révisions additives).
