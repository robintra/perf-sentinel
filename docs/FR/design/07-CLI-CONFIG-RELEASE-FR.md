# CLI, configuration et profil release

## Conception du CLI

Le CLI (`sentinel-cli`) est intentionnellement léger. Il parse les arguments avec [clap](https://docs.rs/clap/) et délègue aux fonctions de `sentinel-core`. Sept sous-commandes sont disponibles : `analyze`, `explain`, `watch`, `demo`, `bench`, `pg-stat` et `inspect`.

### Analyze : rapport coloré par défaut, JSON avec `--ci`

`perf-sentinel analyze` affiche un rapport coloré dans le terminal en mode interactif (sans `--ci`). C'est la sortie que les humains voient en utilisant l'outil localement. Avec `--ci`, la sortie passe en JSON structuré pour la consommation par les machines, et le processus sort avec le code 1 si le quality gate échoue.

Cela suit la convention d'outils comme `cargo test` (sortie colorée par défaut, `--format json` pour le CI).

Le flag `--format` offre un contrôle explicite sur le format de sortie : `text` (terminal coloré, défaut), `json` (rapport structuré) ou `sarif` (SARIF v2.1.0 pour le code scanning). Avec `--ci` sans `--format`, la sortie est en JSON par défaut pour la rétrocompatibilité.

### Explain : vue arborescente par trace

`perf-sentinel explain --input FILE --trace-id ID` construit un arbre à partir des relations `parent_span_id` et annote les findings en ligne. Il exécute uniquement les détecteurs par trace (N+1, redondant, lent, fanout) ; les findings cross-trace ne sont pas inclus.

### Bench : lots pré-clonés

```rust
let batches: Vec<Vec<SpanEvent>> = (0..iterations)
    .map(|_| events.clone())
    .collect();
```

Les lots d'entrée sont clonés **avant** le début de la mesure. Cela garantit que le benchmark ne mesure que la performance de `pipeline::analyze`, pas le surcoût de `Vec<SpanEvent>::clone`. Puisque `analyze` consomme son entrée (`Vec<SpanEvent>` est déplacé), chaque itération a besoin de sa propre copie.

### Calcul des percentiles

```rust
per_event_ns.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
let p50_idx = ((per_event_ns.len() as f64 * 0.50).ceil() as usize).saturating_sub(1);
let p99_idx = ((per_event_ns.len() as f64 * 0.99).ceil() as usize).min(per_event_ns.len() - 1);
```

Le calcul d'index basé sur le plafond suit la [méthode du rang le plus proche](https://en.wikipedia.org/wiki/Percentile#The_nearest-rank_method) pour les percentiles. Le `.saturating_sub(1)` convertit du rang basé sur 1 vers l'index basé sur 0. Le `.min(len - 1)` empêche l'accès hors limites quand `ceil` arrondit à `len`.

### Débit à partir des nanosecondes

```rust
let elapsed_nanos: u64 = durations_ns.iter().sum();
let total_seconds = elapsed_nanos as f64 / 1_000_000_000.0;
let throughput = if total_seconds > 0.0 { total_events / total_seconds } else { 0.0 };
```

Le débit est calculé à partir de la précision nanoseconde (pas milliseconde) pour éviter la division par zéro quand les itérations se terminent en moins de 1ms. Le champ `total_elapsed_ms` dans la sortie est dérivé des nanosecondes pour l'affichage.

### Mesure RSS

Mesure mémoire spécifique à la plateforme :

| Plateforme | Méthode                                       | Unité                   |
|------------|-----------------------------------------------|-------------------------|
| Linux      | `/proc/self/status` -> ligne `VmRSS`          | Ko (converti en octets) |
| macOS      | `libc::getrusage(RUSAGE_SELF)` -> `ru_maxrss` | Octets (sur macOS)      |
| Windows    | Non implémenté                                | Retourne `None`         |

L'implémentation macOS utilise `unsafe` pour l'appel FFI `libc::getrusage`. C'est justifié : il n'existe pas d'API Rust safe pour cet appel système, et la fonction est bien documentée dans POSIX. La valeur de retour est vérifiée (`if ret == 0`) avant d'utiliser le résultat.

### Sortie colorée avec détection TTY

```rust
let is_tty = force_color || std::io::stdout().is_terminal();
let (bold, cyan, red, yellow, green, dim, reset) = if is_tty {
    ("\x1b[1m", "\x1b[36m", "\x1b[31m", "\x1b[33m", "\x1b[32m", "\x1b[2m", "\x1b[0m")
} else {
    ("", "", "", "", "", "", "")
};
```

Les codes d'échappement ANSI sont supprimés quand stdout n'est pas un terminal (ex. redirigé vers un fichier ou `jq`). Le paramètre `force_color` permet aux tests d'exercer le chemin coloré sans vrai TTY. Cela suit la convention d'outils comme `ls --color=auto` et la [sortie de rustc](https://doc.rust-lang.org/rustc/command-line-arguments.html).

### PgStat : analyse de hotspots pg_stat_statements

`perf-sentinel pg-stat --input FILE` parse les exports `pg_stat_statements` de PostgreSQL (CSV ou JSON, auto-détecté) et produit des classements de hotspots par temps d'exécution total, nombre d'appels et temps d'exécution moyen. Le flag `--traces` permet la référence croisée avec les findings de traces : l'outil exécute `pipeline::analyze()` sur le fichier de traces et marque les entrées `pg_stat_statements` dont le template normalisé apparaît aussi dans les findings.

Cette sous-commande est intentionnellement séparée d'`analyze` car les données `pg_stat_statements` n'ont pas de `trace_id` -- elles ne peuvent pas participer au pipeline de corrélation de traces.

### Inspect : TUI interactif

`perf-sentinel inspect --input FILE` lance une interface terminal construite avec [ratatui](https://ratatui.rs/) et [crossterm](https://docs.rs/crossterm/). Ces dépendances sont dans `sentinel-cli/Cargo.toml` uniquement (pas `sentinel-core`) car le TUI est une préoccupation de présentation.

**Layout :** découpage en 3 panneaux -- liste des traces (haut-gauche, 30%), findings de la trace sélectionnée (haut-droite, 70%), détail du finding avec arbre de spans (bas, 50%). Le panneau de détail réutilise `explain::build_tree()` et `explain::format_tree_text()` pour l'affichage de l'arbre de spans.

**Gestion d'état :** la struct `App` contient des `findings_by_trace` pré-calculés (indexés à la construction) pour éviter de recalculer à chaque frame. L'état de navigation (selected_trace, selected_finding, active_panel, scroll_offset) est mis à jour par les événements clavier.

## Parsing de la configuration

### Double format : sectionné + plat

La config supporte deux formats pour la rétrocompatibilité :

**Sectionné (recommandé) :**
```toml
[detection]
n_plus_one_min_occurrences = 5
```

**Legacy plat :**
```toml
n_plus_one_threshold = 5
```

**Priorité :** valeur sectionnée > valeur plate > défaut. Cela est implémenté avec des champs `Option<T>` dans les structs de section brutes :

```rust
struct DetectionSection {
    n_plus_one_min_occurrences: Option<u32>,
    // ...
}
```

`serde(default)` produit `None` pour les champs absents. La conversion `From<RawConfig> for Config` utilise des chaînes `.or()` :

```rust
n_plus_one_threshold: raw.detection.n_plus_one_min_occurrences
    .or(raw.n_plus_one_threshold)
    .unwrap_or(defaults.n_plus_one_threshold),
```

### Bornes de validation

Chaque champ numérique a des bornes explicites dans `validate()` :

| Champ                        | Min   | Max                  | Raison                                     |
|------------------------------|-------|----------------------|--------------------------------------------|
| `max_payload_size`           | 1 024 | 104 857 600 (100 Mo) | Empêcher la désactivation de la protection |
| `max_active_traces`          | 1     | 1 000 000            | Empêcher la mémoire non bornée             |
| `max_events_per_trace`       | 1     | 100 000              | Empêcher l'OOM par trace                   |
| `n_plus_one_threshold`       | 1     | *(aucun)*            | Au moins 1 occurrence pour détecter        |
| `window_duration_ms`         | 1     | *(aucun)*            | Fenêtre non nulle                          |
| `slow_query_threshold_ms`    | 1     | *(aucun)*            | Seuil non nul                              |
| `slow_query_min_occurrences` | 1     | *(aucun)*            | Au moins 1 occurrence                      |
| `max_fanout`                 | 1     | 100 000              | Empêcher la désactivation de la détection  |
| `trace_ttl_ms`               | 100   | *(aucun)*            | Intervalle d'éviction minimum              |
| `sampling_rate`              | 0.0   | 1.0                  | Probabilité valide                         |
| `io_waste_ratio_max`         | 0.0   | 1.0                  | Ratio valide                               |

La vérification de `listen_addr` non-loopback émet un avertissement mais ne rejette pas :

```rust
tracing::warn!(
    "Daemon configured to listen on non-loopback address: {}. \
     Endpoints have no authentication: use a reverse proxy or \
     network policy for security.",
    self.listen_addr
);
```

Cela permet aux utilisateurs avancés de lier à `0.0.0.0` derrière un reverse proxy, tout en rendant explicites les implications de sécurité.

## Profil release

```toml
[profile.release]
codegen-units = 1
lto = "thin"
strip = true
panic = "abort"
opt-level = 3
```

### `codegen-units = 1`

Une seule unité de codegen active l'optimisation du crate entier : le compilateur peut inliner à travers tous les modules et optimiser le crate entier comme une seule unité de traduction. Le compromis est un temps de compilation plus long (le codegen parallèle est désactivé). Pour les builds release, c'est acceptable.

Référence : [The Rust Performance Book: Build Configuration](https://nnethercote.github.io/perf-book/build-configuration.html)

### `lto = "thin"`

[ThinLTO](https://blog.llvm.org/2016/06/thinlto-scalable-and-incremental-lto.html) fournit la plupart des bénéfices de taille de binaire et de performance du LTO complet avec des temps de liaison significativement plus rapides. Le LTO complet ajoute ~30s au temps de liaison sur ce projet avec un bénéfice supplémentaire marginal. ThinLTO permet l'inlining inter-modules et l'élimination de code mort tout en supportant les builds incrémentaux.

### `strip = true`

Supprime les symboles de debug du binaire release. Réduit la taille de ~15 Mo à ~8 Mo. Acceptable pour un outil CLI distribué où les utilisateurs n'ont pas besoin d'informations de debug.

### `panic = "abort"`

Élimine la machinerie d'unwinding (~200 Ko d'économie binaire). Puisque perf-sentinel est un outil autonome (pas une bibliothèque consommée par du code Rust qui attrape les panics avec `catch_unwind`), abort-on-panic est sûr et réduit à la fois la taille du binaire et le surcoût à l'exécution.

### `opt-level = 3`

Optimisation maximale : inlining agressif, vectorisation de boucles et élimination de code mort. Le chemin chaud de perf-sentinel est le traitement de données (correspondance de chaînes, opérations HashMap, chaînes d'itérateurs) qui bénéficie de l'inlining. La [documentation Cargo](https://doc.rust-lang.org/cargo/reference/profiles.html) note que la différence entre `opt-level = 2` et `3` est principalement un inlining plus agressif, ce qui est exactement ce dont un outil pipeline a besoin.

L'alternative `opt-level = "s"` (optimiser pour la taille) a été envisagée mais rejetée : la différence de taille binaire est marginale (~200 Ko), tandis que la différence de débit peut atteindre 10-30% sur les charges de traitement de données.

## Stratégie de distribution

1. **GitHub Releases** (principal) : binaires multi-plateformes pour 4 cibles (linux/amd64, linux/arm64, macOS/arm64, windows/amd64) avec checksums SHA256. Les Mac Intel peuvent utiliser le binaire arm64 via Rosetta 2
2. **`cargo install sentinel-cli`** via crates.io
3. **Docker** (`FROM scratch`, `USER 65534`) : image minimale pour les déploiements Kubernetes

Les GitHub Actions sont épinglées aux SHAs de commit pour la sécurité de la chaîne d'approvisionnement. L'outil `cross` utilisé pour la cross-compilation ARM est épinglé à une version spécifique (`--version 0.2.5`) pour éviter des comportements inattendus lors de mises à jour upstream. Le workflow de release génère des checksums SHA256 pour tous les binaires.
