# Ingestion et mode daemon

## Conversion OTLP

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../diagrams/svg/otlp-conversion_dark.svg">
  <img alt="Conversion OTLP deux passes" src="../../diagrams/svg/otlp-conversion.svg">
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
  <source media="(prefers-color-scheme: dark)" srcset="../../diagrams/svg/daemon_dark.svg">
  <img alt="Architecture du daemon" src="../../diagrams/svg/daemon.svg">
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
- `worst_finding_trace: HashMap<(String, String), ExemplarData>` -- indexé par (finding_type, severity), mis à jour à chaque appel `record_batch()`
- `worst_waste_trace: Option<ExemplarData>` -- le trace_id du finding avec le plus d'I/O évitables

`RwLock` est utilisé plutôt que `Mutex` car `render()` (chemin de lecture) est appelé fréquemment par les scrapes Prometheus, alors que `record_batch()` (chemin d'écriture) est appelé moins souvent. L'empoisonnement de lock est géré gracieusement via `unwrap_or_else(PoisonError::into_inner)`, de sorte qu'un panic dans un thread ne cascade pas en crashs sur les acquisitions de lock suivantes.

**Injection d'exemplars :**

`inject_exemplars()` itère sur le texte rendu ligne par ligne. Pour les lignes `perf_sentinel_findings_total{...}`, il parse les labels `type` et `severity` pour trouver l'exemplar correspondant. Pour les lignes `perf_sentinel_io_waste_ratio`, il ajoute l'exemplar de gaspillage.

Le format suit la spécification OpenMetrics : `metric{labels} value # {trace_id="abc123"}`. Quand des exemplars sont présents, le header `Content-Type` passe de `text/plain; version=0.0.4` (Prometheus) à `application/openmetrics-text; version=1.0.0` (OpenMetrics) pour que la source de données Prometheus de Grafana puisse reconnaître et afficher les liens d'exemplars.

## Ingestion pg_stat_statements

`ingest/pg_stat.rs` fournit un chemin d'analyse autonome pour les exports `pg_stat_statements` de PostgreSQL. Contrairement à l'ingestion basée sur les traces, ces données n'ont pas de `trace_id` ni de `span_id` -- elles ne peuvent pas alimenter le pipeline de détection N+1/redondant. Elles fournissent un classement de hotspots et une référence croisée avec les findings de traces.

### Décisions de conception

**Séparé de `IngestSource` :** le trait `IngestSource` retourne `Vec<SpanEvent>`, mais les données `pg_stat_statements` ne correspondent pas à `SpanEvent` (pas de trace_id, span_id, ni timestamp). Elles produisent leur propre type `PgStatReport` avec des classements.

**Auto-détection du format :** suit le même pattern d'heuristique byte-level que `json.rs`. Si le premier octet non-espace est `[` ou `{`, parse en JSON ; sinon, parse en CSV. Pas de crate csv externe -- le parseur CSV gère le quoting RFC 4180 manuellement (champs entre guillemets doubles, `""` échappé).

**Réutilisation de la normalisation SQL :** chaque requête passe par `normalize::sql::normalize_sql()` pour produire un template comparable avec les findings basés sur les traces.

### Référence croisée

`cross_reference()` accepte `&mut [PgStatEntry]` et `&[Finding]`. Il construit un `HashSet` des templates de findings et marque les entrées dont le `normalized_template` correspond. Complexité O(n + m) où n = entrées, m = findings. Le flag `seen_in_traces` permet à la CLI de mettre en évidence les requêtes présentes dans les deux sources de données.
