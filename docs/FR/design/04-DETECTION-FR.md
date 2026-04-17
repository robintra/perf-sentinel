# Algorithmes de détection

La détection est la quatrième étape du pipeline. Elle analyse les traces corrélées pour identifier sept types d'anti-patterns : les requêtes N+1, les appels redondants, les opérations lentes, le fanout excessif, les services bavards, la saturation du pool de connexions et les appels sérialisés.

## Pattern partagé : clés HashMap empruntées

Les trois détecteurs regroupent les spans par une clé composite. Un point clé est que les spans vivent dans la struct `Trace`, qui survit à la fonction de détection. Cela signifie que nous pouvons **emprunter** depuis les spans au lieu de cloner :

```rust
// N+1 : grouper par (event_type, template)
HashMap<(&EventType, &str), Vec<usize>>

// Redondant : grouper par (event_type, template, params)
HashMap<(&EventType, &str, &[String]), Vec<usize>>

// Lent : grouper par (event_type, template)
HashMap<(&EventType, &str), Vec<usize>>
```

Les valeurs sont des `Vec<usize>` : des indices dans `trace.spans` plutôt que des spans clonés. Cela garde le HashMap petit et évite de copier les données d'événements.

Pour une trace avec 50 spans, chacun ayant un template de 40 caractères, les clés empruntées économisent 50 × 40 = 2 000 octets d'allocations de String par passe de groupement.

## Détection N+1

### Algorithme

1. Grouper les spans par `(&EventType, &str template)`
2. Ignorer les groupes avec moins de `threshold` occurrences (défaut 5)
3. Compter les **jeux de paramètres distincts** via `HashSet<&[String]>`
4. Ignorer les groupes avec moins de `threshold` paramètres distincts (mêmes paramètres = redondant, pas N+1)
5. Calculer la fenêtre temporelle entre le plus ancien et le plus récent timestamp
6. Ignorer les groupes où la fenêtre dépasse `window_limit_ms` (défaut 500ms)
7. Assigner la sévérité : Critical si >= 10 occurrences, Warning sinon

### Paramètres distincts via slices empruntés

```rust
let distinct_params: HashSet<&[String]> = indices
    .iter()
    .map(|&i| trace.spans[i].params.as_slice())
    .collect();
```

Utiliser `&[String]` comme clé de HashSet est un choix de conception critique :
- **Pas d'allocation :** emprunte le Vec existant comme référence de slice
- **Pas de bug de collision :** compare directement le contenu complet du Vec, contrairement à une approche `join(",")` où `["a,b"]` et `["a", "b"]` produiraient la même chaîne jointe

La bibliothèque standard de Rust implémente `Hash` et `Eq` pour `&[T]` quand `T: Hash + Eq`, rendant cela à coût zéro.

### Calcul de fenêtre basé sur les itérateurs

```rust
pub fn compute_window_and_bounds_iter<'a>(
    mut iter: impl Iterator<Item = &'a str>,
) -> (u64, &'a str, &'a str) {
    let Some(first) = iter.next() else {
        return (0, "", "");
    };
    let mut min_ts = first;
    let mut max_ts = first;
    let mut has_second = false;
    for ts in iter {
        has_second = true;
        if ts < min_ts { min_ts = ts; }
        if ts > max_ts { max_ts = ts; }
    }
    // ...
}
```

**Pourquoi un itérateur au lieu de `&[&str]` ?** L'appelant devrait d'abord collecter les timestamps dans un Vec :

```rust
// Ancien (alloue) :
let timestamps: Vec<&str> = indices.iter().map(|&i| ...).collect();
let (w, min, max) = compute_window_and_bounds(&timestamps);

// Nouveau (zéro allocation) :
let (w, min, max) = compute_window_and_bounds_iter(
    indices.iter().map(|&i| trace.spans[i].event.timestamp.as_str())
);
```

La version basée sur les itérateurs élimine une allocation `Vec<&str>` par groupe de détection. Avec 3 détecteurs × plusieurs groupes par trace × milliers de traces, cela s'accumule.

Le booléen `has_second` remplace une variable `count` qui n'était utilisée que pour vérifier `count < 2`. Cela évite d'incrémenter un compteur à chaque itération.

### Parseur de timestamp ISO 8601

