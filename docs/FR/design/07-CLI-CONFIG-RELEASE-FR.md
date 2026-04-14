# CLI, configuration et profil release

## Conception du CLI

Le CLI (`sentinel-cli`) est intentionnellement léger. Il parse les arguments avec [clap](https://docs.rs/clap/) et délègue aux fonctions de `sentinel-core`. Sept sous-commandes sont disponibles : `analyze`, `explain`, `watch`, `demo`, `bench`, `pg-stat` et `inspect`.

### Analyze : rapport coloré par défaut, JSON avec `--ci`

`perf-sentinel analyze` affiche un rapport coloré dans le terminal en mode interactif (sans `--ci`). C'est la sortie que les humains voient en utilisant l'outil localement. Avec `--ci`, la sortie passe en JSON structuré pour la consommation par les machines et le processus sort avec le code 1 si le quality gate échoue.

Cela suit la convention d'outils comme `cargo test` (sortie colorée par défaut, `--format json` pour le CI).

Le flag `--format` offre un contrôle explicite sur le format de sortie : `text` (terminal coloré, défaut), `json` (rapport structuré) ou `sarif` (SARIF v2.1.0 pour le code scanning). Avec `--ci` sans `--format`, la sortie est en JSON par défaut pour la rétrocompatibilité.

### Explain : vue arborescente par trace

`perf-sentinel explain --input FILE --trace-id ID` construit un arbre à partir des relations `parent_span_id` et annote les findings en ligne. Il exécute uniquement les détecteurs par trace (N+1, redondant, lent, fanout), les findings cross-trace ne sont pas inclus.

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

L'implémentation macOS utilise `unsafe` pour l'appel FFI `libc::getrusage`. C'est justifié : il n'existe pas d'API Rust safe pour cet appel système et la fonction est bien documentée dans POSIX. La valeur de retour est vérifiée (`if ret == 0`) avant d'utiliser le résultat.

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

Cette sous-commande est intentionnellement séparée d'`analyze` car les données `pg_stat_statements` n'ont pas de `trace_id`, elles ne peuvent pas participer au pipeline de corrélation de traces.

### Inspect : TUI interactif

`perf-sentinel inspect --input FILE` lance une interface terminal construite avec [ratatui](https://ratatui.rs/) et [crossterm](https://docs.rs/crossterm/). Ces dépendances sont dans `sentinel-cli/Cargo.toml` uniquement (pas `sentinel-core`) car le TUI est une préoccupation de présentation.

**Layout :** découpage en 3 panneaux, liste des traces (haut-gauche, 30%), findings de la trace sélectionnée (haut-droite, 70%), détail du finding avec arbre de spans (bas, 50%). Le panneau de détail réutilise `explain::build_tree()` et `explain::format_tree_text()` pour l'affichage de l'arbre de spans.

**Gestion d'état :** la struct `App` contient des `findings_by_trace` pré-calculés (indexés à la construction) pour éviter de recalculer à chaque frame. L'état de navigation (selected_trace, selected_finding, active_panel, scroll_offset) est mis à jour par les événements clavier.

### Feature flags

Le workspace utilise des feature flags Cargo pour garder les dépendances daemon optionnelles :

| Feature  | Crate           | Ce qu'il active                                                                                                                         |
|----------|-----------------|-----------------------------------------------------------------------------------------------------------------------------------------|
| `daemon` | `sentinel-core` | `hyper`, `hyper-util`, `http-body-util`, `bytes`, `arc-swap`. Active `daemon.rs`, scraper/state Scaphandre, scraper/state cloud energy. |
| `daemon` | `sentinel-cli`  | Transmet à `sentinel-core/daemon`. Active la sous-commande `watch`.                                                                     |
| `tui`    | `sentinel-cli`  | `ratatui`, `crossterm`. Active la sous-commande `inspect`.                                                                              |

### Localisation du code source dans les findings

Les findings peuvent inclure un champ optionnel `code_location` contenant les attributs OTel `code.*` extraits du span :

```rust
pub struct CodeLocation {
    pub function: Option<String>,
    pub filepath: Option<String>,
    pub lineno: Option<u32>,
    pub namespace: Option<String>,
}
```

Ces attributs sont extraits dans `ingest/otlp.rs` depuis les attributs du span lui-même (pas du parent) : `code.function`, `code.filepath`, `code.lineno`, `code.namespace`. Quand ils sont présents, le rapport CLI affiche la source ("Source: OrderService.processItems (OrderService.java:42)").

**Intégration SARIF.** La sortie SARIF v2.1.0 traduit `code_location` en `physicalLocation` :

```json
{
  "physicalLocation": {
    "artifactLocation": { "uri": "src/OrderService.java" },
    "region": { "startLine": 42 }
  }
}
```

Cela permet les annotations en ligne dans GitHub Code Scanning et GitLab SAST. Le champ `region` n'est émis que si `lineno` est présent.

**Dégradation gracieuse.** La plupart des agents OTel auto-instrumentés n'émettent pas `code.lineno`. Dans ce cas, `code_location` est `None` et le finding apparaît sans ligne source, sans bruit supplémentaire.

**Sanitization de `code.filepath`.** L'attribut OTel `code.filepath` est contrôlé par le client (un span hostile peut y mettre n'importe quelle chaîne). Avant de l'émettre comme `artifactLocation.uri` SARIF, `sanitize_sarif_filepath` rejette toute valeur qui pourrait phisher un consommateur ou contourner les résolveurs de code scanning. Le sanitizer renvoie `None` (et donc omet le `physicalLocations` array) pour :

- Chemins absolus (POSIX `/...`, Windows `\...`).
- Tout colon. Les chemins sources légitimes dans les apps instrumentées ne contiennent pas de colons. Rejet inconditionnel pour éviter les bypasses subtils autour de `javascript:`, `data:`, `file:`, etc.
- Segments de path traversal. Littéral `..` et variantes percent-encodées (`%2e%2e`, `%2E%2E`, casse mixte, `.%2e`, `%2e.`).
- Séquences double-encodées (`%25...`) qui décodent en `%` au premier passage puis en caractère réel au second.
- Préfixes UTF-8 overlong (`%c0`, `%c1`) qui décodent en encodages non-canoniques de caractères ASCII dans les décodeurs laxistes.
- Caractères de contrôle (newlines, NUL, etc.) qui pourraient casser le tokenizer du consommateur SARIF ou injecter dans les logs.
- Caractères Unicode BiDi et invisibles (`U+061C`, `U+180E`, `U+202A..U+202E`, `U+2066..U+2069`, `U+200B..U+200F`, `U+FEFF`) qui peuvent confondre l'affichage des noms de fichier (Trojan Source, CVE-2021-42574).

Les findings dont le filepath est rejeté apparaissent toujours dans le rapport SARIF, seul le tableau `physicalLocations` est omis (les `logicalLocations` et autres champs restent).

### Sous-commande `query`

`perf-sentinel query --daemon http://localhost:4318 <action>` interroge l'API HTTP du daemon en cours d'exécution. Cinq actions sont disponibles :

| Action | Endpoint API | Sortie | Description |
|---|---|---|---|
| `findings` | `/api/findings` | terminal coloré (défaut) ou JSON | Lister les findings récents avec filtres `--service`, `--type`, `--severity`, `--limit` |
| `explain` | `/api/explain/{trace_id}` | arbre coloré (défaut) ou JSON | Afficher l'arbre de trace avec findings en ligne (depuis la mémoire du daemon) |
| `inspect` | `/api/findings` | TUI ratatui | TUI interactif 3 panneaux alimenté par les données live du daemon |
| `correlations` | `/api/correlations` | tableau coloré (défaut) ou JSON | Afficher les corrélations cross-trace actives |
| `status` | `/api/status` | résumé coloré (défaut) ou JSON | Afficher l'état du daemon : version, uptime, traces actives, findings stockés |

Toutes les actions sauf `inspect` acceptent `--format text|json`. Le défaut est `text` (sortie colorée), comme la commande `analyze`. `--format json` produit du JSON brut pour le scripting.

**Sortie colorée.** `findings` réutilise `print_findings()` de la commande `analyze`. `explain` désérialise la réponse en `ExplainTree` et appelle `format_tree_text()`. `inspect` récupère d'abord les findings via `/api/findings?limit=10000`, puis pour chaque `trace_id` distinct récupère l'arbre via `/api/explain/{trace_id}` et le passe au TUI via `App::with_pre_rendered_trees`. Les traces encore dans la fenêtre du daemon affichent leur vrai arbre de spans ; les traces évincées s'affichent sans arbre (skip silencieux). `correlations` affiche un tableau avec la confiance en pourcentage coloré (rouge >= 80%, jaune >= 50%). `status` affiche les clés/valeurs avec l'uptime formaté (Xh Ym Zs).

La sous-commande est protégée par le feature flag `daemon`. Elle utilise le client HTTP partagé (`http_client::build_client`) avec un timeout de 10 secondes.

Le flag `--daemon` spécifie l'URL de base du daemon (défaut `http://localhost:4318`). C'est le même port que l'endpoint OTLP HTTP, les routes `/api/*` sont servies par le même serveur axum.

Les deux features sont dans le `default` du CLI. Les utilisateurs de `sentinel-core` en tant que dépendance de bibliothèque peuvent l'utiliser sans `daemon` pour éviter le stack hyper :

```toml
perf-sentinel-core = { version = "0.3", default-features = false }
```

Cela compile le pipeline batch complet (normalize, correlate, detect, score, report) sans code client HTTP. Les types de config (`ScaphandreConfig`, `CloudEnergyConfig`) sont toujours disponibles pour que le parseur TOML fonctionne ; seuls les scrapers runtime et les types state sont conditionnels.

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
