# Architecture du pipeline et système de types

## Pourquoi un pipeline linéaire

perf-sentinel traite les traces I/O à travers une séquence de transformations : `event -> normalize -> correlate -> detect -> score -> report`. C'est un **pipeline linéaire**, pas une architecture hexagonale (ports et adaptateurs).

Le raisonnement est simple : les données circulent dans une seule direction. Les événements entrent, sont transformés à chaque étape et produisent un rapport. Il n'y a pas de dépendances bidirectionnelles, pas d'événements de domaine, pas de patterns d'interaction complexes. Une architecture hexagonale introduirait de l'indirection par traits entre chaque étape : ajoutant de la charge cognitive, du coût à la compilation et du dispatch dynamique pour zéro bénéfice.

Les traits ne sont utilisés qu'aux **frontières** du pipeline :
- **Entrée :** trait `IngestSource` (JSON, OTLP)
- **Sortie :** trait `ReportSink` (fichier JSON, stdout)

Entre les frontières, chaque étape est une **fonction pure** : elle prend des données en entrée et retourne des données transformées en sortie. Pas d'effets de bord, pas d'état, pas d'objets trait. Cela rend chaque étape testable indépendamment sans mocks : il suffit de construire les données d'entrée et d'asserter sur la sortie.

Ce pattern est courant dans les outils de traitement de données de l'écosystème Rust. Des projets comme [ripgrep](https://github.com/BurntSushi/ripgrep) et [bat](https://github.com/sharkdp/bat) suivent des architectures similaires de "pipeline de transformations".

## La chaîne de types

Chaque étape du pipeline produit un type distinct :

```
SpanEvent  ->  NormalizedEvent  ->  Trace  ->  Finding  ->  Report
 (event.rs)   (normalize/mod.rs) (correlate/) (detect/)  (report/mod.rs)
```

**Pourquoi des types distincts au lieu de mutations en place ?** Chaque étape ajoute de l'information (la normalisation ajoute `template` + `params`, la corrélation regroupe par `trace_id`, la détection produit des findings). Rendre cela explicite dans le système de types signifie que le compilateur garantit qu'aucune étape ne peut utiliser des données d'une étape future. Un `NormalizedEvent` est garanti d'avoir un champ `template` : un `SpanEvent` brut ne l'a pas.

**Transfert de propriété :** `normalize_all()` prend `Vec<SpanEvent>` par valeur (déplacé, pas emprunté). C'est délibéré :
- L'appelant n'a pas besoin des événements bruts après la normalisation
- Évite les annotations de lifetime qui se propageraient à travers chaque étape
- Permet au normaliseur de déplacer les champs (`SpanEvent` est consommé dans `NormalizedEvent.event`)
- Coût zéro : le `SpanEvent` est déplacé dans `NormalizedEvent`, pas cloné

## Sortie déterministe

