# Ingestion et mode daemon

## Conversion OTLP

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/otlp-conversion_dark.svg">
  <img alt="Conversion OTLP deux passes" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/otlp-conversion.svg">
</picture>

### Conception en deux passes

`convert_otlp_request()` traite chaque bloc `resource_spans` en deux passes :

**Passe 1 : Construction de l'index des spans :**
```rust
let span_index: HashMap<&[u8], &Span> = scope_spans.iter()
    .flat_map(|ss| &ss.spans)
    .map(|span| (span.span_id.as_slice(), span))
    .collect();
```

**Passe 2 : Conversion des spans I/O :**
```rust
for span in &scope.spans {
    if let Some(event) = convert_span(span, service_name, &span_index) {
        events.push(event);
    }
}
```

**Pourquoi deux passes ?** Dans OTLP, un span parent peut apparaître après son enfant dans le message protobuf. La première passe construit une table de recherche pour que la seconde passe puisse résoudre `source.endpoint` depuis l'attribut `http.route` du span parent. Une approche en une seule passe manquerait les spans parents définis plus loin dans le message.

L'index utilise des clés `&[u8]` (octets bruts du span_id), évitant l'encodage hexadécimal juste pour la recherche. L'index de spans est plafonné à 100 000 spans par resource pour prévenir l'épuisement mémoire depuis des payloads OTLP pathologiques. Un `tracing::warn!` est émis quand le cap est atteint pour aider les opérateurs à diagnostiquer une résolution de parent dégradée.

### Table de recherche `bytes_to_hex`

```rust
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        buf.push(HEX[(b >> 4) as usize]);
        buf.push(HEX[(b & 0x0f) as usize]);
    }
    // Tous les octets viennent de HEX (ASCII 0-9, a-f), toujours du UTF-8 valide.
    String::from_utf8(buf).expect("hex table is ASCII")
}
```

C'est une optimisation bien connue pour l'encodage hexadécimal. Au lieu d'utiliser `write!(hex, "{b:02x}")` (qui invoque la machinerie de formatage par octet à ~30ns), la table de recherche convertit chaque octet en deux caractères hexadécimaux via décalage de bits à ~5ns par octet. Le `Vec<u8>` est pré-alloué et l'appel `from_utf8` est infaillible puisque seuls des chiffres hexadécimaux ASCII sont insérés. Pas de `unsafe` nécessaire : le `expect` est une assertion à coût zéro sur une condition qui ne peut pas échouer.

Pour un trace_id de 16 octets + un span_id de 8 octets, cela économise ~600ns par conversion de span. À 100 000 événements/sec, c'est 60ms/sec de surcoût évité.

### `nanos_to_iso8601` : algorithme de Howard Hinnant

> **Note :** cette fonction vit dans le module partagé `time.rs` et est réutilisée par les ingestions Jaeger et Zipkin via `micros_to_iso8601`.

La conversion de nanosecondes Unix vers `YYYY-MM-DDTHH:MM:SS.mmmZ` utilise l'algorithme de date civile de [Howard Hinnant](https://howardhinnant.github.io/date_algorithms.html). Les étapes clés :

1. Convertir les nanosecondes en jours depuis l'epoch + millisecondes restantes
2. Décaler l'epoch au 1er mars, an 0 (en ajoutant 719 468 jours)
3. Calculer l'ère (cycle de 400 ans) et le jour de l'ère
4. Dériver l'année de l'ère, le jour de l'année, le mois et le jour en utilisant une formule sans table de recherche

