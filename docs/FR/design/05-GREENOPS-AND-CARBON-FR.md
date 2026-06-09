# Scoring GreenOps et conversion carbone

## Score d'intensité I/O (IIS)

La métrique centrale est le Score d'Intensité I/O (I/O Intensity Score) : le nombre d'opérations I/O générées par requête utilisateur pour un endpoint donné.

```
IIS(endpoint) = total_io_ops(endpoint) / invocation_count(endpoint)
```

Un endpoint appelé à travers 3 traces avec 18 opérations I/O au total a `IIS = 18 / 3 = 6.0`. Cela normalise les différents volumes de trafic : un endpoint à fort trafic avec 1000 invocations et 6000 opérations I/O a le même IIS (6.0) qu'un endpoint à faible trafic avec 3 invocations et 18 opérations.

Le dénominateur utilise `.max(1)` comme garde contre la division par zéro, bien que ce cas ne puisse pas se produire en pratique (un endpoint qui apparaît dans `endpoint_stats` a forcément été vu dans au moins une trace).

## Algorithme de scoring : cinq étapes

### Étape 1 : statistiques par endpoint

```rust
for (trace_idx, trace) in traces.iter().enumerate() {
    for span in &trace.spans {
        total_io_ops += 1;
        let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats {
            total_io_ops: 0,
            invocation_count: 0,
            last_seen_trace: usize::MAX,
        });
        stats.total_io_ops += 1;
        if stats.last_seen_trace != trace_idx {
            stats.invocation_count += 1;
            stats.last_seen_trace = trace_idx;
        }
    }
}
```

**Passe unique avec sentinelle par trace :** `invocation_count` est incrémenté la première fois qu'une paire `(service, endpoint)` est vue dans une trace donnée, puis `last_seen_trace` est positionné pour bloquer toute ré-incrémentation sur la même trace. Initialiser la sentinelle à `usize::MAX` (et non `0`) garde l'index de trace `0` valide comme marqueur de "première rencontre". Cela évite une seconde passe `get_mut` sur un `HashSet` par trace (une sonde `HashMap` de moins par paire `(trace, endpoint)`).

**`EndpointStats<'a>` avec `service` emprunté :** le champ `service` emprunte `&'a str` depuis les événements span au lieu de cloner le String. Le clone ne se produit que plus tard lors de la construction des structs `TopOffender` pour la sortie. Cela évite un clone de String par endpoint unique dans la boucle interne.

**Structure sous-jacente (`HashMap + sort` vs `BTreeMap`) :** la map par endpoint est un `HashMap` finalisé par un unique `sort_by` pour la vue publique, et non un `BTreeMap`. Sous le régime d'accès de perf-sentinel (beaucoup de spans par endpoint unique, K petit devant N), les mesures sur 1M de spans donnent systématiquement l'avantage à `HashMap + sort` :

| Cardinalité endpoints | Spans | `HashMap + sort` | `BTreeMap` | Ratio |
|----------------------:|------:|-----------------:|-----------:|------:|
|                    16 |    1M |            15 ms |      19 ms | 1,24x |
|                    64 |    1M |            16 ms |      31 ms | 1,94x |
|                   256 |    1M |            17 ms |      49 ms | 2,89x |
|                  1024 |    1M |            18 ms |      73 ms | 3,99x |

Le tri gratuit à l'itération du `BTreeMap` est noyé par son surcoût `O(log K)` par insertion. Le tri terminal est `O(K log K)` sur K petit (20-90 µs sur toute la plage), négligeable à côté du volume d'insertions.

### Étape 2 : dédup des I/O évitables

```rust
let mut dedup: HashMap<(&str, &str, &str), usize> = HashMap::with_capacity(findings.len());
for f in &findings {
    if matches!(f.finding_type, FindingType::SlowSql | FindingType::SlowHttp) {
        continue; // les findings lents ne sont pas évitables
    }
    let avoidable = f.pattern.occurrences.saturating_sub(1);
    let entry = dedup.entry((&f.trace_id, &f.pattern.template, &f.source_endpoint)).or_insert(0);
    *entry = (*entry).max(avoidable);
}
```

**Pourquoi inclure `source_endpoint` dans la clé ?** Le même template SQL (ex. `SELECT * FROM config WHERE key = ?`) peut être appelé depuis deux endpoints différents dans la même trace. Les opérations évitables de chaque endpoint doivent être comptées indépendamment. Sans `source_endpoint`, `max(5, 3) = 5` sous-compterait : le total correct est `5 + 3 = 8`.

**Pourquoi `max()` au lieu de `sum()` ?** Au sein du même (trace, template, endpoint), les détecteurs N+1 et redondant peuvent tous deux se déclencher sur des ensembles de spans qui se chevauchent. Prendre le max empêche le double comptage : si N+1 rapporte 9 évitables et redondant rapporte 4 évitables pour le même groupe, le vrai compteur d'évitables est 9 (l'ensemble le plus grand inclut déjà le plus petit).

**Findings lents exclus :** les requêtes lentes sont des opérations nécessaires qui se trouvent être lentes. Elles ont besoin d'optimisation (indexation, cache), pas d'élimination. Les inclure dans le ratio de gaspillage confondrait "I/O gaspillées" avec "I/O lentes".

### Étape 3 : calcul de l'IIS par endpoint

```rust
let iis_map: HashMap<&str, f64> = endpoint_stats.iter()
    .map(|(&ep, stats)| {
        let invocations = stats.invocation_count.max(1) as f64;
        (ep, stats.total_io_ops as f64 / invocations)
    })
    .collect();
```

La map IIS est calculée une seule fois et réutilisée pour l'enrichissement des findings (étape 4) et le classement des top offenders (étape 5).

### Étape 4 : enrichir les findings

Chaque finding reçoit un `GreenImpact` :

```rust
GreenImpact {
    estimated_extra_io_ops: if slow { 0 } else { occurrences - 1 },
    io_intensity_score: iis,
}
```

### Étape 5 : top offenders

Triés par IIS décroissant, avec un ordre alphabétique en cas d'égalité pour une sortie déterministe :

```rust
top_offenders.sort_by(|a, b| {
    b.io_intensity_score.partial_cmp(&a.io_intensity_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.endpoint.cmp(&b.endpoint))
});
```

`partial_cmp` avec `unwrap_or(Equal)` gère `NaN` de manière sûre, bien que NaN ne puisse pas se produire puisque le dénominateur est toujours >= 1.0.

## Ratio de gaspillage I/O

```
ratio_gaspillage = avoidable_io_ops / total_io_ops
```

Quand `total_io_ops == 0`, le ratio est `0.0` (pas NaN). C'est la fraction d'opérations I/O qui pourraient être éliminées en corrigeant les anti-patterns détectés. Cela s'aligne avec le composant **Énergie** du [modèle SCI (ISO/IEC 21031:2024)](https://sci-guide.greensoftware.foundation/) de la [Green Software Foundation](https://greensoftware.foundation/) : réduire les calculs inutiles réduit la consommation d'énergie.

## Conversion carbone

Le pipeline de scoring résout deux dimensions indépendantes pour chaque span : **l'énergie par opération** (`E`) et **l'intensité du réseau électrique** (`I`). Chacune a sa propre chaîne de repli, de la source la plus précise jusqu'aux valeurs embarquées par défaut.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/carbon-scoring_dark.svg">
  <img alt="Résolution de l'énergie et de l'intensité dans le scoring carbone" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/carbon-scoring.svg">
</picture>

### Alignement SCI v1.0