```rust
fn parse_timestamp_ms(ts: &str) -> Option<u64> {
    let time_part = ts.split('T').nth(1)?;
    let time_part = time_part.trim_end_matches('Z');
    let mut colon_parts = time_part.split(':');
    let hours: u64 = colon_parts.next()?.parse().ok()?;
    let minutes: u64 = colon_parts.next()?.parse().ok()?;
    let sec_str = colon_parts.next()?;
    // ... parser les secondes et la partie fractionnaire
}
```

**Pourquoi pas [chrono](https://docs.rs/chrono/) ?** chrono ajoute ~150 Ko au binaire et parse ~200ns par timestamp. Ce parseur artisanal gère le format fixe (`YYYY-MM-DDTHH:MM:SS.mmmZ`) en ~5ns en découpant sur des délimiteurs connus et en utilisant des appels itérateurs `.next()` au lieu de collecter dans des Vecs.

Le parseur utilise des itérateurs partout (`split(':')` -> `.next()`, `split('.')` -> `.next()`) pour éviter d'allouer des collections `Vec<&str>` intermédiaires.

Le parseur calcule les millisecondes depuis l'epoch Unix en parsant les composantes date (`YYYY-MM-DD`) et heure. La conversion date-vers-jours utilise l'[algorithme de Howard Hinnant](http://howardhinnant.github.io/date_algorithms.html) (domaine public), sans dépendance externe.

### Comparaison lexicographique des timestamps

Les timestamps min/max sont trouvés via comparaison de chaînes : `if ts < min_ts { min_ts = ts; }`. Cela fonctionne car les timestamps ISO 8601 avec des champs de largeur fixe (`2025-07-10T14:32:01.123Z`) se trient chronologiquement lorsqu'ils sont comparés lexicographiquement. C'est garanti par le [standard ISO 8601](https://www.iso.org/iso-8601-date-and-time-format.html), Section 5.3.3.

## Détection redondante

### Clés de slice empruntées

```rust
HashMap<(&EventType, &str, &[String]), Vec<usize>>
```

La clé en trois parties inclut le slice complet des paramètres, garantissant que deux spans avec le même template mais des paramètres différents sont dans des groupes différents. C'est le comportement correct : la détection redondante signale les **doublons exacts** (même template ET mêmes paramètres).

L'utilisation de `&[String]` au lieu de joindre les paramètres en une seule chaîne prévient un bug subtil de collision : `["a,b"]` (un paramètre contenant une virgule) et `["a", "b"]` (deux paramètres) produiraient la même clé jointe `"a,b"` mais sont des jeux de paramètres sémantiquement différents.

### Sévérité

- **Info** (< 5 occurrences) : courant pour les consultations de config, les health checks
- **Warning** (>= 5 occurrences) : probablement un bug de boucle ou un cache manquant

Le seuil de 2 (minimum pour signaler) attrape tout doublon exact. Contrairement au N+1 qui nécessite 5+ occurrences, même 2 requêtes identiques dans une seule requête sont suspectes et méritent d'être signalées au niveau Info.

### Paramètres bindés des ORM

Les ORM qui utilisent des paramètres nommés (Entity Framework Core avec `@__param_0`, Hibernate avec `?1`) produisent des spans SQL ou les valeurs réelles ne sont pas visibles dans `db.statement`/`db.query.text`. Dans ce cas, les patterns N+1 (même requête avec des valeurs différentes) apparaissent comme des requêtes redondantes (même template, mêmes params visibles), car perf-sentinel ne peut pas distinguer les valeurs bindées. Les deux findings identifient correctement le pattern de requêtes répétées. Les ORM qui injectent les valeurs littérales (SeaORM en requêtes brutes, JDBC sans prepared statements) permettent une classification précise N+1 vs redondant.

## Détection lente

### Arithmétique saturante

```rust
let threshold_us = threshold_ms.saturating_mul(1000);
// ...
if max_duration_us > threshold_us.saturating_mul(5) {
    Severity::Critical
}
```

[`saturating_mul`](https://doc.rust-lang.org/std/primitive.u64.html#method.saturating_mul) retourne `u64::MAX` en cas de dépassement au lieu de revenir à zéro. Cela empêche un `threshold_ms = u64::MAX` malveillant ou mal configuré de désactiver les seuils de sévérité.

### Ne fait pas partie du ratio de gaspillage

Les findings lents ont `green_impact.estimated_extra_io_ops = 0`. Ce sont des opérations **nécessaires** qui se trouvent être lentes : elles ont besoin d'optimisation (indexation, cache), pas d'élimination. Les inclure dans le ratio de gaspillage confondrait "I/O évitables" (N+1, redondant) avec "I/O lentes" (qui nécessitent une solution différente).

## Orchestration de la détection

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/detection_dark.svg">
  <img alt="Orchestration de la détection" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/detection.svg">
</picture>

```rust
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        findings.extend(detect_n_plus_one(trace, ...));
        findings.extend(detect_redundant(trace));
        findings.extend(detect_slow(trace, ...));
    }
    findings
}
```

Les quatre détecteurs s'exécutent séquentiellement sur chaque trace. Bien qu'ils pourraient théoriquement partager une seule passe de groupement, les types de clés diffèrent (`(&EventType, &str)` vs `(&EventType, &str, &[String])`) et les implémentations séparées sont plus claires et testables indépendamment. Avec des tailles de trace typiques de 10-50 spans, quatre passes O(n) sont négligeables.

## Détection de fanout

### Algorithme

1. Regrouper les spans par `parent_span_id`
2. Ignorer les groupes où le parent a `max_fanout` ou moins d'enfants (défaut 20)
3. Pour chaque parent dépassant le seuil, émettre un finding `ExcessiveFanout`
4. Sévérité : Warning si > `max_fanout`, Critical si > 3x `max_fanout`

### Pas dans le ratio de gaspillage

Comme les findings lents, les findings de fanout ont `green_impact.estimated_extra_io_ops = 0`. Le fanout excessif est un problème structurel qui nécessite une optimisation architecturale, pas une élimination d'I/O.

## Détection des services bavards

### Algorithme

1. Pour chaque trace, compter les spans de type `http_out`
2. Ignorer les traces avec moins de `chatty_service_min_calls` appels HTTP sortants (défaut 15)
3. Émettre un finding `chatty_service` avec le service et le nombre total d'appels
4. Sévérité : Warning si > seuil, Critical si > 3x seuil

### Pas dans le ratio de gaspillage

Les findings de services bavards ont `green_impact.estimated_extra_io_ops = 0`. Un service bavard est un problème architectural (granularité de décomposition des services) qui nécessite un redesign des API, pas une simple élimination d'I/O. Le compteur de gaspillage ne devrait refléter que les I/O qui peuvent être supprimées par refactoring local (batching, cache).

### Différence avec le fanout

Le fanout excessif détecte un **parent unique** avec trop d'enfants directs. Le service bavard détecte une **trace entière** avec trop d'appels HTTP sortants, indépendamment de la structure parent-enfant. Une trace peut déclencher les deux si un seul parent génère tous les appels ou seulement le service bavard si les appels sont répartis sur plusieurs parents.

## Détection de saturation du pool de connexions

### Algorithme

1. Regrouper les spans SQL par service
2. Pour chaque service, trier les spans par timestamp de début
3. Exécuter un algorithme de balayage (sweep line) : traiter chaque span comme un intervalle `[début, début + durée]`, suivre la concurrence maximale
4. Ignorer les services où la concurrence maximale est inférieure ou égale à `pool_saturation_concurrent_threshold` (défaut 10)
5. Émettre un finding `pool_saturation` avec le service et le pic de concurrence
6. Sévérité : Warning si > seuil, Critical si > 3x seuil

### Sweep line

L'algorithme de balayage crée deux événements par span : un événement d'ouverture au timestamp de début et un événement de fermeture au timestamp de fin (début + durée). Les événements sont triés chronologiquement. Un compteur est incrémenté à chaque ouverture et décrémenté à chaque fermeture. La valeur maximale atteinte par le compteur est la concurrence pic.

### Pas dans le ratio de gaspillage

Les findings de saturation du pool ont `green_impact.estimated_extra_io_ops = 0`. Elles signalent un risque de contention des ressources, pas des I/O évitables.

## Détection des appels sérialisés

### Algorithme

1. Grouper les spans frères par `parent_span_id`
2. Pour chaque groupe, trier les enfants par **temps de fin** (croissant)
3. Trouver la plus longue sous-séquence non chevauchante via programmation dynamique (Weighted Interval Scheduling avec poids unitaires)
4. Si la séquence optimale a >= `serialized_min_sequential` (défaut 3) spans avec des templates distincts, émettre un finding
5. Sévérité : toujours Info (heuristique, risque inhérent de faux positifs)

```
Entrée : trace avec N spans, groupés par parent_span_id
Sortie : 0 ou plusieurs findings SerializedCalls

pour chaque parent_id dans spans_par_parent :
    enfants = spans avec ce parent_id
    si len(enfants) < serialized_min_sequential :
        passer

    trier enfants par end_time croissant
    
    // Calcul des prédécesseurs : pour chaque span i, recherche binaire
    // de p(i), le span j (j < i) le plus à droite dont end_time <= start_time de i.
    // O(log n) par span.
    
    // Récurrence DP :
    //   dp[i] = max(dp[i-1], dp[p(i)] + 1)
    // où dp[i] = plus longue sous-séquence non chevauchante dans enfants[0..=i]
    
    // Backtrack depuis dp[n-1] pour reconstruire les spans sélectionnés.
    // Garde : le prédécesseur doit être strictement inférieur à l'index courant
    // pour garantir la terminaison sur des entrées dégénérées (spans de durée zéro).
    
    si len(sélectionnés) >= serialized_min_sequential
       ET templates_distincts(sélectionnés) > 1 :
        émettre finding pour la séquence sélectionnée
```

Complexité : O(n log n) pour le tri + O(n log n) pour toutes les recherches binaires + O(n) pour le remplissage DP et le backtrack = O(n log n) total par groupe parent. C'est le même coût asymptotique que l'approche gloutonne plus simple, mais la programmation dynamique garantit de trouver la plus longue séquence non chevauchante possible. Par exemple, avec les spans A:[0-200ms], B:[100-150ms], C:[160-300ms], D:[310-400ms], une approche gloutonne triée par temps de début sélectionnerait {A, D} (longueur 2), tandis que la DP identifie correctement {B, C, D} (longueur 3).

La recherche binaire utilise `partition_point` directement sur le slice trié, évitant une allocation séparée pour le tableau des prédécesseurs.

### Pourquoi `info` uniquement

Le détecteur ne peut pas observer les dépendances de données entre les appels. Deux appels séquentiels à des services différents peuvent être intentionnellement ordonnés (par exemple, créer un enregistrement puis notifier un service dépendant). La sévérité `info` signale une opportunité d'investigation, pas un défaut confirmé.

### Filtrage de template

Le détecteur ignore les séquences où tous les spans partagent le même template normalisé. Ce motif est un N+1 (même opération répétée avec des paramètres différents), pas une sérialisation. En exigeant des templates différents, le détecteur cible le pattern « récupérer l'utilisateur, puis ses commandes, puis ses préférences » où les appels sont indépendants et pourraient s'exécuter en parallèle.

### Estimation du gain de temps

Le finding inclut le gain de temps potentiel : `durée_séquentielle_totale - durée_individuelle_max`. Si 3 appels séquentiels prennent chacun 100 ms, les paralléliser pourrait réduire la latence de 300 ms à 100 ms, soit 200 ms économisées. C'est une estimation optimale qui suppose qu'il n'y a pas de contention sur des ressources partagées.

### Pas dans le ratio de gaspillage

Les findings d'appels sérialisés ont `green_impact.estimated_extra_io_ops = 0`. Paralléliser des appels séquentiels réduit la latence mais ne réduit pas le nombre total d'opérations I/O. Le ratio de gaspillage ne mesure que les I/O éliminables.

## Percentiles lents cross-trace

En mode batch, `detect_slow_cross_trace` collecte les spans lents à travers toutes les traces et calcule les percentiles p50/p95/p99 par template normalisé. Seuls les templates apparaissant dans au moins 2 traces distinctes sont rapportés.

## Orchestration de la détection (mise à jour)

```rust
pub fn detect(traces: &[Trace], config: &DetectConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    for trace in traces {
        findings.append(&mut detect_n_plus_one(trace, ...));
        findings.append(&mut detect_redundant(trace));
        findings.append(&mut detect_slow(trace, ...));
        findings.append(&mut detect_fanout(trace, config.max_fanout));
        findings.append(&mut detect_chatty(trace, config.chatty_service_min_calls));
        findings.append(&mut detect_pool_saturation(trace, config.pool_saturation_concurrent_threshold));
        findings.append(&mut detect_serialized(trace, config.serialized_min_sequential));
    }
    findings
}
```

Les sept détecteurs s'exécutent séquentiellement sur chaque trace. `append(&mut ...)` est utilisé à la place de `extend()` pour transférer les buffers en O(1) sans passer par un itérateur. L'analyse des percentiles lents cross-trace s'exécute séparément dans `pipeline.rs` après la détection par trace et avant le scoring.

## Corrélation temporelle cross-trace (mode daemon)

En mode `watch`, perf-sentinel observe l'ensemble des findings sur tous les traces au fil du temps. Le module `detect/correlate_cross.rs` fournit un moteur de corrélation qui identifie les co-occurrences récurrentes entre findings de services différents : par exemple, "chaque fois que le N+1 dans order-svc se déclenche, une saturation du pool apparaît dans payment-svc dans les 2 secondes."

### Structure du corrélateur

`CrossTraceCorrelator` est une struct possédée par la boucle événementielle du daemon. Elle maintient trois collections :

```rust
pub struct CrossTraceCorrelator {
    occurrences: VecDeque<FindingOccurrence>,
    pair_counts: HashMap<PairKey, PairState>,
    source_totals: HashMap<CorrelationEndpoint, u32>,
    config: CorrelationConfig,
}
```

- `occurrences` : fenêtre glissante des findings récents, stockée dans un VecDeque pour une éviction O(1) par l'avant.
- `pair_counts` : compteurs de co-occurrences par paire (source, cible). Chaque entrée contient le compteur, un reservoir borné de délais observés, un compteur `total_observations`, un état PRNG `SplitMix64` par paire et les timestamps first/last seen.
- `source_totals` : nombre d'occurrences par endpoint actuellement dans la fenêtre, utilisé comme dénominateur pour le score de confiance. Maintenu de manière incrémentale (incrémenté au `push_back`, décrémenté au `pop_front`). Les entrées sont supprimées quand le compteur atteint zéro, ce qui borne la map au nombre d'endpoints distincts plutôt qu'au nombre d'occurrences.

### Algorithme d'ingestion

La méthode `ingest()` est appelée à chaque tick du daemon avec le lot de findings courant. L'algorithme a cinq étapes :

1. **Eviction des entrées périmées.** Parcourir `occurrences` de l'avant vers l'arrière, retirer les entrées plus anciennes que `now_ms - window_ms` (défaut 10 min) et décrémenter `source_totals` pour chaque endpoint évincé. O(k) où k est le nombre d'entrées expirées.
2. **Nettoyage des paires obsolètes.** Une seule passe `HashMap::retain` sur `pair_counts` retire les paires dont `last_seen_ms` est hors de la fenêtre. O(pairs).
3. **Recherche de co-occurrences.** Pour chaque finding entrant, parcourir les occurrences en ordre inverse (plus récent en premier). Si une occurrence provient d'un service **différent** et que le délai ne dépasse pas `lag_threshold_ms` (défaut 5 000 ms), incrémenter le compteur de la paire et enregistrer le délai via reservoir sampling (voir ci-dessous). Le scan s'arrête tôt dès qu'on atteint des entrées au-delà du seuil de délai. O(l) où l est le nombre d'occurrences dans la fenêtre de délai.
4. **Ajout à la fenêtre.** Ajouter le finding aux occurrences et incrémenter son compteur dans `source_totals`.
5. **Application du cap mémoire.** Si `pair_counts` dépasse `max_tracked_pairs` (défaut 10 000), utiliser `select_nth_unstable_by_key` (O(n) en moyenne) pour trouver les paires avec le compteur le plus bas et les évincer jusqu'à respecter le cap.

### Score de confiance

```
confidence = co_occurrence_count / source_total_occurrences
```

Une paire n'est rapportée que si `co_occurrence_count >= min_co_occurrences` (défaut 5) et `confidence >= min_confidence` (défaut 0.7).

### Reservoir sampling pour les délais

Une paire chaude qui se déclenche des milliers de fois dans la fenêtre ferait sinon croître `lags_ms` sans borne (mégaoctets par paire). Pour garder la mémoire par paire constante, `record_lag` utilise l'algorithme R de reservoir sampling plafonné à `MAX_LAG_SAMPLES = 256` :

- Tant que le reservoir a de la place, append inconditionnel.
- Une fois plein, tirer `r` uniformément dans `[0, total_observations)` via `SplitMix64`. Si `r < MAX_LAG_SAMPLES`, remplacer `lags_ms[r]`. Conditionnellement à `r < k`, `r` est lui-même uniforme dans `[0, k)`, donc le choix du slot est non-biaisé sans tirage PRNG supplémentaire.

Le PRNG est un état `SplitMix64` par `PairState`, seedé à la construction depuis `now_ms ^ (hash_endpoint(source) << 17) ^ hash_endpoint(target)`. `hash_endpoint` est un FNV-1a déterministe sur les champs `finding_type`, `service` et `template` de l'endpoint (PAS le `DefaultHasher` qui utilise un `RandomState` par process et rendrait le corrélateur non-déterministe entre runs). Deux runs du daemon rejouant le même fichier de traces produisent des samples reservoir identiques et donc des médianes identiques.

### Calcul de la médiane

Le helper `median()` trie un clone des valeurs de délai et retourne l'élément médian (longueur impaire) ou la moyenne des deux médians (longueur paire). Le tri est borné par `MAX_LAG_SAMPLES` grâce au reservoir, donc le calcul de la médiane est O(k log k) avec k = 256 quelle que soit la fréquence de la paire.

### Identifiant de chaque extrémité

Chaque côté d'une paire est identifié par un `CorrelationEndpoint` :

```rust
pub struct CorrelationEndpoint {
    pub finding_type: FindingType,
    pub service: String,
    pub template: String,
}
```

Cela signifie que deux N+1 sur le même service mais avec des templates différents sont traités comme des endpoints distincts.

### Cap mémoire

Plusieurs mécanismes bornent l'usage mémoire :

- **Eviction de la fenêtre glissante** : `occurrences` est nettoyé à chaque `ingest()`. Les entrées plus anciennes que `window_ms` sont supprimées et leur compteur dans `source_totals` est décrémenté (entrée retirée si elle atteint zéro).
- **Nettoyage de pair_counts** : les paires dont `last_seen_ms` est hors de la fenêtre sont retirées.
- **Cap reservoir** : chaque `PairState.lags_ms` est borné à `MAX_LAG_SAMPLES = 256` f64 (~2 KB par paire), quelle que soit la fréquence de la paire.
- **Cap pairs avec éviction des plus faibles** : quand `pair_counts.len()` dépasse `max_tracked_pairs`, les paires les moins significatives (compteur le plus bas) sont évincées via `select_nth_unstable_by_key`.

### Configuration

```toml
[daemon.correlation]
enabled = true
window_ms = 600000
lag_threshold_ms = 5000
min_co_occurrences = 5
min_confidence = 0.7
max_tracked_pairs = 10000
```

L'option `enabled` (défaut false) active la corrélation. Les résultats sont exposés via `GET /api/correlations` et dans la sortie NDJSON du daemon.

## Corrections actionnables (suggestions framework-aware)

À partir de v0.4.2, un champ `suggested_fix: Option<SuggestedFix>` sur `Finding` porte une remédiation spécifique au framework qui va au-delà de la chaîne générique `suggestion`. Ce champ est peuplé par `detect::suggestions::enrich` après que les détecteurs per-trace aient retourné, à l'intérieur de `detect()`. La première version livrait Java/JPA uniquement. L'état actuel couvre Java (JPA, WebFlux, Quarkus reactive, Quarkus non-réactif, Helidon SE, Helidon MP), C# (.NET 8 à 10 avec EF Core / Pomelo MySQL) et Rust (Diesel, SeaORM), avec un fallback générique par langage pour le fan-out HTTP et le caching request-scoped.

### Structure `SuggestedFix`

```rust
pub struct SuggestedFix {
    pub pattern: String,          // "n_plus_one_sql" miroir du finding.type parent
    pub framework: String,        // "java_jpa" ou "java_generic"
    pub recommendation: String,   // phrase courte et impérative
    pub reference_url: Option<String>,
}
```

Sérialisé en JSON comme objet imbriqué sous `finding.suggested_fix`, omis quand absent. Émis en SARIF sous `result.fixes[0].description.text` (forme description-only de l'objet fix SARIF 2.1.0). La CLI l'affiche comme ligne imbriquée `Suggested fix:` juste après la ligne générique `Suggestion:`.

### Détecteur de framework

Le détecteur est une fonction pure qui lit uniquement `finding.code_location` (déjà peuplé par chaque détecteur depuis les attributs OTel `code.*` du span). Pas d'accès au niveau span, pas d'allocation supplémentaire. Chaîne de décision :

1. Pas de `code_location` ou pas de `filepath` → retourne `None`.
2. Mapper l'extension du fichier vers un langage : `.java` → Java, `.cs` → C#, `.rs` → Rust. Tout le reste → retourne `None`.
3. Parcourir les règles du langage dans l'ordre déclaré (plus spécifique en premier). Retourne le premier framework dont le hint namespace matche.
4. Fallback sur le framework générique du langage (`JavaGeneric`, `CsharpGeneric`, `RustGeneric`) quand aucune règle ne matche.

Le match namespace est segment-boundary-aware des **deux côtés** : le hint doit commencer à la racine du namespace ou juste après un séparateur et doit se terminer à la fin du namespace ou juste avant un autre séparateur. Les caractères de séparation sont `.` (Java, C#) et `::` (Rust). Exemples :

- `diesel::` matche `diesel::query_dsl::FilterDsl` et `crate::diesel::reexport` mais **pas** `crate::mydiesel::query` (la boundary de tête protège le code utilisateur qui contient le hint).
- `io.helidon` matche `io.helidon.webserver.Routing` mais **pas** `io.helidongrpc.Foo` (la boundary de fin protège les paquets utilisateur dont le premier segment commence simplement par le hint).
- `Microsoft.EntityFrameworkCore` matche `Microsoft.EntityFrameworkCore.Query` mais **pas** `Microsoft.EntityFrameworkCoreCache.Provider`.

### Règles par langage

L'ordre compte au sein d'un langage : le premier framework qui matche gagne. Les hints JPA passent intentionnellement après ceux de Quarkus reactive parce que `org.hibernate.reactive` contient `org.hibernate`.

**Java (`JAVA_RULES`) :**

| Framework                | Hints namespace                                                                                                                       |
|--------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| `JavaHelidonMp`          | `io.helidon.microprofile`                                                                                                             |
| `JavaHelidonSe`          | `io.helidon`                                                                                                                          |
| `JavaQuarkusReactive`    | `io.quarkus.hibernate.reactive`, `io.quarkus.panache.reactive`, `io.quarkus.reactive`, `org.hibernate.reactive`, `io.smallrye.mutiny` |
| `JavaQuarkus`            | `io.quarkus.hibernate.orm`, `io.quarkus.panache.common`, `io.quarkus`                                                                 |
| `JavaWebFlux`            | `org.springframework.web.reactive`, `reactor.core`                                                                                    |
| `JavaJpa`                | `jakarta.persistence`, `javax.persistence`, `org.hibernate`, `org.springframework.data.jpa`                                           |
| `JavaGeneric` (fallback) | (tout fichier `.java` sans les hints ci-dessus)                                                                                       |

`JavaQuarkusReactive` énumère explicitement ses sous-packages réactifs. Le catch-all `io.quarkus` appartient à `JavaQuarkus` (non-réactif), donc tout namespace Quarkus réactif doit matcher l'un des hints réactifs plus spécifiques en premier. Helidon ne chevauche aucun autre framework.

**C# (`CSHARP_RULES`) :**

| Framework                  | Hints namespace                                               |
|----------------------------|---------------------------------------------------------------|
| `CsharpEfCore`             | `Microsoft.EntityFrameworkCore`, `Pomelo.EntityFrameworkCore` |
| `CsharpGeneric` (fallback) | (tout fichier `.cs` sans les hints ci-dessus)                 |

**Rust (`RUST_RULES`) :**

| Framework                | Hints namespace                               |
|--------------------------|-----------------------------------------------|
| `RustDiesel`             | `diesel::`                                    |
| `RustSeaOrm`             | `sea_orm::`                                   |
| `RustGeneric` (fallback) | (tout fichier `.rs` sans les hints ci-dessus) |

### Table de mapping

Un static `LazyLock<HashMap<(FindingType, Framework), SuggestedFix>>`. Les lookups absents de la table laissent `suggested_fix` à `None`. Entrées actuelles :

| Type de finding | Framework             | Ancre de la recommandation                                                                                     |
|-----------------|-----------------------|----------------------------------------------------------------------------------------------------------------|
| `NPlusOneSql`   | `JavaJpa`             | `JOIN FETCH` ou `@EntityGraph`, Hibernate User Guide                                                           |
| `NPlusOneSql`   | `JavaQuarkusReactive` | Mutiny `Session.fetch()` + `@NamedEntityGraph`, guide Quarkus Hibernate Reactive                               |
| `NPlusOneSql`   | `JavaQuarkus`         | JPQL/Panache `JOIN FETCH`, `@EntityGraph` ou `Session.fetchProfile`, guide Quarkus Hibernate ORM               |
| `NPlusOneSql`   | `JavaHelidonSe`       | Requête nommée Helidon `DbClient` avec JOIN ou binding JDBC `:ids`                                             |
| `NPlusOneSql`   | `JavaHelidonMp`       | JPA `@EntityGraph` ou JPQL `JOIN FETCH` (les entités MP sont gérées par JPA via Hibernate)                     |
| `NPlusOneHttp`  | `JavaWebFlux`         | `Flux.merge()` / `Flux.zip()` pour le parallélisme ou endpoint batch                                           |
| `NPlusOneHttp`  | `JavaQuarkusReactive` | `Uni.combine().all().unis(...)` pour le parallélisme, guide Mutiny combining                                   |
| `NPlusOneHttp`  | `JavaQuarkus`         | `CompletableFuture.allOf` sur `ManagedExecutor`, batch via Quarkus REST Client                                 |
| `NPlusOneHttp`  | `JavaHelidonSe`       | Helidon SE `WebClient` + `Single.zip` / `Multi.merge` pour le parallélisme ou endpoint batch                   |
| `NPlusOneHttp`  | `JavaHelidonMp`       | MicroProfile Rest Client + `CompletableFuture.allOf` sur l'executor `@ManagedExecutorConfig` ou endpoint batch |
| `NPlusOneHttp`  | `JavaGeneric`         | Endpoint batch ou `@Cacheable` request-scoped                                                                  |
| `RedundantSql`  | `JavaQuarkusReactive` | `@CacheResult` ou `Uni.memoize().indefinitely()`                                                               |
| `RedundantSql`  | `JavaQuarkus`         | `@CacheResult` (extension cache Quarkus) ou déduplication HashMap `@RequestScoped`                             |
| `RedundantSql`  | `JavaGeneric`         | Cache service-level (Caffeine, Spring Cache)                                                                   |
| `NPlusOneSql`   | `CsharpEfCore`        | `.Include()` / `.ThenInclude()`, `.AsSplitQuery()` pour l'explosion cartésienne                                |
| `RedundantSql`  | `CsharpEfCore`        | `IMemoryCache`, DbContext scopé pour le short-circuit per-request                                              |
| `NPlusOneHttp`  | `CsharpGeneric`       | `Task.WhenAll` pour les appels parallèles, endpoint batch, response caching `HttpClient`                       |
| `NPlusOneSql`   | `RustDiesel`          | `belonging_to` + `grouped_by` ou `.inner_join` / `.left_join` pour une seule query                             |
| `NPlusOneSql`   | `RustSeaOrm`          | `find_with_related` / `find_also_related` ou `QuerySelect::join`                                               |
| `RedundantSql`  | `RustDiesel`          | Cache `moka` ou `OnceCell` request-local                                                                       |
| `RedundantSql`  | `RustSeaOrm`          | Cache `moka` ou `OnceCell` request-local                                                                       |
| `NPlusOneHttp`  | `RustGeneric`         | `tokio::join!` / `futures::future::join_all` pour le parallélisme ou endpoint batch                            |

### Chemin d'extension pour les contributeurs

Pour ajouter un nouveau framework :

1. Étendre l'enum privé `Framework` dans `detect/suggestions.rs`.
2. Choisir un langage et ajouter une entrée `(Framework, &[hint])` au slice de règles de ce langage. Placer les frameworks plus spécifiques avant les moins spécifiques.
3. Ajouter des entrées à la static `FIXES` pour chaque paire `(FindingType, Framework)` à mapper.
4. Ajouter des tests unitaires sous le module `tests` du même fichier.

Pour ajouter un nouveau langage :

1. Étendre l'enum `Language` et ses méthodes `rules()` / `generic()`.
2. Ajouter le match d'extension de fichier dans `language_from_filepath`.
3. Définir un nouveau slice `*_RULES` et une variante générique fallback sur `Framework`.

Aucun changement de câblage ailleurs : l'orchestrateur `detect()` appelle déjà `suggestions::enrich` à la fin de la passe de détection per-trace et les rendus CLI / JSON / SARIF gèrent déjà un `suggested_fix` optionnel.