Cela évite le crate [chrono](https://docs.rs/chrono/) (~150 Ko de surcoût binaire) et son surcoût de parsing de ~200ns. L'algorithme artisanal gère correctement les années bissextiles (vérifié par un test avec `2024-02-29`).

### Priorité du type d'événement

Quand un span possède à la fois un attribut SQL (`db.statement` ou `db.query.text`) et un attribut HTTP (`http.url` ou `url.full`), SQL prend la priorité. C'est intentionnel : l'instrumentation de base de données est plus spécifique que l'instrumentation client HTTP. L'attribut SQL contient le texte réel de la requête nécessaire à la normalisation, tandis que l'attribut HTTP pourrait représenter la même opération au niveau transport.

Les conventions sémantiques OTel legacy (pré-1.21) et stables (1.21+) sont toutes deux supportées : `db.statement` et `db.query.text` pour le SQL, `http.url` et `url.full` pour le HTTP, `http.method` et `http.request.method` pour le verbe, `http.status_code` et `http.response.status_code` pour le statut. Cela assure la compatibilité avec les anciens SDKs OTel comme avec les agents Java modernes (v2.x).

### Protection contre la dérive d'horloge

```rust
if end_nanos < start_nanos {
    tracing::trace!("Span has end_time < start_time (clock skew?), duration forced to 0");
}
let duration_us = end_nanos.saturating_sub(start_nanos) / 1000;
```

`saturating_sub` retourne 0 pour les durées négatives au lieu de boucler. Un log de niveau trace aide les opérateurs à diagnostiquer les problèmes d'intégration OTLP sans inonder les logs.

## Ingestion JSON

```rust
pub fn ingest(&self, raw: &[u8]) -> Result<Vec<SpanEvent>, Self::Error> {
    if raw.len() > self.max_size {
        return Err(JsonIngestError::PayloadTooLarge { ... });
    }
    serde_json::from_slice(raw)
}
```

La taille du payload est vérifiée **avant** la désérialisation. Cela empêche `serde_json` d'allouer de la mémoire pour un payload JSON de plusieurs gigaoctets avant de le rejeter.

### Auto-détection du format

`JsonIngest` auto-détecte le format d'entrée en utilisant des heuristiques au niveau des octets. Il examine les premiers 1-4 Ko du payload pour déterminer s'il s'agit de Jaeger, Zipkin ou du format natif, évitant un double parsing coûteux.

**Sanitisation à la frontière.** Après le parsing, le chemin d'ingestion JSON valide `cloud_region` via `is_valid_region_id` et exécute `sanitize_span_event` sur chaque événement, appliquant les mêmes limites de champs et troncatures UTF-8 que le chemin OTLP. Cela garantit des données uniformément sanitisées en aval, quel que soit le format d'ingestion.

### Ingestion Jaeger JSON

`ingest/jaeger.rs` parse le format d'export Jaeger JSON. Le `startTime` (microsecondes) est converti via `micros_to_iso8601` du module partagé `time.rs`. Le `parent_span_id` est extrait depuis les `references` où `refType = "CHILD_OF"`.

### Ingestion Zipkin JSON v2

`ingest/zipkin.rs` parse le format Zipkin JSON v2. Le `parentId` est un champ direct. Les tags sont un `HashMap<String, String>`.

## Boucle événementielle du daemon

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/daemon_dark.svg">
  <img alt="Architecture du daemon" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/daemon.svg">
</picture>

### Architecture

```
OTLP gRPC (port 4317)   ─┐
OTLP HTTP (port 4318)   ─┤─→ mpsc::channel(1024) ─→ TraceWindow ─→ éviction ─→ detect ─→ score ─→ NDJSON
Socket unix JSON        ─┘
```

La boucle événementielle utilise `tokio::select!` pour multiplexer :
- **Réception d'événements** depuis le canal -> normaliser -> pousser dans la fenêtre
- **Ticker** toutes les TTL/2 ms -> évincer les traces expirées -> detect/score -> émettre
- **Ctrl+C** -> vider toutes les traces -> detect/score -> émettre -> arrêt

### Normalisation en dehors du verrou

```rust
// Normaliser EN DEHORS du verrou :
let normalized: Vec<_> = events.into_iter().map(normalize::normalize).collect();
// Puis acquérir le verrou et pousser :
let mut w = window.lock().await;
for event in normalized { w.push(event, now_ms); }
```

La normalisation est un travail lié au CPU (regex, manipulation de chaînes). La déplacer en dehors du verrou `Mutex` minimise le temps de détention du verrou aux seules opérations HashMap. Sous contention (ticker et réception s'exécutant concurremment), cela empêche le ticker d'éviction de bloquer sur la normalisation.

### Échantillonnage au niveau des traces

```rust
fn should_sample(trace_id: &str, rate: f64) -> bool {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // base de décalage FNV-1a
    for b in trace_id.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3); // nombre premier FNV-1a
    }
    (hash as f64 / u64::MAX as f64) < rate
}
```

Le [hash FNV-1a](https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function) est un hash rapide, non cryptographique qui produit une sortie bien distribuée. La base de décalage et le nombre premier sont les constantes standard FNV-1a 64 bits.

**Pourquoi FNV-1a ?** Plus simple et plus rapide (~2ns pour un trace_id typique) que `std::hash::DefaultHasher` (SipHash, ~10ns). La qualité cryptographique n'est pas nécessaire pour l'échantillonnage : seule la distribution uniforme compte.

**Déterministe :** le même `trace_id` produit toujours la même décision d'échantillonnage, garantissant que tous les événements d'une trace sont soit conservés soit supprimés ensemble.

**Cache par lot :** la fonction `apply_sampling()` filtre un lot d'événements en utilisant un cache `HashMap<String, bool>`. Au sein d'un seul lot, plusieurs événements peuvent partager un `trace_id`. Le cache utilise `get()` avant `insert()` de sorte que `trace_id` n'est cloné que pour le premier événement de chaque trace, pas à chaque hit de cache. Extraire cette logique dans une fonction dédiée garde la boucle `tokio::select!` lisible.

### Canal borné

```rust
let (tx, mut rx) = mpsc::channel::<Vec<SpanEvent>>(1024);
```

Le [canal borné](https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.channel.html) fournit une contre-pression : si la boucle événementielle prend du retard et que le buffer se remplit à 1024 lots, les émetteurs d'ingestion attendront jusqu'à ce qu'un espace soit disponible. Cela empêche la croissance mémoire non bornée par des producteurs rapides.

### Renforcement de la sécurité

**Permissions du socket Unix :**
```rust
use std::os::unix::fs::PermissionsExt;
std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
```

Le mode `0o600` restreint la lecture/écriture au propriétaire du socket uniquement, empêchant d'autres utilisateurs locaux d'injecter des événements. Si `set_permissions` échoue, le fichier socket est supprimé et le listener ne démarre pas (erreur fatale, pas un simple avertissement).

**Sémaphore de connexion :**
```rust
let semaphore = Arc::new(tokio::sync::Semaphore::new(128));
```

Limite les connexions concurrentes du socket JSON à 128. Sans cela, un attaquant local pourrait ouvrir des milliers de connexions, chacune consommant une tâche tokio et de la mémoire tampon.

**Limite d'octets par connexion :**
```rust
const CONNECTION_LIMIT_FACTOR: u64 = 16;
let limited = stream.take(max_payload_size as u64 * CONNECTION_LIMIT_FACTOR);
```

Chaque connexion est limitée à 16 × max_payload_size octets au total (défaut 16 Mo). Cela empêche une seule connexion de consommer une mémoire non bornée avec un flux de données qui ne contient jamais de saut de ligne.

**Timeouts des requêtes :**
- gRPC : `tonic::transport::Server::builder().timeout(Duration::from_secs(60))`
- HTTP : `tower::timeout::TimeoutLayer::new(Duration::from_secs(60))` via le `HandleErrorLayer` d'axum

Ceux-ci empêchent les connexions lentes/bloquées de retenir des ressources indéfiniment. Le handler de timeout HTTP émet un log `tracing::debug!` avant de retourner `408 REQUEST_TIMEOUT`, aidant les opérateurs à diagnostiquer les clients lents ou bloqués.

### Sortie NDJSON

Les findings sont émis en JSON délimité par des sauts de ligne sur stdout en utilisant `serde_json::to_writer` avec un handle stdout verrouillé pour éviter les allocations String intermédiaires :

```rust
let stdout = std::io::stdout();
let mut lock = stdout.lock();
for finding in &findings {
    if serde_json::to_writer(&mut lock, finding).is_ok() {
        let _ = writeln!(lock);
    }
}
```

Ce format est compatible avec les outils d'agrégation de logs (Loki, ELK) qui consomment du JSON délimité par des lignes. Chaque ligne est un objet JSON complet qui peut être parsé indépendamment.

### Ratio de gaspillage cumulatif

La jauge Prometheus `io_waste_ratio` est calculée à partir de compteurs cumulatifs :

```rust
let cumulative_total = metrics.total_io_ops.get();
if cumulative_total > 0.0 {
    metrics.io_waste_ratio.set(metrics.avoidable_io_ops.get() / cumulative_total);
}
```

C'est une moyenne sur toute la durée, pas une métrique fenêtrée. Les utilisateurs qui ont besoin d'un taux récent peuvent utiliser `rate()` de Prometheus sur les compteurs bruts (`total_io_ops`, `avoidable_io_ops`).

### Exemplars Grafana

Le crate `prometheus` 0.14.0 ne supporte pas nativement les exemplars OpenMetrics. Plutôt que d'ajouter une dépendance, les annotations exemplars sont injectées par post-traitement du texte Prometheus rendu.

**Suivi des trace_id worst-case :**

`MetricsState` stocke les données d'exemplars dans des champs protégés par `RwLock` :
- `worst_finding_trace: HashMap<(String, String), ExemplarData>` : indexé par (finding_type, severity), mis à jour à chaque appel `record_batch()`
- `worst_waste_trace: Option<ExemplarData>` : le trace_id du finding avec le plus d'I/O évitables

`RwLock` est utilisé plutôt que `Mutex` car `render()` (chemin de lecture) est appelé fréquemment par les scrapes Prometheus, alors que `record_batch()` (chemin d'écriture) est appelé moins souvent. L'empoisonnement de lock est géré gracieusement via `unwrap_or_else(PoisonError::into_inner)`, de sorte qu'un panic dans un thread ne cascade pas en crashs sur les acquisitions de lock suivantes.

**Injection d'exemplars :**

`inject_exemplars()` itère sur le texte rendu ligne par ligne. Pour les lignes `perf_sentinel_findings_total{...}`, il parse les labels `type` et `severity` pour trouver l'exemplar correspondant. Pour les lignes `perf_sentinel_io_waste_ratio`, il ajoute l'exemplar de gaspillage.

Le format suit la spécification OpenMetrics : `metric{labels} value # {trace_id="abc123"}`. Quand des exemplars sont présents, le header `Content-Type` passe de `text/plain; version=0.0.4` (Prometheus) à `application/openmetrics-text; version=1.0.0` (OpenMetrics) pour que la source de données Prometheus de Grafana puisse reconnaître et afficher les liens d'exemplars.

## Ingestion pg_stat_statements

`ingest/pg_stat.rs` fournit un chemin d'analyse autonome pour les exports `pg_stat_statements` de PostgreSQL. Contrairement à l'ingestion basée sur les traces, ces données n'ont pas de `trace_id` ni de `span_id`, elles ne peuvent pas alimenter le pipeline de détection N+1/redondant. Elles fournissent un classement de hotspots et une référence croisée avec les findings de traces.

### Décisions de conception

**Séparé de `IngestSource` :** le trait `IngestSource` retourne `Vec<SpanEvent>`, mais les données `pg_stat_statements` ne correspondent pas à `SpanEvent` (pas de trace_id, span_id, ni timestamp). Elles produisent leur propre type `PgStatReport` avec des classements.

**Auto-détection du format :** suit le même pattern d'heuristique byte-level que `json.rs`. Si le premier octet non-espace est `[` ou `{`, parse en JSON ; sinon, parse en CSV. Pas de crate csv externe, le parseur CSV gère le quoting RFC 4180 manuellement (champs entre guillemets doubles, `""` échappé).

**Réutilisation de la normalisation SQL :** chaque requête passe par `normalize::sql::normalize_sql()` pour produire un template comparable avec les findings basés sur les traces.

### Sortie à quatre classements

`rank_pg_stat(entries, top_n)` retourne un `PgStatReport` avec quatre classements dans un ordre stable, indexé par position : `[by_total_time, by_calls, by_mean_time, by_io_blocks]`. Chaque classement contient les mêmes top-N entrées, réordonnées selon son propre critère :

- **by_total_time** : `total_exec_time_ms` décroissant. Requêtes qui dominent le temps DB wall-clock. Signal hotspot principal.
- **by_calls** : `calls` décroissant. Requêtes à fort volume, candidates N+1 typiques.
- **by_mean_time** : `mean_exec_time_ms` décroissant. Requêtes individuellement lentes indépendamment du volume.
- **by_io_blocks** : `shared_blks_hit + shared_blks_read` décroissant. Signal de pression cache : requêtes qui touchent le plus de pages du buffer partagé, peu importe si elles étaient chaudes ou froides. Complémentaire de `by_total_time` quand le CPU est idle mais que le cache s'agite.

Le sub-switcher du dashboard HTML onglet `pg_stat` consomme ces quatre classements par position, donc les nouveaux classements s'ajoutent en fin de liste (jamais réordonnés, jamais insérés au milieu) pour préserver la stabilité des indices côté consommateurs.

### Référence croisée

`cross_reference()` accepte `&mut [PgStatEntry]` et `&[Finding]`. Il construit un `HashSet` des templates de findings et marque les entrées dont le `normalized_template` correspond. Complexité O(n + m) où n = entrées, m = findings. Le flag `seen_in_traces` permet à la CLI de mettre en évidence les requêtes présentes dans les deux sources de données.

## Scrape Prometheus pour pg_stat

La fonction `fetch_from_prometheus(endpoint, top_n)` dans `ingest/pg_stat.rs` permet de récupérer les données `pg_stat_statements` directement depuis l'API HTTP de Prometheus, sans export CSV/JSON manuel.

### Fonctionnement

1. Construire une requête PromQL `topk(N, pg_stat_statements_seconds_total)` pour obtenir les N requêtes les plus consommatrices.
2. Envoyer une requête `GET /api/v1/query?query=...` au endpoint Prometheus configuré via le client HTTP partagé (`http_client::build_client`).
3. Parser la réponse JSON au format standard Prometheus (`data.result[]`).
4. Extraire les labels `query` ou `queryid` du champ `metric` pour chaque résultat.
5. Convertir la valeur en millisecondes (la métrique est en secondes), normaliser le SQL via `normalize_sql()` et produire des `PgStatEntry`.

Le timeout est fixé à 30 secondes. Les erreurs de transport et de format sont rapportées via des variantes dédiées de `PgStatError` (`PrometheusRequest`, `PrometheusFormat`).

### Usage CLI

```bash
perf-sentinel pg-stat --prometheus http://prometheus:9090
```

Le flag `--prometheus` est mutuellement exclusif avec `--input`. Le `--traces` pour la référence croisée fonctionne de la même manière qu'avec un fichier local.

La sous-commande `report` expose la même capacité via `--pg-stat-prometheus URL`, mutuellement exclusif avec le flag fichier `--pg-stat FILE` (enforced au niveau clap via `conflicts_with`). Quand l'un des deux est passé, le `PgStatReport` résultant est embarqué dans l'onglet `pg_stat` du dashboard HTML avec les quatre classements décrits ci-dessus. Le chemin de scrape est partagé avec `pg-stat --prometheus`, aucun code de fetch dupliqué.

## Ingestion Tempo

`ingest/tempo.rs` fournit le chemin de replay post-mortem : la sous-commande interroge l'API HTTP d'un Grafana Tempo en cours d'exécution, récupère les corps de traces en protobuf OTLP, les décode via le helper existant `convert_otlp_request` et renvoie un `Vec<SpanEvent>` au pipeline d'analyse standard. Deux modes : trace unique par ID (`--trace-id`, un seul `GET /api/traces/{id}`) ou search-then-fetch (`--service --lookback`, un `GET /api/search` suivi de fetches trace par trace). Gatée derrière la cargo feature `tempo`.

### Fetch parallèle avec cap de concurrence

La boucle de fetch par trace est parallélisée via `tokio::task::JoinSet`, protégée par un `Arc<Semaphore>` capé à `FETCH_CONCURRENCY = 16` permits. Chaque task spawnée acquiert un permit via `acquire_owned` avant l'appel HTTP et le libère au drop (RAII). Le cap a été choisi empiriquement pour saturer une connexion Tempo distante sur lien WAN (observé ~10-20s pour 100 traces vs. ~2m30s avec la boucle séquentielle précédente) sans mettre à genoux une seule replica de query-frontend. Il est hardcodé aujourd'hui, pas exposé en configuration utilisateur. Le pattern reprend celui de `score::cloud_energy::scraper`, qui parallélise de la même façon les requêtes CPU Prometheus par service.

### Séparation des timeouts

Deux constantes dédiées plutôt qu'une valeur unique : `SEARCH_TIMEOUT = 5s` pour `/api/search` (la réponse est une petite liste de trace IDs, un timeout serré fait échouer vite sur un endpoint cassé) et `FETCH_TRACE_TIMEOUT = 30s` pour `/api/traces/{id}` (les corps de traces peuvent légitimement faire plusieurs MiB sur une requête à fan-out large et la query-frontend doit assembler les spans depuis les ingesters + le stockage long terme). Un cap unique à 5 s droppait empiriquement des dizaines de traces par batch de 100 sur les fenêtres longues ; 30 s correspond au défaut de la datasource Tempo côté Grafana. Les deux timeouts sont passés en paramètre au helper partagé `fetch_raw` plutôt que stockés dans une constante unique au niveau module, pour que les chemins search et fetch-trace ne puissent pas diverger silencieusement.

### Ctrl-C et agrégation d'erreurs

La drain loop est conduite par un `tokio::select!` avec ordre `biased` : `tokio::signal::ctrl_c()` est polled avant `set.join_next()` pour qu'une interruption en attente ne soit pas starvée par une rafale de completions. Sur signal, `set.abort_all()` flague toutes les tasks in-flight pour cancellation ; les traces déjà complétées sont conservées, les tasks aborted résolvent en `JoinError::is_cancelled()` et sont silencieusement skippées. La variante dédiée `TempoError::Interrupted` est renvoyée uniquement si zéro trace n'a eu le temps de se compléter avant le signal, pour que les quality gates CI puissent distinguer un abort opérateur d'un résultat vide authentique (`NoTracesFound`).

Les failures par trace loguent au niveau `debug`, pas `error`. Une seule ligne de summary classifiée (`emit_fetch_summary`) est émise à la fin de la boucle, bucketée par type d'erreur (`timeout`, `transport`, `http_status`, `protobuf_decode`, `body_read`, `json_parse`, `task_panic`) pour que l'outillage downstream (Loki, CloudWatch) puisse alerter sur le bon signal sans parser 50 lignes `ERROR` individuelles sur un Tempo dégradé. La sévérité du summary suit la pire classe observée : `warn` si uniquement des skips `TraceNotFound` ont eu lieu (condition occasionnelle attendue, ex. une trace sortie de rétention entre le search et le fetch), `error` sinon. Un test unitaire (`classify_fetch_error_buckets_every_hard_failure_variant`) sert de garde-fou contre la dérive : si une nouvelle variante est ajoutée à `TempoError` plus tard, elle ne tombe pas silencieusement dans `"other"`.

## API de requête du daemon

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/query-api_dark.svg">
  <img alt="Architecture de l'API de requête du daemon" src="https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/diagrams/svg/query-api.svg">
</picture>

En mode `watch`, le daemon expose une API HTTP de consultation aux côtés des endpoints existants `/v1/traces` et `/metrics`. Cela permet d'interroger l'état interne du daemon sans accéder directement à stdout.

### FindingsStore

`daemon/findings_store.rs` implémente un buffer circulaire thread-safe pour les findings récents :

```rust
pub struct FindingsStore {
    inner: RwLock<VecDeque<StoredFinding>>,
    max_size: usize,
}
```

Le `RwLock` tokio permet plusieurs lecteurs simultanés (scrapes de l'API) sans bloquer entre eux, tout en garantissant l'exclusivité pour les écritures (insertion depuis `process_traces`). La capacité initiale du VecDeque est plafonnée à `min(max_size, INITIAL_CAPACITY_CEILING)` avec un plafond de 4096 pour amortir les réallocations sans gonfler le RSS au démarrage.

**Court-circuit `max_size == 0` :** quand `max_retained_findings = 0`, `push_batch` retourne immédiatement sans allouer. Cela permet aux opérateurs qui désactivent l'API (`api_enabled = false`) de récupérer la mémoire du store en mettant aussi `max_retained_findings = 0`.

**Clones hors lock :** `push_batch` construit les nouvelles entrées `StoredFinding` AVANT d'acquérir le write lock, puis fait un `extend + drain` rapide sous lock. Les lecteurs API ne sont pas bloqués par les allocations `Finding::clone()`.

**Eviction :** quand le buffer atteint sa capacité maximale (défaut 10 000), chaque nouvel ajout via `push_batch` évince les plus anciens via `drain(..excess)`. Cela maintient un coût mémoire borné.

**Filtrage :** `query()` parcourt le buffer en ordre inverse (plus récent d'abord) et applique des filtres optionnels par service, type de finding et sévérité. La limite par défaut est de 100 résultats, plafonnée à `MAX_FINDINGS_LIMIT = 1000`.

### Endpoints HTTP

`daemon/query_api.rs` définit six routes axum montées dans le routeur existant du daemon. Le router n'est mergé dans le stack HTTP que si `[daemon] api_enabled = true` (défaut true). Mettre `api_enabled = false` désactive toutes les routes `/api/*` tout en conservant l'ingestion OTLP et `/metrics`.

| Endpoint                   | Méthode | Plafond                                                                     | Description                                                                        |
|----------------------------|---------|-----------------------------------------------------------------------------|------------------------------------------------------------------------------------|
| `/api/findings`            | GET     | `?limit=` plafonné à `MAX_FINDINGS_LIMIT = 1000`                            | Findings récents, avec filtres `?service=`, `?type=`, `?severity=`, `?limit=`      |
| `/api/findings/{trace_id}` | GET     | aucun                                                                       | Findings pour un trace_id spécifique                                               |
| `/api/explain/{trace_id}`  | GET     | aucun                                                                       | Arbre de trace avec findings en ligne (depuis la mémoire du daemon)                |
| `/api/correlations`        | GET     | tronqué à `MAX_CORRELATIONS_LIMIT = 1000` (trié par confiance décroissante) | Corrélations cross-trace actives. Vide quand `correlator` est `None`               |
| `/api/status`              | GET     | aucun                                                                       | Santé du daemon : version, uptime, traces actives, findings stockés                |
| `/api/export/report`       | GET     | hérite des plafonds `/api/findings` et `/api/correlations`                  | Snapshot `Report` JSON complet, prêt à piper dans `perf-sentinel report --input -` |

### Sémantique du snapshot `/api/export/report`

L'endpoint retourne un `Report` de forme identique à la sortie de `analyze --format json`, ce qui permet au pipeline HTML de le consommer sans parseur séparé. Les champs sont remplis depuis l'état live du daemon : `findings` depuis `FindingsStore::query`, `correlations` depuis `CrossTraceCorrelator::active_correlations`, `analysis.events_processed` et `traces_analyzed` depuis les compteurs metrics. `green_summary`, `quality_gate` et `per_endpoint_io_ops` restent vides par conception : le daemon ne fait pas tourner l'étape de scoring à chaque batch (les nombres SCI nécessitent une fenêtre d'observation suffisamment large, que `analyze` fournit et pas le daemon), et la quality gate est un concept batch. Les consommateurs qui en ont besoin doivent pousser la sortie daemon vers `analyze --input -` à la place.

Gestion du cold-start : quand `events_processed == 0`, l'endpoint retourne HTTP 503 avec `{"error": "daemon has not yet processed any events"}`. Ça évite qu'un dashboard affiche un "zéro finding" trompeur sur un daemon qui n'a pas encore reçu son premier batch OTLP.

Atomicité du snapshot : le handler acquiert le read lock du `FindingsStore` puis le mutex du correlator en séquence, pas atomiquement. Les deux collections peuvent donc être décalées d'un batch (findings de la génération N, correlations de N+1), acceptable pour un dashboard post-mortem mais pas pour un contrat de snapshot strict.

L'état partagé est encapsulé dans `QueryApiState` :

```rust
pub struct QueryApiState {
    pub findings_store: Arc<FindingsStore>,
    pub window: Arc<tokio::sync::Mutex<TraceWindow>>,
    pub detect_config: DetectConfig,
    pub start_time: std::time::Instant,
    pub correlator: Option<Arc<tokio::sync::Mutex<CrossTraceCorrelator>>>,
}
```

Le endpoint `/api/explain/{trace_id}` consulte la TraceWindow pour récupérer les spans (s'ils sont encore en mémoire), exécute les détecteurs par trace, puis construit l'arbre via `explain::build_tree` et `explain::format_tree_json`. Si la trace a déjà été évincée, il retourne un objet JSON avec un champ `error`.

### Configuration

```toml
[daemon]
api_enabled = true
max_retained_findings = 10000
```

`api_enabled` est à `true` par défaut. Quand `api_enabled = false`, les routes `/api/*` ne sont pas montées, mais le `FindingsStore` est toujours peuplé à chaque tick (la détection tourne avant la vérification du flag). Pour libérer aussi la mémoire du store, mettez `max_retained_findings = 0` : `push_batch` court-circuite alors sans allouer.

## Intégration du corrélateur cross-trace

Le `CrossTraceCorrelator` (décrit dans [04 : Détection](04-DETECTION-FR.md)) est instancié dans la boucle événementielle du daemon quand `[daemon.correlation] enabled = true`. À chaque tick :

1. `process_traces` produit un lot de findings.
2. Les findings sont insérés dans le FindingsStore via `push_batch`.
3. Les findings sont passés au corrélateur via `ingest(findings, now_ms)`.
4. `GET /api/correlations` appelle `active_correlations()` pour retourner les paires au-dessus des seuils configurés.

Le corrélateur est possédé par la boucle du daemon (pas dans un Arc/Mutex séparé), puisque seul le ticker y accède. Cela évite le coût de synchronisation.