La détection utilise `HashMap` en interne pour le groupement. Le `HashMap` de Rust utilise un [hasher aléatoire](https://doc.rust-lang.org/std/collections/struct.HashMap.html) (SipHash par défaut), donc l'ordre d'itération varie entre les exécutions. Sans tri, la même entrée pourrait produire des findings dans des ordres différents entre les exécutions.

La fonction partagée `detect::sort_findings()` trie les findings après le scoring avec une clé multi-niveaux :

```rust
pub fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.finding_type.cmp(&b.finding_type)
            .then_with(|| a.severity.cmp(&b.severity))
            .then_with(|| a.trace_id.cmp(&b.trace_id))
            .then_with(|| a.source_endpoint.cmp(&b.source_endpoint))
            .then_with(|| a.pattern.template.cmp(&b.pattern.template))
    });
}
```

Cette fonction est définie dans `detect/mod.rs` et réutilisée par `pipeline::analyze()` et `cmd_inspect` pour garantir un ordre cohérent partout. Cela nécessite que `FindingType` et `Severity` implémentent `Ord`. Le `Ord` dérivé utilise l'ordre de déclaration des variantes, donnant un tri stable : `NPlusOneSql < NPlusOneHttp < RedundantSql < ... < SlowHttp < ExcessiveFanout`.

Les top offenders sont triés de manière similaire (IIS décroissant, ordre alphabétique en cas d'égalité) pour garantir le même rapport pour la même entrée.

## Découpage en workspace

Le projet est découpé en deux crates :
- **sentinel-core** : crate bibliothèque contenant toute la logique du pipeline
- **sentinel-cli** : crate binaire fournissant le point d'entrée CLI

**Pourquoi ce découpage ?** La bibliothèque core peut être embarquée par d'autres projets Rust (ex. un harnais de test personnalisé qui appelle `pipeline::analyze` directement). Le CLI est intentionnellement léger : il parse les arguments avec [clap](https://docs.rs/clap/), charge la configuration et délègue aux fonctions de sentinel-core. Toute la logique métier réside dans la bibliothèque.

La direction de dépendance est unidirectionnelle : `sentinel-cli` dépend de `sentinel-core`, jamais l'inverse.

## Quality gate comme étape séparée

Le quality gate (`quality_gate::evaluate`) est une étape distincte appelée après le scoring, pas intégrée dans la détection ou le reporting. Cette séparation permet :
- À la détection de trouver **tous** les problèmes indépendamment des seuils
- Au scoring de calculer **toutes** les métriques indépendamment du pass/fail
- Au quality gate de prendre une décision binaire pass/fail basée sur des **règles configurables**

Les trois règles (max N+1 SQL critiques, max N+1 HTTP warning+, max ratio de gaspillage) sont évaluées indépendamment. Le gate passe uniquement si toutes les règles passent. C'est plus flexible qu'un seuil de sévérité unique.

## Structure du rapport

La struct `Report` combine quatre sections :

```rust
pub struct Report {
    pub analysis: Analysis,        // duration_ms, events_processed, traces_analyzed
    pub findings: Vec<Finding>,    // triés, enrichis avec green_impact
    pub green_summary: GreenSummary, // IIS, ratio de gaspillage, top offenders, CO2
    pub quality_gate: QualityGate,  // passed + résultats individuels des règles
}
```

**Pourquoi une seule struct ?** La sérialisation JSON avec `serde_json::to_writer_pretty` produit le rapport complet en un seul appel. Les consommateurs (scripts CI, tableaux de bord) parsent un seul objet JSON, pas plusieurs fichiers. L'annotation `#[serde(skip_serializing_if = "Option::is_none")]` sur les champs optionnels (valeurs CO2) garde le JSON propre lorsque ces fonctionnalités ne sont pas configurées.

## Configuration clippy au niveau du crate

`sentinel-core/src/lib.rs` active `clippy::pedantic` globalement :

```rust
#![warn(clippy::pedantic)]
#![allow(clippy::cast_possible_truncation)] // u128 -> u64 pour elapsed_ms
#![allow(clippy::cast_precision_loss)]      // usize -> f64 pour les ratios
#![allow(clippy::similar_names)]            // min_ts/min_ms, max_ts/max_ms sont clairs
```

Les trois exceptions sont documentées avec leur justification. Chaque autre `#[allow]` dans le codebase a un commentaire en ligne expliquant pourquoi.

## Gestion des erreurs

Le projet utilise des erreurs typées partout :
- `ConfigError` : échecs de parsing et validation de la configuration
- `DaemonError` : échecs de parsing d'adresse et de liaison du listener
- `JsonIngestError` : échecs de taille de payload et de parsing JSON
- `JsonReportError` : échecs d'écriture sur stdout

Tous les types d'erreur utilisent [thiserror](https://docs.rs/thiserror/) pour la dérivation des traits `Display` et `Error`. Il n'y a aucun `Box<dyn Error>` ou `.unwrap()` dans le code de production de la bibliothèque. Les quelques appels `.expect()` (enregistrement de métriques Prometheus, création de `NonZeroUsize`) sont dans des chemins infaillibles protégés par une validation en amont et sont documentés avec des commentaires doc `# Panics`.
