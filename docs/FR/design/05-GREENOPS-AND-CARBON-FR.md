# Scoring GreenOps et conversion carbone

## Score d'intensité I/O (IIS)

La métrique centrale est le Score d'Intensité I/O (I/O Intensity Score) : le nombre d'opérations I/O générées par requête utilisateur pour un endpoint donné.

```
IIS(endpoint) = total_io_ops(endpoint) / invocation_count(endpoint)
```

Un endpoint appelé à travers 3 traces avec 18 opérations I/O au total a `IIS = 18 / 3 = 6.0`. Cela normalise les différents volumes de trafic : un endpoint à fort trafic avec 1000 invocations et 6000 opérations I/O a le même IIS (6.0) qu'un endpoint à faible trafic avec 3 invocations et 18 opérations.

Le dénominateur utilise `.max(1)` comme garde contre la division par zéro, bien que ce cas ne puisse pas se produire en pratique (un endpoint qui apparaît dans `endpoint_stats` a forcément été vu dans au moins une trace).

## Algorithme de scoring : cinq phases

### Phase 1 : statistiques par endpoint

```rust
let mut seen_endpoints: HashSet<&str> = HashSet::new();
for trace in traces {
    seen_endpoints.clear();
    for span in &trace.spans {
        total_io_ops += 1;
        let stats = endpoint_stats.entry(key).or_insert_with(|| EndpointStats { ... });
        stats.total_io_ops += 1;
        seen_endpoints.insert(key);
    }
    for &ep in &seen_endpoints {
        endpoint_stats.get_mut(ep).unwrap().invocation_count += 1;
    }
}
```

**Réutilisation du HashSet :** `seen_endpoints.clear()` réutilise le même HashSet à chaque itération de trace. Sans cela, chaque trace allouerait un nouveau HashSet. Pour 10 000 traces, cela économise 10 000 allocations.

**`EndpointStats<'a>` avec `service` emprunté :** le champ `service` emprunte `&'a str` depuis les événements span au lieu de cloner le String. Le clone ne se produit que plus tard lors de la construction des structs `TopOffender` pour la sortie. Cela évite un clone de String par endpoint unique dans la boucle interne.

### Phase 2 : dédup des I/O évitables

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

### Phase 3 : calcul de l'IIS par endpoint

```rust
let iis_map: HashMap<&str, f64> = endpoint_stats.iter()
    .map(|(&ep, stats)| {
        let invocations = stats.invocation_count.max(1) as f64;
        (ep, stats.total_io_ops as f64 / invocations)
    })
    .collect();
```

La map IIS est calculée une seule fois et réutilisée pour l'enrichissement des findings (Phase 4) et le classement des top offenders (Phase 5).

### Phase 4 : enrichir les findings

Chaque finding reçoit un `GreenImpact` :

```rust
GreenImpact {
    estimated_extra_io_ops: if slow { 0 } else { occurrences - 1 },
    io_intensity_score: iis,
}
```

### Phase 5 : top offenders

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

### Constante énergétique

```rust
const ENERGY_PER_IO_OP_KWH: f64 = 0.000_000_1; // 0,1 uWh par opération I/O
```

C'est une approximation d'ordre de grandeur, pas une valeur mesurée. Elle tient compte d'une requête de base de données ou d'un aller-retour HTTP typique sur une infrastructure cloud. Le [projet Cloud Carbon Footprint](https://www.cloudcarbonfootprint.org/docs/methodology/) utilise une approche similaire d'estimation de l'énergie à partir de l'utilisation des ressources plutôt que d'une mesure directe.

La valeur doit être divulguée comme méthodologie selon les exigences SCI. Elle est documentée dans le code, dans [LIMITATIONS-FR.md](../LIMITATIONS-FR.md) et ici.

### Formule de conversion

```
gCO2eq = io_ops × ENERGY_PER_IO_OP_KWH × intensité_carbone × PUE
```

Où :
- `intensité_carbone` = gCO2eq/kWh pour le réseau électrique de la région
- `PUE` = Power Usage Effectiveness (facteur de surcoût du datacenter)

### Recherche par région

La table d'intensité carbone est embarquée comme tableau statique et convertie en `HashMap` via `LazyLock` :

```rust
static REGION_MAP: LazyLock<HashMap<&'static str, (f64, Provider)>> =
    LazyLock::new(|| CARBON_TABLE.iter().map(...).collect());
```

**Pourquoi `LazyLock<HashMap>` au lieu d'un scan linéaire ?** L'implémentation originale parcourait les 41 entrées à chaque appel. Avec le HashMap, la recherche est O(1). Le coût d'initialisation est payé une seule fois au premier accès.

**Recherche insensible à la casse :** la fonction publique `lookup_region()` convertit l'entrée en minuscules via `to_ascii_lowercase()` avant la recherche. Toutes les clés de la table sont stockées en minuscules. En interne, une fonction privée `lookup_region_lower()` saute la conversion pour les appelants ayant déjà normalisé la région (ex. `score_green` pré-lowercase une seule fois et réutilise le résultat pour les appels multiples à `io_ops_to_co2_grams`).

### Valeurs PUE

| Fournisseur | PUE   | Source                                                                                                                                                    |
|-------------|-------|-----------------------------------------------------------------------------------------------------------------------------------------------------------|
| AWS         | 1,135 | [AWS Sustainability](https://sustainability.aboutamazon.com/)                                                                                             |
| GCP         | 1,10  | [Google Environmental Report](https://sustainability.google/reports/)                                                                                     |
| Azure       | 1,185 | [Microsoft Sustainability Report](https://www.microsoft.com/en-us/corporate-responsibility/sustainability)                                                |
| Générique   | 1,2   | [Uptime Institute Global Survey 2023](https://uptimeinstitute.com/resources/research-and-reports/uptime-institute-global-data-center-survey-results-2023) |

Le PUE (Power Usage Effectiveness) mesure le ratio entre l'énergie totale du datacenter et l'énergie de l'équipement IT. Un PUE de 1,10 signifie 10% de surcoût pour le refroidissement, l'éclairage et l'infrastructure. La moyenne de l'industrie est ~1,58, mais les fournisseurs cloud hyperscale atteignent des valeurs significativement plus basses.

### Données d'intensité carbone

Les intensités carbone régionales du réseau électrique (gCO2eq/kWh) proviennent des moyennes annuelles [Electricity Maps](https://www.electricitymaps.com/) (2023-2024) et du projet [Cloud Carbon Footprint](https://www.cloudcarbonfootprint.org/). La table couvre 15 régions AWS, 8 régions GCP, 6 régions Azure et 14 codes pays ISO.

Quand la région configurée n'est pas trouvée dans la table, les champs CO2 sont omis du rapport (aucune valeur par défaut n'est inventée).