perf-sentinel implémente la spécification [Software Carbon Intensity v1.0](https://sci-guide.greensoftware.foundation/) (devenue [ISO/IEC 21031:2024](https://www.iso.org/standard/86612.html)) de la Green Software Foundation. La formule est :

```
SCI = ((E × I) + M) per R
```

Où :
- **`E`** = énergie consommée par la charge de travail (kWh)
- **`I`** = intensité carbone géographique du réseau (gCO₂eq/kWh)
- **`M`** = émissions embodiées de fabrication matérielle, amorties
- **`R`** = unité fonctionnelle (le dénominateur "par X")

Dans perf-sentinel :
- **`R = 1 trace`** : une requête utilisateur. Chaque trace corrélée est une unité fonctionnelle.
- **`E = io_ops × ENERGY_PER_IO_OP_KWH`** : proxy à partir du compteur d'ops I/O.
- **`I = lookup_region(region).intensity`** : depuis la table carbone embarquée.
- **`M = traces.len() × embodied_per_request_gco2`** : configurable, défaut 0,001 g/req.

### Constante énergétique

```rust
pub const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1; // 0,1 uWh par opération I/O
```

C'est une approximation d'ordre de grandeur, pas une valeur mesurée. Elle tient compte d'une requête de base de données ou d'un aller-retour HTTP typique sur une infrastructure cloud. Le [projet Cloud Carbon Footprint](https://www.cloudcarbonfootprint.org/docs/methodology/) utilise une approche similaire d'estimation de l'énergie à partir de l'utilisation des ressources plutôt que d'une mesure directe.

La valeur doit être divulguée comme méthodologie selon les exigences SCI. Elle est documentée dans le code, dans [LIMITATIONS-FR.md](../LIMITATIONS-FR.md) et ici.

### Carbone embodié (terme `M`)

```rust
pub const DEFAULT_EMBODIED_CARBON_PER_REQUEST_GCO2: f64 = 0.001;
```

Le défaut de `0,001 gCO₂/requête` est dérivé d'hypothèses typiques sur le cycle de vie d'un serveur :

- Un serveur x86 moderne a une empreinte carbone embodiée de **~1000 kgCO₂eq** sur un cycle de vie de 4 ans (sources : [API Boavizta](https://doc.api.boavizta.org/) lifecycle assessments, [méthodologie Cloud Carbon Footprint](https://www.cloudcarbonfootprint.org/docs/methodology/)).
- 4 ans × 365 jours × 86400 secondes × 1 requête/sec ≈ 126 millions de requêtes amorties par serveur.
- 1000 g par serveur / 126e6 requêtes ≈ **0,000008 gCO₂/req** (8e-6 g) à 1 req/sec, montant à ~0,001 à des taux de requêtes plus bas ou pour du matériel moins amorti.

Le défaut `0,001 g/req` est une **borne supérieure conservatrice pour des serveurs microservices peu chargés**. La méthodologie AWS Customer Carbon Footprint (2025) rapporte ~320 kgCO2eq/an pour un Dell R640, ce qui à des taux d'utilisation typiques donne 10-50 ugCO2/req, soit 10-20x en dessous de notre défaut. Les utilisateurs avec des données d'infrastructure mesurées devraient abaisser cette valeur via `[green] embodied_carbon_per_request_gco2`.

**L'embodié est indépendant de la région.** Les émissions de fabrication matérielle ne varient pas selon le lieu de déploiement. perf-sentinel émet le carbone embodié inconditionnellement quand le scoring vert est activé, même quand aucune région ne se résout, pour que les utilisateurs voient au moins une estimation plancher.

### Formule de conversion

Pour chaque bucket de région :
```
operational_region = io_ops_in_region × ENERGY_PER_IO_OP_KWH × carbon_intensity × PUE
```

Total opérationnel sur toutes les régions :
```
operational_gco2 = Σ operational_region
```

Embodié :
```
embodied_gco2 = traces.len() × embodied_per_request_gco2
```

Mid-point CO₂ total :
```
total.mid = operational_gco2 + embodied_gco2
```

CO₂ évitable (via ratio, voir "Évitable via ratio" ci-dessous) :
```
accounted_io_ops = total_io_ops - unknown_ops
avoidable.mid = operational_gco2 × (avoidable_io_ops / accounted_io_ops)
```

Le dénominateur `accounted_io_ops` exclut le bucket synthétique `unknown` pour que le ratio soit cohérent avec `operational_gco2` (qui l'exclut aussi). Numérateur et dénominateur sur la même base comptable.

Intervalle d'incertitude (multiplicatif 2×, pas arithmétique ±50%) :
```
total.low  = total.mid × 0,5    // mid divisé par 2
total.high = total.mid × 2,0    // mid multiplié par 2
(idem pour avoidable.low / avoidable.high)
```

C'est un **intervalle log-symétrique** : la moyenne géométrique de `low` et `high` vaut `mid`. Le cadrage 2× correspond mieux à l'incertitude d'ordre de grandeur du modèle proxy I/O qu'une fenêtre symétrique ±50%. Voir "Cadrage de l'incertitude" ci-dessous.

### Sémantique SCI v1.0 : numérateur vs intensité

La spécification SCI v1.0 définit `SCI = ((E × I) + M) / R`, une **intensité** exprimée par unité fonctionnelle R. perf-sentinel rapporte le **numérateur** de cette formule, sommé sur toutes les traces analysées :

```
co2.total.mid = Σ operational_gco2 + embodied_gco2
              = (E × I) + M   (sommé sur les traces analysées)
```

C'est une **empreinte** (gCO₂eq absolus), pas un score d'intensité. Les consommateurs qui veulent l'intensité SCI par requête la calculent en aval :

```
sci_par_trace = co2.total.mid / analysis.traces_analyzed
```

Pour taguer cette distinction sémantique au niveau des données, `CarbonEstimate` porte un champ `methodology` avec deux valeurs possibles :

- `"sci_v1_numerator"` : utilisé sur `co2.total`. L'empreinte `(E × I) + M` sommée sur les traces.
- `"sci_v1_operational_ratio"` : utilisé sur `co2.avoidable`. Le ratio global aveugle à la région `operational × (avoidable/accounted)`, excluant le carbone embodié.

Les deux valeurs distinctes signalent aux consommateurs en aval que `total` et `avoidable` sont calculés différemment et ne doivent pas être comparés comme s'ils étaient des quantités homogènes.

### Évitable via ratio (choix de design)

Calculer le CO₂ évitable de manière précise par région nécessiterait de propager la résolution de région à travers la phase de dédup des findings (qui agrège actuellement les ops I/O évitables globalement par `(trace_id, template, source_endpoint)`). C'est complexe et sujet aux erreurs.

À la place, perf-sentinel calcule :

```
avoidable.mid = operational_gco2 × (avoidable_io_ops / accounted_io_ops)
```

Cela préserve l'**échelle relative** (une réduction de 50% du gaspillage donne une chute de 50% du CO₂ évitable) sans nécessiter d'attribution par finding. Le compromis : quand les ops évitables sont concentrées dans une région à haute intensité, ce ratio sous-attribue légèrement les économies. La simplification est documentée comme limitation connue et taguée au niveau des données via `methodology: "sci_v1_operational_ratio"`.

**Le carbone embodié est exclu de l'évitable.** Vous ne pouvez pas optimiser le silicium fabriqué en corrigeant des requêtes N+1 : les émissions embodiées sont fixes par requête peu importe l'efficacité de l'application. L'estimation évitable ne considère que le terme opérationnel.

### Résolution multi-région

Chaque span résout vers une région effective via une chaîne à 3 niveaux (premier match gagne) :

1. **`event.cloud_region`** : extrait de l'attribut de ressource OTel `cloud.region` (avec fallback sur attribut de span pour les SDKs qui le mettent sur les spans individuels). Le plus autoritatif. Les valeurs sont assainies à la frontière d'ingestion : les chaînes invalides (non-ASCII-alphanumérique-tiret-underscore, longueur > 64 ou vides) sont silencieusement écartées.
2. **`[green.service_regions][event.service.to_lowercase()]`** : surcharge config pour les environnements où OTel ne le fournit pas (ex. ingestion Jaeger / Zipkin). Insensible à la casse (le loader de config met les clés en minuscules).
3. **`[green] default_region`** : fallback global.

Les spans sans région résolvable atterrissent dans un bucket synthétique `"unknown"` : zéro contribution au CO₂ opérationnel. Le breakdown `regions[]` montre tout de même le bucket pour que les utilisateurs voient les ops I/O orphelines (signal visible pour le troubleshooting ; les messages `tracing::debug!` détaillés sont disponibles via `RUST_LOG=debug`).

**Plafond de cardinalité des régions.** Le BTreeMap par région est plafonné à 256 régions distinctes en une passe de scoring (constante `MAX_REGIONS`). Les chaînes de région excédentaires tombent dans le bucket `unknown`, empêchant l'épuisement mémoire depuis des attributs OTLP `cloud.region` contrôlés par un attaquant ou mal configurés.

**Scalaire CO₂ des TopOffender en mode multi-région.** Quand le scoring multi-région est actif (soit `[green.service_regions]` est non-vide, soit un span porte `cloud.region`), le scalaire `top_offenders[].co2_grams` est mis à `None` pour tous. Le calculer depuis `default_region` uniquement serait incohérent avec le breakdown par région ; les utilisateurs doivent se fier à `green_summary.regions[]` pour l'attribution par région dans les déploiements multi-région.

### Cadrage de l'incertitude : multiplicatif 2×, pas ±50%

Chaque estimation CO₂ est rapportée comme `{ low, mid, high }` :

```rust
pub struct CarbonEstimate {
    pub low: f64,           // mid × 0,5
    pub mid: f64,           // meilleure estimation
    pub high: f64,          // mid × 2,0
    pub model: &'static str,       // "io_proxy_v1"
    pub methodology: &'static str, // "sci_v1_numerator" ou "sci_v1_operational_ratio"
}
```

Les facteurs `0,5` et `2,0` encodent un **intervalle d'incertitude multiplicative 2×** autour du midpoint :

```
moyenne_géométrique(low, high) = sqrt(low × high) = sqrt(mid² × 0,5 × 2,0) = mid
```

C'est un **intervalle log-symétrique** : le mid est le centre géométrique, pas le centre arithmétique. L'écart entre `low` et `high` est un facteur 4 (high/low = 4), plus large qu'une fenêtre symétrique ±50% (qui donnerait high/low = 3).

**Pourquoi 2× et pas ±50% ?** Le modèle proxy I/O a une incertitude d'ordre de grandeur à chaque étape :
- `ENERGY_PER_IO_OP_KWH = 0,1 µWh/op` est une approximation d'ordre de grandeur.
- Les valeurs d'intensité réseau de CCF/Electricity Maps sont des moyennes annuelles ; l'intensité en temps réel varie 2-3× sur une journée.
- Les PUE sont des moyennes par fournisseur ; les datacenters individuels varient.
- Le carbone embodié suppose une valeur conservatrice de cycle de vie serveur qui peut être décalée d'un ordre de grandeur pour du matériel spécifique.

Une fenêtre symétrique ±50% (high = 1,5 × mid) sous-estimerait cette incertitude réelle. Le cadrage multiplicatif 2× est délibérément choisi pour être honnête : la valeur réelle est dans un facteur 2 de `mid`, dans un sens ou l'autre.

Les bornes reflètent l'incertitude agrégée du modèle, **pas** la variance par endpoint. Le modèle n'a pas assez de résolution pour distinguer la précision par endpoint.

### Versionnement du modèle

Le champ `model: "io_proxy_v1"` versionne la méthodologie d'estimation. Les améliorations futures (pondération par opération, profils horaires de carbone, intégration RAPL) bumperont cette version, permettant aux consommateurs en aval de tracer quelle méthodologie a produit un rapport donné.

### Recherche par région

La table d'intensité carbone est embarquée comme tableau statique et convertie en `HashMap` via `LazyLock` :

```rust
static REGION_MAP: LazyLock<HashMap<&'static str, (f64, Provider)>> =
    LazyLock::new(|| CARBON_TABLE.iter().map(...).collect());
```

**Pourquoi `LazyLock<HashMap>` au lieu d'un scan linéaire ?** L'implémentation originale parcourait les 41 entrées à chaque appel. Avec le HashMap, la recherche est O(1). Le coût d'initialisation est payé une seule fois au premier accès.

**Recherche insensible à la casse :** la fonction publique `lookup_region()` convertit l'entrée en minuscules via `to_ascii_lowercase()` avant la recherche. Toutes les clés de la table sont stockées en minuscules. L'étape de scoring multi-région utilise un `BTreeMap<String, usize>` (pas `HashMap`) pour répartir les ops I/O par région résolue. Cela garantit un ordre d'itération déterministe et des sommes flottantes stables entre exécutions.

### Valeurs PUE

| Fournisseur | PUE   | Source                                                                                                                                                    |
|-------------|-------|-----------------------------------------------------------------------------------------------------------------------------------------------------------|
| AWS         | 1,15  | [AWS Cloud sustainability](https://sustainability.aboutamazon.com/products-services/aws-cloud) (flotte mondiale 2024)                                     |
| GCP         | 1,09  | [Google data centers efficiency](https://datacenters.google/efficiency/) (moyenne annuelle flotte 2024)                                                   |
| Azure       | 1,17  | [Microsoft datacenter efficiency](https://datacenters.microsoft.com/sustainability/efficiency/) (FY25, juillet 2024 à juin 2025, owned-and-controlled)    |
| Générique   | 1,2   | [Uptime Institute Global Survey 2023](https://uptimeinstitute.com/resources/research-and-reports/uptime-institute-global-data-center-survey-results-2023) (les éditions 2024 et 2025 montrent un plateau similaire dans la fourchette 1,5 à 1,6 de moyenne industrie) |

Le PUE (Power Usage Effectiveness) mesure le ratio entre l'énergie totale du datacenter et l'énergie de l'équipement IT. Un PUE de 1,15 signifie 15% de surcoût pour le refroidissement, l'éclairage et l'infrastructure. La moyenne de l'industrie est ~1,58 (Uptime Institute), et les fournisseurs cloud hyperscale atteignent des valeurs significativement plus basses, le 1,09 de GCP passant sous le plancher symbolique des 10% de surcoût.

### Données d'intensité carbone

Les intensités carbone régionales du réseau électrique (gCO2eq/kWh) proviennent des moyennes annuelles [Electricity Maps](https://www.electricitymaps.com/) (2023-2024) et du projet [Cloud Carbon Footprint](https://www.cloudcarbonfootprint.org/). La table couvre 15 régions AWS, 8 régions GCP, 6 régions Azure et 14 codes pays ISO.

Quand la région configurée n'est pas trouvée dans la table, les champs CO2 sont omis du rapport (aucune valeur par défaut n'est inventée).

## Profils horaires d'intensité carbone

La valeur annuelle plate par région écarte la variance diurne qui peut être importante dans les réseaux avec une forte part de renouvelables variables ou de forts pics de demande. Pour capturer cette variance, perf-sentinel embarque un profil UTC 24 valeurs par région pour quatre régions avec des formes diurnes bien documentées :

- **France (`eu-west-3`)** : baseload nucléaire, forme plate-avec-pic-soir.
- **Allemagne (`eu-central-1`)** : charbon + gaz + renouvelables variables, pics matin/soir prononcés.
- **Royaume-Uni (`eu-west-2`)** : éolien + gaz, pics jumeaux modérés.
- **US-East (`us-east-1`)** : gaz + charbon, plateau diurne 13h-18h UTC (9h-14h heure Est).

La moyenne arithmétique de chaque profil approxime la valeur annuelle plate correspondante dans les ±5%, préservant la continuité méthodologique. Le profil Allemagne (`eu-central-1`) violait historiquement cet invariant (moyenne ~431 gCO₂/kWh, figée au niveau de la crise charbon 2022, contre 338 en annuel) : depuis 0.8.7 il est recalibré sur le niveau Electricity Maps 2024 (~341) et l'invariant tient pour toutes les régions sans exception. Les utilisateurs peuvent désactiver les profils horaires avec `use_hourly_profiles = false`.

Sources : rapports open-data annuels Electricity Maps (2023-2024), ENTSO-E Transparency Platform, RTE eco2mix (France), Fraunhofer ISE Energy-Charts (Allemagne), NGESO carbonintensity.org.uk (Royaume-Uni), EIA hourly generation data (US-East).

La table n'embarque intentionnellement **pas** de profils mensuels (24x12). Le gain de précision saisonnier est marginal par rapport au coût en complexité. Le tag `IntensitySource` distingue déjà annuel vs horaire, ce qui rend l'extension future rétrocompatible.

Le chemin de scoring parcourt chaque span une fois et dispatche entre trois sources d'intensité :

```rust
let intensity_used = if ctx.use_hourly_profiles
    && hourly_profile_for_region_lower(region).is_some()
    && let Some(hour) = time::parse_utc_hour(&span.event.timestamp)
{
    lookup_hourly_intensity_lower(region, hour).unwrap_or(annual_intensity)
} else {
    annual_intensity
};
```

Quand le dispatch sélectionne le chemin horaire pour une région, la ligne `RegionBreakdown` est taguée `intensity_source: "hourly"` et le `CarbonEstimate.model` de niveau supérieur passe de `"io_proxy_v1"` à `"io_proxy_v2"`. Si le même rapport contient des régions passées par le chemin plat, ces régions restent taguées `intensity_source: "annual"` tandis que le modèle de niveau supérieur lit toujours `"io_proxy_v2"`. Le tag enregistre "le modèle le plus précis utilisé quelque part dans le run".

**Auto-cohérence des lignes de breakdown.** L'identité `co2_gco2 ≈ io_ops × grid_intensity_gco2_kwh × pue × ENERGY_PER_IO_OP_KWH` ne tient que dans le cas proxy (pas de snapshot Scaphandre/cloud). Quand de l'énergie mesurée est présente et que des services dans la même région utilisent des coefficients différents, l'intensité affichée reste la moyenne pondérée mais l'identité devient approximative.

**Les timestamps doivent être en UTC.** `parse_utc_hour` rejette les formes d'offset non-UTC (`+02:00`, `-05:00`) plutôt que de les décaler silencieusement. Les spans avec timestamps non-parsables retombent sur l'intensité annuelle plate pour la région.

**Invariant somme-puis-divise (défense contre la dérive dedup).** Un helper unique `compute_operational_gco2(io_ops, intensity, pue)` empêche la formule d'être réimplémentée de façon incohérente entre chemins, étendu avec un helper de plus bas niveau `per_op_gco2(energy_kwh, intensity, pue)` qui est la source unique de vérité pour la multiplication `energy × intensity × pue`. Les trois chemins (proxy, horaire, Scaphandre) passent par ce helper.

## Intégration énergétique par processus Scaphandre

Le modèle proxy utilise une constante fixe `ENERGY_PER_IO_OP_KWH` (0,1 µWh par op). C'est une approximation à deux ordres de grandeur près. perf-sentinel offre un support opt-in pour remplacer le proxy par un coefficient mesuré au niveau service dérivé des lectures de puissance par processus de [Scaphandre](https://github.com/hubblo-org/scaphandre).

**Comment ça s'intègre dans l'architecture.** Scaphandre est un processus externe installé par l'utilisateur. perf-sentinel NE bundle PAS et NE fork PAS Scaphandre : il scrape l'endpoint Prometheus `/metrics` que Scaphandre expose déjà. Le module `score/scaphandre.rs` possède :

- `ScaphandreConfig` : parsé depuis `[green.scaphandre]` dans `.perf-sentinel.toml`.
- `ScaphandreState` : supporté par `ArcSwap<HashMap<String, ServiceEnergy>>` pour des lectures sans verrou depuis le chemin de scoring. Le scraper construit un nouveau `Arc<HashMap>` à chaque scrape réussi et le swap atomiquement ; les lecteurs font un seul `load_full()` sans contention de lock.
- `spawn_scraper()` : une tâche tokio qui s'exécute toutes les `scrape_interval_secs`.
- `parse_scaphandre_metrics()` : parser Prometheus sensible aux échappements. Itère par `.chars()` pour la sécurité UTF-8. Fast path sans allocation quand aucun backslash n'est présent dans les valeurs de labels. Gère les séquences `\"` et `\\`.
- `OpsSnapshotDiff` : un helper de snapshot-diff qui lit les compteurs d'ops par service depuis `MetricsState::service_io_ops_total`.
- `apply_scrape()` : applique les lectures de puissance parsées + les deltas d'ops à l'état.

**La formule.** Pour chaque service mappé dans une fenêtre de scrape :

```
power_watts       = process_power_microwatts / 1_000_000
joules            = power_watts × scrape_interval_secs
kwh               = joules / 3_600_000
energy_per_op_kwh = kwh / ops_observed_in_window
```

Quand `ops_observed_in_window == 0`, l'entrée d'état existante est **conservée** inchangée plutôt qu'effacée, ce qui évite le flapping du tag model pour les services idle.

**Où le coefficient se branche.** Le daemon prend un snapshot synchrone de toutes les sources d'énergie au début de chaque tick `process_traces` via `build_tick_ctx`. Cette map fusionnée est attachée à `CarbonContext.energy_snapshot` pour la durée du tick. Chaque `EnergyEntry` porte le coefficient et un tag de modèle (`"scaphandre_rapl"` ou `"cloud_specpower"`). Dans la boucle de spans de `compute_carbon_report`, l'énergie par op est résolue comme suit :

```rust
let (energy_kwh, measured_model) = match &ctx.energy_snapshot {
    Some(snapshot) => match snapshot.get(&span.event.service) {
        Some(entry) => (entry.energy_per_op_kwh, Some(entry.model_tag)),
        None => (ENERGY_PER_IO_OP_KWH, None),
    },
    None => (ENERGY_PER_IO_OP_KWH, None),
};
let op_co2 = per_op_gco2(energy_kwh, intensity_used, pue);
```

L'étape de scoring suit des flags par région (`any_scaphandre`, `any_kepler_ebpf`, `any_redfish_bmc`, `any_cloud_specpower`, `any_realtime_report`) et le `CarbonEstimate.model` de niveau supérieur reflète la source la plus précise utilisée : `"electricity_maps_api"` > `"scaphandre_rapl"` > `"kepler_ebpf"` > `"redfish_bmc"` > `"cloud_specpower"` > `"io_proxy_v3"` > `"io_proxy_v2"` > `"io_proxy_v1"`. Quand des facteurs de calibration sont actifs, `+cal` est ajouté. Toutes les sources d'énergie se composent naturellement avec les profils horaires : une op avec énergie mesurée en eu-west-3 à 3h du matin UTC utilise l'énergie mesurée ET l'intensité horaire simultanément.

**Compteur d'ops par service comme source unique de vérité.** Le scraper lit le compteur d'ops par service depuis `MetricsState::service_io_ops_total` (un `CounterVec` Prometheus) via `snapshot_service_io_ops()`. Le chemin d'intake d'événements du daemon incrémente ce compteur sur chaque événement normalisé.

**Shutdown gracieux.** Le daemon capture le `JoinHandle` du scraper et appelle `.abort()` sur lui avant le drain `process_traces` final dans la branche Ctrl-C. Cela empêche les lignes de log "scrape failed" d'apparaître après le message "Shutting down daemon".

**Ce que Scaphandre ne fait PAS.** Voir la section `Limites de précision Scaphandre` dans `docs/FR/LIMITATIONS-FR.md` pour la discussion complète. Version courte : Scaphandre donne des coefficients par service, pas d'attribution par finding. Deux findings N+1 dans la même JVM pendant la même fenêtre de scrape partagent le même coefficient par construction, car RAPL est au niveau processus, pas au niveau span.

## Estimation d'énergie cloud (CPU% + SPECpower)

Pour les VMs cloud (AWS, GCP, Azure) qui n'exposent pas Intel RAPL aux guests, perf-sentinel offre une voie alternative d'estimation d'énergie basée sur les métriques d'utilisation CPU et le modèle SPECpower. Le module se trouve dans `score/cloud_energy/` et reproduit la structure du module Scaphandre.

**Architecture.** Le répertoire `cloud_energy/` contient :

- `config.rs` : `CloudEnergyConfig` et `ServiceCloudConfig` par service (provider, région, instance_type, overrides optionnels idle/max watts).
- `table.rs` : table de lookup embarquée avec les valeurs idle et max watts pour ~390 types d'instances après le refresh CCF du 2026-04-24. Toutes les entrées suivent une méthodologie unique homogène : `idle_watts = vCPU * idle_per_vCPU` et `max_watts = vCPU * max_per_vCPU`, avec les coefficients tirés par fournisseur de `ccf-coefficients` 2026-04-24 (`coefficients-{aws,gcp,azure}-use.csv`). Aucun overhead baseboard n'est reconstruit : la colonne baseboard AWS a été abandonnée par CCF en 2026-04-24 et n'est pas réajoutée. La règle des 5 pour cent répartit les entrées modernes en deux groupes : ré-alignées sur CCF quand le calcul SPECpower direct divergeait (Sapphire Rapids sur AWS `m7i`/`c7i`/`r7i` et GCP `c3`, EPYC Genoa sur AWS `m7a`/`c7a` et GCP `c3d`/`n2d`, Graviton 2/3/3E/4 mappés sur le proxy CCF EPYC 2nd Gen, EPYC Turin sur AWS `m8a`/`c8a`, Emerald Rapids sur GCP `c4`), conservées sur le calcul `SPECpower_ssj 2008` direct 2024 Q1 - 2026 Q2 quand dans les 5 pour cent ou absentes du CSV du fournisseur (AWS Milan `m6a`/`c6a`, Turin GCP `c4d`, Ampere Altra GCP `t2a`, Sapphire Rapids Azure, Emerald Rapids Azure, Genoa Azure, Cobalt 100 Azure, Sierra Forest). Nouvelles familles AWS ajoutées par ce refresh : `m8a` / `c8a` (Turin), `m8i` / `c8i` (Emerald Rapids), `r7a` (Genoa memory-optimized). Nouvelle famille GCP : `c4a` (Axion ARM Neoverse V2, proxié sur AWS Graviton 4). Voir `docs/FR/LIMITATIONS-FR.md`.
- `scraper.rs` : scraper API JSON Prometheus. Interroge `avg(rate(cpu_metric[interval]))` par service.
- `state.rs` : `CloudEnergyState` supporté par `ArcSwap` pour des lectures sans verrou depuis le chemin de scoring.
- `mod.rs` : ré-exports et documentation du module.

**La formule.** Pour chaque service avec une config cloud :

```
cpu_percent       = prometheus_query(cpu_metric, service_label)
watts             = idle_watts + (max_watts - idle_watts) * (cpu_percent / 100)
joules            = watts * scrape_interval_secs
kwh               = joules / 3_600_000
energy_per_op_kwh = kwh / ops_in_window
```

**Tag de modèle et précédence.** Le coefficient porte le tag `"cloud_specpower"`. Dans `build_tick_ctx`, les sources de plus haute fidélité prennent la précédence : Scaphandre écrase Kepler, qui écrase Redfish, qui écrase cloud SPECpower pour un même service. Le tag de modèle de niveau supérieur reflète la source la plus précise : `electricity_maps_api` > `scaphandre_rapl` > `kepler_ebpf` > `redfish_bmc` > `cloud_specpower` > `io_proxy_v3` > `io_proxy_v2` > `io_proxy_v1`.

**Daemon uniquement.** Comme Scaphandre, l'estimation d'énergie cloud est une fonctionnalité daemon uniquement. La commande `analyze` batch utilise toujours le modèle proxy.

**Ce que cloud SPECpower ne fait PAS.** Voir `docs/FR/LIMITATIONS-FR.md` "Limites de précision du cloud SPECpower" pour la discussion complète. Le modèle SPECpower capture la puissance proportionnelle au CPU mais pas la mémoire, les I/O ou le réseau. La multi-tenance n'est pas corrigée. La précision est d'environ +/-30%.

## Notes d'attribution Kepler et Redfish

Les intégrations Kepler et Redfish suivent le même schéma d'état partagé que Scaphandre et cloud SPECpower (`AgedEnergyMap` adossé à `ArcSwap`, fenêtre de fraîcheur `3 × scrape_interval`, `OpsSnapshotDiff` partagé par service) mais chacune porte des compromis méthodologiques qui méritent une note dédiée.

**Sémantique du delta de compteur Kepler.** Kepler expose un compteur de joules cumulés monotone par conteneur ou processus, contrairement à la jauge de microwatts instantanée de Scaphandre. La tâche de scrape tient une `HashMap<service, last_raw_joules>` et calcule à chaque tick `delta = current - previous`, puis n'émet l'entrée que si `delta > 0.0 && delta.is_finite()`. Ce filtre est volontaire : quand l'exporteur Kepler redémarre, le compteur se réinitialise à zéro et `current < previous` produit un delta négatif, la garde le rejette. Les lectures non finies (`NaN`, `±Inf`) sont également rejetées. Le scrape suivant produit le prochain delta significatif à partir de la nouvelle référence. La première observation par service (pas de `previous`) n'émet pas de delta, le compteur brut est enregistré pour le scrape suivant.

**Mode de scrape Kepler (direct vs Prometheus-médié).** Kepler s'exécute en général comme `DaemonSet` Kubernetes (un pod par nœud). En production, le déploiement réaliste consiste à scraper un Prometheus amont qui agrège l'ensemble du `DaemonSet` plutôt qu'un seul pod Kepler, sinon seule l'énergie d'un nœud est visible. L'intégration `[green.kepler]` actuelle ne couvre que le **scrape direct** (mêmes contours que Scaphandre, avec en plus le calcul de delta de compteur cumulatif). Une version ultérieure ajoutera un mode `source = "prometheus"` qui émettra des requêtes PromQL sur un Prometheus amont, la surface de configuration anticipe cette évolution avec l'enum `metric_kind` déjà en place.

**Formule d'attribution au niveau du nœud pour Redfish.** Redfish expose une lecture de puissance murale par châssis, pas par service. Le scraper transforme cette lecture en coefficient énergie-par-opération par service via :

```
chassis_joules = chassis_watts × scrape_interval_secs
total_ops      = Σ ops_delta(service) pour service ∈ mappé(châssis)
energy_per_op  = (chassis_joules / 3_600_000) / total_ops    (en kWh par opération)
```

Chaque service mappé au châssis reçoit la **même** valeur `energy_per_op` pour cette fenêtre de scrape. C'est l'interprétation correcte d'une puissance au niveau du nœud tant qu'aucun signal plus fin n'est disponible, et c'est documenté comme une granularité connue dans `docs/FR/LIMITATIONS-FR.md` "Limites de précision Redfish BMC". Les châssis inactifs (aucune opération mappée cette fenêtre) laissent l'entrée précédente de chaque service intacte, sans division par zéro et sans oscillation. Les lectures de wattage non finies, nulles, à zéro ou négatives sont rejetées comme états transitoires du BMC, le coefficient précédent est préservé.

**Limitation TLS Redfish.** La plupart des BMCs présentent un certificat auto-signé par défaut. Le `http_client::build_client` partagé de perf-sentinel s'appuie sur `hyper-rustls` avec le magasin de racines webpki publiques, qui rejette les certificats auto-signés. Le champ `RedfishConfig::ca_bundle_path` anticipe les bundles CA fournis par l'opérateur, mais le chargement PEM effectif est **reporté à une version ultérieure**. Définir `ca_bundle_path` aujourd'hui amène le scraper à émettre une erreur explicite et à refuser de démarrer : c'est volontaire, pour que les opérateurs avec un BMC auto-signé voient la limite immédiatement plutôt qu'au milieu d'un handshake TLS loin de la configuration concernée. Contournements dans la version courante : placer le BMC derrière un reverse proxy qui présente un certificat signé publiquement, ou utiliser HTTP sur un segment réseau de confiance.

**Variance JSON entre fournisseurs pour Redfish.** Les différents fournisseurs de BMC renvoient des formes légèrement différentes sous `/redfish/v1/Chassis/{id}/Power`. Le pointeur JSON par défaut `/PowerControl/0/PowerConsumedWatts` résout correctement chez Dell iDRAC, HPE iLO, Lenovo XCC, Supermicro X11+ et la référence OpenBMC, mais les formes spécifiques au fournisseur (ex. `Oem.Hpe.PowerSummary.Watts` chez HPE) sont surchargeables via le champ de configuration `power_path`. Le parseur rejette `null`, `0`, les valeurs négatives et `NaN` comme invalides pour que les états transitoires du BMC (démarrage, rampe de ventilateurs) ne polluent pas le coefficient.

**Protection contre la limitation de débit Redfish.** `scrape_interval_secs` est écrêté à `[15, 3600]` pour Redfish (contre `[1, 3600]` pour Scaphandre et Kepler). Plusieurs BMCs (notamment HPE iLO 4/5) limitent les requêtes Redfish en dessous de 30 secondes, et de nombreux fournisseurs maintiennent la valeur en cache interne sur un cycle de mise à jour de 30 s, donc un intervalle plus rapide n'apporte aucune information tout en s'exposant à des erreurs 429. Valeur par défaut : 60 s.

**Surface SSRF assumée par construction.** Les scrapers Kepler, Redfish, Scaphandre et cloud-energy acceptent tous de joindre une URL loopback ou RFC 1918 (`http://127.0.0.1:9102/metrics`, `https://10.0.0.5/redfish/v1/...`). C'est volontaire : Kepler s'exécute typiquement en `DaemonSet` sur le même nœud, les BMCs sont sur des réseaux de management, Scaphandre expose un endpoint Prometheus local. La validation à la lecture de la configuration refuse les URLs avec des identifiants embarqués (`@`) ou des caractères de contrôle, le cap sur la taille du corps dans `http_client::fetch_get` (8 Mio) borne la mémoire par fetch, et le client `hyper-util` partagé est construit sans suivi de redirections, donc un endpoint malicieux ne peut pas faire un 302 vers `http://169.254.169.254/`. La garantie au déploiement : chaque URL que joint le daemon vient d'une configuration `.perf-sentinel.toml` fournie par l'opérateur, jamais dérivée d'une entrée externe (spans, réponses BMC, résultats de requêtes Prometheus).

**Tags carbone à deux axes.** La fidélité de l'énergie (`E`, classée par [`carbon_compute::higher_fidelity_measured`]) et la fidélité de l'intensité réseau (`I`, exposée par [`region_breakdown::select_co2_model_tag`]) sont des axes indépendants. Une même fenêtre peut porter `co2.model = "electricity_maps_api"` (l'intensité temps réel est la source `I` la plus précise) tout en reportant `per_service_energy_model` à `"scaphandre_rapl"` pour le même service (RAPL est la source `E` la plus précise). L'asymétrie est intentionnelle : tagger le rapport selon la source `I` la plus précise pendant que la ventilation par service suit `E` permet aux auditeurs de voir les deux dimensions sans les fusionner dans un seul tag.

## Intégration intensité temps réel Electricity Maps

Le bloc `[green.electricity_maps]` active le polling temps réel de l'intensité carbone du réseau électrique. Le scraper du daemon interroge périodiquement l'endpoint `/carbon-intensity/latest` d'Electricity Maps par zone et alimente le `CarbonContext` du tick courant, où la valeur prend la précédence sur les profils annuels et horaires pour les régions cloud mappées. Documenté à <https://app.electricitymaps.com/developer-hub/api/getting-started>.

**Déduplication par zone.** Le scraper itère sur `region_map` (`cloud_region -> zone`) mais une zone donnée n'est récupérée qu'une seule fois par tick, même si plusieurs `cloud_region` pointent dessus (montages multi-AZ classiques, ou `aws:eu-west-3` et `local-k3d` tous deux pinnés sur `FR`). La lecture est ensuite dispatchée à chaque `cloud_region` qui correspond. Le nombre d'appels API reste proportionnel au nombre de zones distinctes, pas à la taille de `region_map`. Critique sur les tiers à quota contraint, le tier gratuit en particulier limite à une seule zone aujourd'hui mais le calcul de quota bénéficie quand même d'un même mapping de zone partagé entre staging et prod.

**Métadonnées d'estimation.** L'API Electricity Maps expose deux champs optionnels à côté de `carbonIntensity` :

```json
{
  "zone": "FR",
  "carbonIntensity": 56.0,
  "isEstimated": true,
  "estimationMethod": "TIME_SLICER_AVERAGE"
}
```

`isEstimated` vaut `true` quand l'API a comblé un trou (zone Tier B/C, ou trou temporel comblé par un algorithme comme `TIME_SLICER_AVERAGE`), et `false` pour les valeurs entièrement mesurées. perf-sentinel parse les deux champs avec `#[serde(default)]` pour rester forward-compatible si une version future de l'API cesse de les émettre.

Les flags se propagent à travers `IntensityReading` (state) jusqu'au `CarbonContext.real_time_intensity` du tick puis jusqu'à l'accumulateur par région. La ligne `green_summary.regions[]` les expose comme deux champs optionnels :

```json
{
  "status": "known",
  "region": "eu-west-3",
  "intensity_source": "real_time",
  "grid_intensity_gco2_kwh": 56.0,
  "intensity_estimated": true,
  "intensity_estimation_method": "TIME_SLICER_AVERAGE",
  "co2_gco2": 1.234
}
```

Les deux champs utilisent `#[serde(skip_serializing_if = "Option::is_none")]` pour que les consommateurs qui les ignorent continuent à désérialiser la ligne sans changement. Les champs n'apparaissent que quand `intensity_source == "real_time"`. Les spans qui retombent sur les profils annuels ou horaires ne portent jamais la metadata, même si l'accumulateur l'a capturée depuis un span voisin.

C'est le signal qu'un reporting Scope 2 attend pour distinguer les émissions mesurées des émissions modélisées. Les auditeurs admettent typiquement les valeurs estimées quand la méthodologie est documentée, surfacer le tag d'algorithme (`TIME_SLICER_AVERAGE`, `GENERAL_PURPOSE_ZONE_DEVELOPMENT`, etc.) rend la piste d'audit auto-portée.

### Rendu utilisateur (0.5.10)

Les deux champs sont surfacés dans les deux couches de rendu visibles par l'opérateur, qui voit la distinction d'un seul coup d'œil.

**Dashboard.** Le tableau Regions de l'onglet GreenOps gagne une 6e colonne `Estimated`. Trois états visuels : un badge orange `Estimated` quand `intensity_estimated == true` (le hover surface une infobulle avec la `intensity_estimation_method`), un badge vert `Measured` quand `intensity_estimated == false`, un tiret neutre pour les lignes dont `intensity_source` n'est pas `real_time` (les profils annuels, horaires et mensuels-horaires ne portent pas de metadata d'estimation, le champ reste `None` de bout en bout). Les deux badges réutilisent les variables CSS de la palette existante (`--color-background-warning`, `--color-text-warning`, `--color-background-success`, `--color-text-success`) pour que les thèmes sombre et clair s'adaptent automatiquement.

**Terminal.** La ligne par-région de `print_green_summary` gagne un suffixe après le champ `source: real_time`. Format :

```
- fr: 42 I/O ops, 0.000123 gCO₂ (56 gCO₂/kWh, source: real_time, estimated/TIME_SLICER_AVERAGE)
- de: 24 I/O ops, 0.000456 gCO₂ (380 gCO₂/kWh, source: real_time, measured)
- us-east-1: 12 I/O ops, 0.000789 gCO₂ (410 gCO₂/kWh, source: annual)
```

Le suffixe est vide quand `intensity_estimated` est `None`, donc les scrapers de logs existants continuent à matcher la forme de ligne pre-0.5.10.

### Version d'API (0.5.11)

perf-sentinel cible l'endpoint `Electricity Maps` API v4 par défaut depuis 0.5.11. Les versions précédentes par défaut sur v3, qu'Electricity Maps continue à servir mais considère comme legacy. La migration a été déclenchée par la promotion de v4 en "latest" dans la doc reference du developer hub (<https://app.electricitymaps.com/developer-hub/api/reference>) et constitue une protection forward-defense contre une éventuelle dépréciation de v3.

Le schéma de réponse sur l'endpoint `carbon-intensity/latest` est byte-identical entre v3 et v4, donc la migration est transparente pour les consommateurs en aval (les lignes `green_summary.regions[]` sont inchangées quelle que soit la version d'API configurée, le path de parsing utilise la même struct).

Rétro-compatibilité : les configs `.perf-sentinel.toml` existantes qui pinnent `endpoint = "https://api.electricitymaps.com/v3"` continuent à fonctionner. Le scraper détecte le path legacy au démarrage via `ApiVersion::from_endpoint` (matche `.../v3` en fin d'URL ou `.../v3/...` dans le path, avec garde de word-boundary contre les faux positifs type `/v30` ou `/v300`) et émet un `tracing::warn!` une fois par démarrage du daemon, pointant l'opérateur vers la migration v4. Depuis 0.5.12, `ApiVersion::from_endpoint` est l'unique source de vérité, également consommée par le champ `green_summary.scoring_config.api_version`. La chaîne d'endpoint passe par `sanitize_for_terminal` avant d'être loggée, pour qu'une TOML hostile ne puisse pas injecter d'octets de contrôle ANSI dans le flux de logs du daemon.

### Transparence de la config de scoring (0.5.12)

L'objet `green_summary.scoring_config` expose la configuration runtime de l'intégration Electricity Maps pour qu'un auditeur ou un reporter Scope 2 puisse voir quel modèle carbone a produit les chiffres sans lire la TOML de l'opérateur. Trois champs, tous dérivés d'`ElectricityMapsConfig` au chargement de la config via `ScoringConfig::from_electricity_maps` :

- `api_version` : détecté à partir d'`api_endpoint` via `ApiVersion::from_endpoint`. Une de `v3` (legacy), `v4` (défaut), `custom` (proxy ou mock sans suffixe `/vN`).
- `emission_factor_type` : miroir du knob TOML, une de `lifecycle` (défaut) ou `direct`.
- `temporal_granularity` : miroir du knob TOML, une de `hourly` (défaut), `5_minutes`, `15_minutes`.

**Périmètre de la surface.** `scoring_config` capture **uniquement la configuration cliente Electricity Maps**. C'est une empreinte méthodologique partielle, pas le vecteur d'entrée SCI complet. Un strict-replay du calcul carbone à partir d'une baseline sauvegardée nécessiterait aussi `[green] embodied_carbon_per_request_gco2`, `[green] use_hourly_profiles`, `[green] per_operation_coefficients`, `[green] include_network_transport` et `[green] network_energy_per_byte_kwh` (aucun n'est dans le JSON aujourd'hui), plus le PUE par région tiré de la table provider embarquée (récupérable seulement si la classification du provider est stable entre les runs). Exposer l'empreinte méthodologique complète est un travail futur, la surface 0.5.12 ferme le gap d'audit sur la tranche Electricity Maps spécifiquement parce que c'est cette tranche que le travail 0.5.10 + 0.5.11 a enrichie de knobs sans les exposer.

**Rétro-compatibilité.** Le champ vaut `None` (et le bandeau du dashboard / la ligne terminal sont masqués) quand `[green.electricity_maps]` n'est pas configuré, donc les rapports produits sans Electricity Maps gardent une forme identique au pre-0.5.12. La forme JSON est additive sur `green_summary` via `#[serde(skip_serializing_if = "Option::is_none", default)]`, donc les baselines pre-0.5.12 réinjectées via `report --before` continuent à parser.

**Plumbing.** `Config::carbon_context()` peuple `CarbonContext::scoring_config: Option<ScoringConfig>` à partir du `green_electricity_maps` chargé. `score_green` le lit depuis le contexte et le copie dans le `GreenSummary` résultant. Le `build_tick_ctx` per-tick du daemon hérite du champ via le clone existant `Cow::Owned(ctx)`, sans reconstruction par tick. Le pipeline batch CLI le récupère directement depuis le `CarbonContext` construit une seule fois.

**Chemin snapshot du daemon.** Depuis 0.5.13, `/api/export/report` sert un `green_summary` vivant rafraîchi par l'event loop après chaque batch (régions, top offenders, ratio d'I/O évitables, chiffres CO2). `scoring_config` est ajouté par-dessus à partir du `Config` de démarrage du daemon, ce qui fait que la chip d'audit et le tab GreenOps apparaissent tous les deux dans le HTML rendu lorsqu'un opérateur fait passer le snapshot par `perf-sentinel report --input -`. La limitation 0.5.12 précédente (le snapshot retournait `GreenSummary::disabled(0)` et seul le champ `scoring_config` était patché, masquant le tab GreenOps) est levée.

**Défense contre l'injection terminal :** les trois champs sont des enums Rust typés à variants bornés, donc le rendu terminal dans `print_green_summary` n'a pas besoin de les wrapper dans `sanitize_for_terminal` (contrairement à `intensity_estimation_method` qui porte une `String` libre depuis le JSON `--input`). Le rendu HTML des chips utilise `textContent` (pas `innerHTML`) et `setAttribute("title", ...)`, qui auto-échappent tous les deux.

## Coefficients énergétiques par opération

Le modèle proxy utilise une seule constante `ENERGY_PER_IO_OP_KWH` (0.1 µWh) pour chaque opération I/O. Cela traite un `SELECT` en lecture seule sur un index de la même manière qu'un `INSERT` écrivant dans le WAL et les pages de données. Les coefficients par opération affinent cela en appliquant un multiplicateur selon le type d'opération.

**Multiplicateurs SQL.** Le verbe est extrait du premier mot du champ `target` (la requête SQL brute), pas du champ `operation`. C'est nécessaire car les spans ingérées via OTLP stockent `db.system` (ex. "postgresql") dans `operation`, pas le verbe SQL.

| Verbe SQL | Multiplicateur | Justification                     |
|-----------|----------------|-----------------------------------|
| SELECT    | 0.5x           | Lecture seule, pas d'écriture WAL |
| INSERT    | 1.5x           | Écriture WAL + page de données    |
| UPDATE    | 1.5x           | Lecture + écriture                |
| DELETE    | 1.2x           | Marquage + WAL                    |
| Autre     | 1.0x           | DDL, EXPLAIN, BEGIN, etc.         |

**Tiers de taille de payload HTTP.** Pour les spans HTTP, le multiplicateur dépend de `response_size_bytes` (extrait de l'attribut OTel `http.response.body.size`).

| Taille payload | Multiplicateur | Seuil           |
|----------------|----------------|-----------------|
| Petit          | 0.8x           | < 10 Ko         |
| Moyen          | 1.2x           | 10 Ko à 1 Mo    |
| Grand          | 2.0x           | > 1 Mo          |
| Inconnu        | 1.0x           | attribut absent |

**Sources.** Les ratios relatifs proviennent de benchmarks académiques d'énergie SGBD (Xu et al. VLDB 2010, Tsirogiannis et al. SIGMOD 2010) et de la méthodologie Cloud Carbon Footprint.

**Où cela s'intègre.** Dans la boucle de spans de `compute_carbon_report`, le chemin proxy applique le coefficient. Quand de l'énergie mesurée est disponible (Scaphandre ou cloud SPECpower), le coefficient n'est PAS appliqué.

**Détail hot path.** La fonction `energy_coefficient()` est `#[inline]` et n'alloue pas : elle utilise `split_ascii_whitespace().next()` (lazy, s'arrête au premier espace) pour l'extraction du verbe et `eq_ignore_ascii_case` pour le matching au lieu de `to_ascii_lowercase()`. Le verbe le plus courant (SELECT) matche dès la première comparaison.

**Config.** `[green] per_operation_coefficients = true` (défaut). Le tag de modèle reste `io_proxy_v1` ou `io_proxy_v2`. Les coefficients par opération sont un raffinement du modèle proxy, pas une nouvelle classe de modèle.

## Énergie de transport réseau

Pour les appels HTTP inter-régions, le coût énergétique du transfert d'octets sur le backbone internet peut être significatif. perf-sentinel offre un terme optionnel d'énergie de transport réseau.

**La formule.**

```
energy_transport_kwh = bytes_transférés * ENERGY_PER_BYTE_KWH
transport_co2        = energy_transport_kwh * intensité_région_source * pue_source
```

Le coefficient par défaut est `4e-11 kWh/octet` (0.04 kWh/Go), le milieu de la fourchette 0.03-0.06 kWh/Go des études récentes (Mytton, Lunden & Malmodin, J. Industrial Ecology, 2024 ; Sustainable Web Design, 2024). L'ancienne valeur Shift Project 2019 (0.07 kWh/Go) était sur la borne haute. Mytton et al. (2024) montrent que le modèle kWh/Go est une simplification : les équipements réseau ont une puissance de base fixe significative. Le coefficient est configurable.

**Détection inter-région.** L'énergie de transport n'est calculée que quand les régions de l'appelant et de l'appelé diffèrent :

1. **Région appelant** : résolue via la chaîne standard (`span.cloud_region` > `service_regions[service]` > `default_region`).
2. **Région appelé** : le hostname est extrait de l'URL cible HTTP puis cherché dans `ctx.service_regions`. Si non mappé, perf-sentinel suppose conservativement la même région.
3. Si les deux régions sont résolues et diffèrent (comparaison insensible à la casse), l'énergie de transport est calculée.

**Sortie rapport.** Le CO₂ transport apparaît comme `transport_gco2` dans `CarbonReport` et `GreenSummary`. Il est inclus dans le total SCI : `total_mid = opérationnel + embodié + transport`. Le champ est omis du JSON quand nul ou quand la fonctionnalité est désactivée.

**Config.** `[green] include_network_transport = false` (défaut, opt-in). Le coefficient est configurable via `[green] network_energy_per_byte_kwh`.

**Optimisations hot path.** Le chemin transport s'exécute dans la boucle de scoring par span. Deux micro-optimisations évitent les allocations dans le cas courant :
- Le hostname extrait de l'URL est comparé à `service_regions` avec un pattern probe-before-allocate : `to_ascii_lowercase()` n'est appelé que si le hostname contient des majuscules (rare pour les noms de service Kubernetes/Docker).
- La région du caller réutilise `region_ref` déjà résolu plus tôt dans la même itération.

**Scalaire `co2_grams` des top offenders.** Le `co2_grams` par offender utilise la constante plate `ENERGY_PER_IO_OP_KWH`. Quand `per_operation_coefficients` est actif (le défaut), `co2_grams` est mis à `None` pour éviter une incohérence avec le breakdown par région. Le classement (par IIS) n'est pas affecté.

**Limitations.** Voir `docs/FR/LIMITATIONS-FR.md` "Énergie de transport réseau" pour la discussion complète.

## Cohérence du cache d'état énergétique

Le scraper Scaphandre et le scraper SPECpower cloud publient tous les deux des lectures `energy_per_op_kwh` par service vers le scoring path à chaque tick. Les deux états partagent un stockage `ArcSwap` dans `crates/sentinel-core/src/score/energy_state.rs`. Les deux types publics (`ScaphandreState` et `CloudEnergyState`) sont des wrappers newtype fins qui délèguent à `AgedEnergyMap` et conservent leur identité nominale pour un plumbing type-safe à travers le daemon.

Le design est volontairement read-heavy / write-rare :

- **Écritures** : une fois par intervalle de scrape (5s par défaut pour Scaphandre, 15s pour cloud energy) par une seule tâche.
- **Lectures** : une fois par tick `process_traces` (typiquement plusieurs par seconde sous charge OTLP réelle).
- **Cohérence** : les lecteurs récupèrent l'`Arc` qui était courant au moment où ils ont appelé `load_full`, les écrivains ne bloquent personne.

`ArcSwap` a été choisi plutôt que `RwLock<HashMap>` parce que la lecture côté `process_traces` est sur la hot loop, et l'échange de pointeur via swap est wait-free contrairement à un `RwLock` qui bloque brièvement sur `read()` quand un writer tient le verrou.

## Champ de confiance sur les findings (interop perf-lint planifié)

Un champ `confidence` est tamponné sur chaque `Finding` dans le rapport JSON et SARIF, indiquant le contexte source de la détection. La valeur est définie par l'appelant du pipeline (`pipeline::analyze_with_traces` pour le mode batch → toujours `CiBatch` ; `daemon::process_traces` pour le mode streaming → dérivé de `config.daemon_environment`). Les détecteurs eux-mêmes ne raisonnent jamais sur la confiance : ils émettent `Confidence::default()` et l'appelant écrase.

Valeurs :

| Confidence          | Source                                                   | Rank SARIF |
|---------------------|----------------------------------------------------------|------------|
| `CiBatch`           | Mode batch `analyze`, toujours                           | 30         |
| `DaemonStaging`     | Daemon `watch` avec `[daemon] environment="staging"`     | 60         |
| `DaemonProduction`  | Daemon `watch` avec `[daemon] environment="production"`  | 90         |

Le champ apparaît dans :

- **Rapport JSON** : chaque objet finding inclut `"confidence": "ci_batch"` / `"daemon_staging"` / `"daemon_production"`.
- **SARIF v2.1.0** : entrée de bag `properties.confidence` par résultat ET une valeur standard `rank` SARIF (0-100).
- **Sortie terminal CLI** : NON affiché (le terminal reste propre pour l'usage interactif).

Le consommateur planifié est perf-lint, une intégration IDE compagnon (pas encore publiée), qui importera les findings runtime depuis la sortie JSON de perf-sentinel et appliquera un multiplicateur de sévérité basé sur la confiance. Tout outil tiers qui consomme la même sortie JSON ou SARIF peut utiliser ce champ de la même manière. Voir `docs/FR/INTEGRATION-FR.md` "Champ de confiance sur les findings" pour l'exemple d'intégration.
