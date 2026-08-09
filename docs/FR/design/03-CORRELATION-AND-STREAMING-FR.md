# Corrélation et streaming

La corrélation regroupe les événements normalisés par `trace_id` pour former des objets `Trace` destinés à la détection. Deux implémentations existent : une pour le mode batch et une pour le mode streaming (daemon).

## Corrélation batch

### Pattern manuel `get_mut` / `insert`

Le corrélateur batch utilise un pattern délibéré au lieu de l'API `HashMap::entry` :

```rust
if let Some(vec) = map.get_mut(event.event.trace_id.as_str()) {
    vec.push(event);
} else {
    let key = event.event.trace_id.clone();
    map.insert(key, vec![event]);
}
```

**Pourquoi pas `entry()` ?** L'API `entry()` nécessite une clé possédée d'emblée car elle doit stocker la clé si l'entrée est vacante. Cela signifierait cloner `trace_id` pour **chaque** événement, même quand la trace existe déjà (le cas courant). Le pattern manuel ne clone que sur le chemin lent (nouvelle trace). Pour une trace avec 50 événements, cela économise 49 clones de String inutiles.

C'est un pattern d'optimisation Rust bien connu documenté dans le [Rust Performance Book](https://nnethercote.github.io/perf-book/hashing.html).

### Indication de capacité

```rust
HashMap::with_capacity(events.len() / 10 + 1)
```

L'heuristique suppose ~10 événements par trace en moyenne. Le `+ 1` empêche une map de capacité zéro quand `events.len() < 10`. Surestimer est peu coûteux (quelques centaines d'octets d'espace de buckets inutilisé), sous-estimer déclenche un rehashing.

## Corrélation streaming : TraceWindow

Le daemon utilise un `TraceWindow` qui combine trois structures de données :

1. **Cache LRU** : borne le nombre total de traces actives
2. **Buffer circulaire** (VecDeque) : borne les événements par trace
3. **Éviction TTL** : expire les traces inactives

### Cache LRU

Le crate [`lru`](https://docs.rs/lru/) fournit un cache LRU O(1) amorti soutenu par une liste doublement chaînée + HashMap. Opérations :

| Opération          | Complexité | Notes                              |
|--------------------|------------|------------------------------------|
| `get_mut(key)`     | O(1)       | Promeut automatiquement en MRU     |
| `push(key, value)` | O(1)       | Évince le LRU si à capacité        |
| `pop_lru()`        | O(1)       | Supprime l'entrée la plus ancienne |
| `peek_lru()`       | O(1)       | Inspecte sans promouvoir           |

La capacité du cache utilise `NonZeroUsize` comme requis par l'API du crate `lru`. La méthode `Config::validate()` rejette `max_active_traces = 0`, donc le `expect("max_active_traces must be >= 1")` dans `TraceWindow::new()` est inaccessible pour les configurations valides.

### Buffer circulaire par trace

Chaque trace stocke ses événements dans un `VecDeque<NormalizedEvent>` :

```rust
struct TraceBuffer {
    events: VecDeque<NormalizedEvent>,
    source_endpoint_groups: HashMap<Arc<str>, HashMap<String, String>>,
    source_endpoint_parent_groups: HashMap<Arc<str>, HashMap<String, Option<String>>>,
    source_endpoint_count: usize,
    resolved_ancestry: Option<LruCache<(Arc<str>, String), AncestryEntry>>,
    resolved_ancestry_cap: usize,
    last_seen_ms: u64,
}
```

Quand une trace dépasse `max_events_per_trace`, l'événement le plus ancien est supprimé :

```rust
if buf.events.len() > self.config.max_events_per_trace {
    buf.events.pop_front();
}
```

**Pourquoi `VecDeque` ?** `Vec::remove(0)` est O(n) car il décale tous les éléments. `VecDeque::pop_front()` est O(1) car il est soutenu par un buffer circulaire. Pour les traces avec un grand nombre d'événements atteignant fréquemment le cap, cela évite une dégradation en O(n^2).

La capacité initiale est `VecDeque::with_capacity(8)` : une petite allocation pour les traces de courte durée qui évite les doublements répétés pour le cas courant de 1-10 événements.

Une trace OTLP peut aussi conserver les contextes d'endpoint entrant avant que
ses événements I/O n'arrivent dans un lot ultérieur. Chaque contexte porte son
lien parent, et les spans intermédiaires non-I/O entrent dans le même LRU
d'ascendance qui conserve le lien parent d'un événement et sa route prouvée
facultative après rotation. Cela répare l'arrivée parent-après-enfant et les
exports scindés par une recherche de huit sauts exactement, sans événement ni
métrique I/O synthétique. Les événements, contextes d'endpoint et entrées
d'ascendance forment trois collections distinctes, **chacune** plafonnée par
`max_events_per_trace`. Le LRU d'ascendance alloue sa
mémoire progressivement au lieu de réserver le plafond configuré pour chaque
trace. Au plafond minimal valide de un, la rotation peut remplacer l'unique
entrée parent ; une chaîne manquante n'utilise alors un fallback que si le
service possède exactement une racine conservée et qu'aucune racine distincte
n'a été observée pour ce service. Une seconde racine distincte marque le service
conservé comme ambigu même si le plafond rejette ce contexte ; répéter une mise
à jour de la même racine ne le fait pas. L'ensemble d'ambiguïté est limité aux
services qui ont une racine conservée. Les services multi-racines et les chaînes
ayant épuisé la profondeur restent inconnus. Ce fallback mono-racine n'est
jamais persisté dans l'événement actif ni dans l'état d'ascendance : il est
appliqué uniquement au résultat de `peek_clone` ou à une trace détachée lors de
sa finalisation. Une racine distincte tardive peut ainsi rétracter l'aperçu
provisoire sans réécrire les événements actifs. La réconciliation explicite
fusionne d'abord les racines dans cet état autoritatif et borné avant de résoudre
les événements. Les traces ne contenant que du contexte utilisent le même LRU
`max_active_traces` et la même éviction `trace_ttl_ms` que les traces avec
événements ; les routes précoces et l'ascendance intermédiaire ne peuvent donc
pas créer un état non borné. Seuls les identifiants OTLP valides des traces
conservées par l'échantillonneur déterministe du daemon entrent dans cet état.

Dans un lot d'ingestion, chaque groupe de racines conservé reçoit une génération
monotone de la fenêtre. Les événements comparent cette génération en O(1) et ne
réappliquent le groupe qu'après une éviction ou un remplacement réel du
contexte. Un groupe dépassant le plafond n'est donc tenté qu'une fois au lieu
d'être rescanné pour chaque événement.

### Éviction TTL

Les traces n'ayant pas reçu d'événements dans le délai `trace_ttl_ms` sont expirées :

```rust
pub fn evict_expired(&mut self, now_ms: u64) -> Vec<(String, Vec<NormalizedEvent>)> {
    let expired_keys: Vec<String> = self.traces.iter()
        .filter(|(_, buf)| now_ms.saturating_sub(buf.last_seen_ms) > ttl)
        .map(|(id, _)| id.clone())
        .collect();
    for key in expired_keys {
        self.traces.pop_entry(&key);
        // ... collecter la trace évincée
    }
}
```

**Scan complet au lieu d'arrêt anticipé :** les ajustements d'horloge (NTP) peuvent faire diverger `last_seen_ms` et la position LRU, laissant des traces expirées derrière des entrées non expirées. Un scan complet du cache garantit que toutes les traces expirées sont évincées quel que soit l'ordre. Le cache est borné par `max_active_traces` (défaut 10k, max 1M), donc le coût du scan est négligeable par rapport à la détection et au scoring.

**`saturating_sub`** empêche le dépassement par le bas si `now_ms < last_seen_ms` (possible avec une dérive d'horloge ou des ajustements NTP).

### Deux méthodes d'éviction

- **`evict()`** : supprime silencieusement les traces expirées (utilisé si l'appelant n'a pas besoin des données)
- **`evict_expired()`** : retourne les traces expirées pour que le daemon puisse exécuter la détection avant de les supprimer

Le daemon utilise toujours `evict_expired()` pour garantir qu'aucune donnée de trace n'est perdue sans analyse.

### `Vec::from(VecDeque)` pour l'éviction

Lors de la conversion des événements de trace évincés de `VecDeque` vers `Vec` :

```rust
.map(|(id, buf)| (id, Vec::from(buf.events)))
```

`Vec::from(VecDeque)` est spécialisé dans la bibliothèque standard pour réutiliser la portion contiguë du buffer circulaire quand c'est possible, évitant les déplacements élément par élément. C'est plus efficace que `.into_iter().collect()` qui alloue toujours un nouveau Vec.

### Budget mémoire

La consommation mémoire maximale du TraceWindow peut être estimée :

```
mémoire_max = max_active_traces × max_events_per_trace
              × (taille_moyenne_événement + taille_moyenne_contexte_endpoint
                 + taille_moyenne_entrée_ascendance)
```

Les trois collections par trace peuvent chacune atteindre leur plafond. Avec les
valeurs par défaut, la seule partie événements représente environ 5 Go au
maximum théorique (10 000 × 1 000 × ~500 octets), auxquels s'ajoutent les
contextes d'endpoint (route plus table des parents) et entrées d'ascendance
bornés séparément.

En pratique, la plupart des traces ont bien moins d'événements que le cap. Pour
des traces typiques de 10 à 50 événements, la seule partie événements vaut
environ :

```
mémoire_typique = 10 000 × 50 × ~500 octets = ~250 Mo
```

Les contextes d'endpoint et les entrées d'ascendance allouées progressivement
s'ajoutent à cette estimation limitée aux événements ; le plafond configuré de
1 000 entrées d'ascendance n'est pas préalloué pour chaque trace.

La validation de la config plafonne `max_active_traces` à 1 000 000 et `max_events_per_trace` à 100 000 pour éviter les erreurs de configuration accidentelles.
