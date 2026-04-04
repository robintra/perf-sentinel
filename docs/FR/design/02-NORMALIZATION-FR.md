# Normalisation : SQL et HTTP

La normalisation est la deuxième étape du pipeline. Elle transforme les `SpanEvent` bruts en `NormalizedEvent` en extrayant un template (requête paramétrée ou pattern d'URL) et les valeurs concrètes des paramètres.

## Pourquoi ne pas utiliser `sqlparser` ?

Le crate [sqlparser](https://docs.rs/sqlparser/) est un parseur SQL complet qui construit un AST. Nous avons délibérément choisi un tokenizer maison à la place :

- **Taille du binaire :** sqlparser ajoute ~300 Ko au binaire release. perf-sentinel cible < 10 Mo au total.
- **Poids des dépendances :** sqlparser amène des crates supplémentaires et augmente le temps de compilation.
- **Agnostique du dialecte :** sqlparser nécessite de spécifier un dialecte SQL (PostgreSQL, MySQL, etc.). Notre tokenizer fonctionne avec tous les dialectes car il ne remplace que les littéraux : il n'a jamais besoin de comprendre la structure de la requête.
- **Performance :** un parseur complet construit un AST que nous jetterions immédiatement. Notre tokenizer en une seule passe traite l'entrée en O(n) sans structure de données intermédiaire.
- **Simplicité :** 120 lignes de code vs une dépendance de 50 000+ lignes.

Le compromis est documenté dans [LIMITATIONS-FR.md](../LIMITATIONS-FR.md) : le tokenizer ne gère que le SQL ASCII et ne réalise pas d'analyse sémantique. Il supporte les CTEs, les identifiants double-quoted, les chaînes dollar-quoted PostgreSQL et les instructions `CALL`.

## Tokenizer SQL : machine à états en une seule passe

`normalize_sql()` traite la requête octet par octet à travers trois états :

| État           | Déclencheur (entrée)          | Action                               | Déclencheur (sortie)                     |
|----------------|-------------------------------|--------------------------------------|------------------------------------------|
| **Normal**     | Défaut / fin de littéral      | Accumule dans le template            | Guillemet `'` ou chiffre isolé           |
| **InString**   | Guillemet ouvrant `'`         | Accumule dans `current_value`        | Guillemet fermant `'` (pas `''`)         |
| **InNumber**   | Chiffre isolé                 | Accumule chiffres/point              | Non-chiffre ou deuxième point            |

### Optimisation batch `push_str`

Au lieu de pousser les caractères un par un avec `template.push(b as char)`, le tokenizer suit un index `normal_start` :

```rust
// À l'entrée dans InString ou InNumber :
if i > normal_start {
    template.push_str(&query[normal_start..i]);
}
// Au retour en Normal :
normal_start = i;
// À la fin de l'entrée (toujours en Normal) :
template.push_str(&query[normal_start..len]);
```

Cela regroupe les séquences contiguës en état Normal en un seul appel `push_str`. Pour une requête typique comme `SELECT * FROM player WHERE game_id = 42`, le préfixe `SELECT * FROM player WHERE game_id = ` est envoyé en un seul appel au lieu de 38 appels `push` individuels.

L'[implémentation de `String::push_str` en Rust](https://doc.rust-lang.org/src/alloc/string.rs.html) copie les octets avec `memcpy`, ce qui est significativement plus rapide que des appels `push` répétés qui vérifient chacun la capacité et réallouent potentiellement.

### Saut de regex pour les listes IN

La plupart des requêtes SQL ne contiennent pas de clauses `IN (...)`. Le tokenizer suit si le mot-clé `IN` apparaît :

```rust
if !has_in_list
    && (b == b'I' || b == b'i')
    && i + 1 < len
    && (bytes[i + 1] == b'N' || bytes[i + 1] == b'n')
    && (i == 0 || bytes[i - 1].is_ascii_whitespace())
    && (i + 2 >= len || !bytes[i + 2].is_ascii_alphanumeric())
{
    has_in_list = true;
}
```

Si `has_in_list` est faux après la boucle principale, la passe regex post-traitement (`IN_LIST_RE.replace_all`) est entièrement sautée. Cela évite ~2us de surcoût regex sur les ~80% de requêtes qui n'ont pas de clause IN.

### Optimisation `Cow::Borrowed`

Quand la regex s'exécute mais ne fait aucun remplacement (ex. `IN (?)` est déjà réduit), `Regex::replace_all` retourne `Cow::Borrowed`. Le code vérifie cela :

```rust
let template = if has_in_list {
    match IN_LIST_RE.replace_all(&template, "IN (?)") {
        Cow::Borrowed(_) => template,    // pas d'allocation
        Cow::Owned(s) => s,              // une allocation
    }
} else {
    template                              // pas de regex du tout
};
```

Cette approche à trois niveaux garantit zéro allocation inutile.

### `LazyLock` pour la regex

La regex `IN_LIST_RE` est compilée une seule fois via [`std::sync::LazyLock`](https://doc.rust-lang.org/std/sync/struct.LazyLock.html) (stable depuis Rust 1.80) :

```rust
static IN_LIST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)IN\s*\(\s*\?(?:\s*,\s*\?)*\s*\)").unwrap()
});
```

`LazyLock` est préféré à la macro `lazy_static!` car il est dans `std` : aucune dépendance externe nécessaire.

### Autres micro-optimisations

- **`String::with_capacity(query.len())`** : pré-alloue le template pour éviter la réallocation dans le cas courant où le template est légèrement plus court que l'entrée.
- **`std::mem::take(&mut current_value)`** : déplace la valeur littérale accumulée dans `params` sans cloner, remplaçant `current_value` par un `String` vide sur place. C'est un transfert de propriété à coût zéro.
- **`is_identifier_byte_before()`** : vérifie si l'octet avant un chiffre est alphanumérique ou underscore, empêchant les chiffres dans les identifiants (`player2`, `col_1`) d'être mal interprétés comme des littéraux numériques.

## Normaliseur HTTP

### Vérification UUID codée à la main

Le normaliseur HTTP remplace les segments de chemin UUID par `{uuid}`. Au lieu d'utiliser une regex, la vérification est codée à la main :

```rust
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 { return false; }
    let b = s.as_bytes();
    b[8] == b'-' && b[13] == b'-' && b[18] == b'-' && b[23] == b'-'
        && b.iter().enumerate().all(|(i, &c)| {
            matches!(i, 8 | 13 | 18 | 23) || c.is_ascii_hexdigit()
        })
}
```

**Pourquoi codé à la main ?** Cette fonction est appelée sur chaque segment de chemin de chaque URL HTTP dans le pipeline. Une regex compilée (`Regex::is_match`) prend ~150ns par appel à cause du surcoût du moteur regex. La vérification codée à la main prend ~3ns : une vérification de longueur (rejet rapide pour >99% des segments), quatre comparaisons d'octets pour les positions des tirets, et une seule passe pour les chiffres hexadécimaux.

À 100 000 événements/sec avec une moyenne de 4 segments de chemin par URL, cela économise ~60ms/sec de surcoût regex.

### `strip_origin` sans bibliothèque URL

```rust
fn strip_origin(target: &str) -> &str {
    target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
        .map_or(target, |rest| rest.find('/').map_or("/", |idx| &rest[idx..]))
}
```

Cela extrait le chemin d'une URL complète sans inclure le crate [url](https://docs.rs/url/) (~50 Ko de surcoût binaire). Gère `http://`, `https://` et les chemins nus (`/api/foo`). Le `find('/')` localise le début du chemin après l'autorité.

### Limite de paramètres de requête

Les paramètres de requête sont retirés du template URL et collectés dans `params`. La collection est plafonnée à 100 paramètres via `.take(100)` pour prévenir les allocations mémoire illimitées depuis des URLs avec des query strings adverses. Les paramètres de requête ne faisant pas partie du template normalisé, les paramètres au-delà de 100 ne sont simplement pas extraits.

### Pré-allocation

```rust
let mut result = String::with_capacity(path.len() + 8);
```

Le `+ 8` tient compte du plus long remplacement (`{uuid}` = 6 caractères, remplaçant un UUID de 36 caractères). Cela évite la réallocation dans le cas courant où les remplacements raccourcissent le chemin.

## Dispatcher de normalisation

La fonction `normalize()` redirige vers le normaliseur SQL ou HTTP selon `event_type` :

```rust
pub fn normalize(event: SpanEvent) -> NormalizedEvent {
    match event.event_type {
        EventType::Sql => { /* sql::normalize_sql(...) */ }
        EventType::HttpOut => { /* http::normalize_http(...) */ }
    }
}
```

`normalize_all()` est un simple `events.into_iter().map(normalize).collect()`. Le `into_iter()` consomme le vecteur d'entrée, et chaque `SpanEvent` est déplacé (pas cloné) dans le normaliseur.
