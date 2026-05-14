# Rapport public périodique

`perf-sentinel disclose` produit un document JSON unique qui agrège les findings collectés sur une période calendaire (typiquement un trimestre) dans une forme adaptée à la transparence publique. La sortie est vérifiable par hash, versionnée par schéma, et distincte du JSON `Report` par batch consommé par le dashboard HTML.

La subcommand est ajoutée en v0.6.x et remplace les recettes de disclosure ad hoc antérieures.

## Quel intent choisir

| intent     | validation | publiable  | usage typique                             |
|------------|------------|------------|-------------------------------------------|
| `internal` | aucune     | non        | brouillons de dev, tests à blanc          |
| `official` | stricte    | oui        | publication trimestrielle de transparence |
| `audited`  | réservé    | pas encore | révision future (sprint 2 / 3)            |

`audited` est accepté par le schéma JSON pour la compatibilité ascendante, mais la CLI retourne `Error: audited intent is not yet implemented` et sort avec le code 2.

## Granularité

- `--confidentiality internal` produit des entrées G1 par application : le détail par anti-pattern (`anti_patterns: [...]`) est inclus.
- `--confidentiality public` produit des entrées G2 : l'agrégat par service plus un seul `anti_patterns_detected_count`, sans le détail par pattern.

Le validator refuse de publier un rapport `confidentiality = public` qui contiendrait des entrées G1.

## Entrées

L'aggregator lit des fichiers NDJSON que le daemon archive à raison d'une enveloppe par fenêtre de scoring :

```json
{"ts":"2026-01-15T14:30:00Z","report":{ ...Report complet... }}
```

Configurer l'archive daemon via :

```toml
[daemon.archive]
path = "/var/lib/perf-sentinel/reports.ndjson"
max_size_mb = 100
max_files = 12
```

Quand le fichier actif dépasse `max_size_mb`, perf-sentinel le renomme en `reports-<timestamp-utc>.ndjson` et ouvre un nouveau fichier. Les anciens fichiers tournés au-delà de `max_files` sont élagués par date de modification.

Les opérateurs qui collectent déjà stdout du daemon via un sidecar peuvent passer le fichier (ou le dossier) résultant à `--input` directement, à condition que chaque ligne soit une enveloppe `{ts, report}`.

## TOML org-config

Les champs statiques organisation/méthodologie/scope vivent dans un fichier TOML que vous committez dans votre repo infra à côté du reste de la config perf-sentinel. Un exemple complet est dans `docs/examples/perf-sentinel-org.toml`. Le même fichier est référencé par `[reporting] org_config_path` quand le daemon doit valider les rapports publiables au démarrage.

## Exemple : brouillon internal (G1)

```bash
perf-sentinel disclose \
  --intent internal \
  --confidentiality internal \
  --period-type calendar-quarter \
  --from 2026-01-01 --to 2026-03-31 \
  --input /var/lib/perf-sentinel/reports.ndjson \
  --output /tmp/perf-sentinel-report.json \
  --org-config /etc/perf-sentinel/org.toml
```

La sortie passe uniquement les vérifications structurelles (pas de validator). `integrity.content_hash` est calculé et stable, mais `integrity.binary_hash` est le SHA-256 du binaire local, pas nécessairement une release publiée.

## Exemple : publication officielle (G2)

```bash
perf-sentinel disclose \
  --intent official \
  --confidentiality public \
  --period-type calendar-quarter \
  --from 2026-01-01 --to 2026-03-31 \
  --input /var/lib/perf-sentinel/reports.ndjson \
  --output /var/www/transparency/perf-sentinel-report.json \
  --org-config /etc/perf-sentinel/org.toml
```

Le validator tourne sur l'ensemble du document. Si un champ requis manque ou sort de la plage, la CLI imprime tous les champs en cause et sort en 2. Corriger l'org-config (ou les données sous-jacentes) puis relancer.

Le chemin de publication recommandé est la racine de votre domaine de transparence :

```
https://transparency.example.fr/perf-sentinel-report.json
```

L'URL de schéma dans `notes.reference_urls.schema` indique quelle version de schéma un consommateur doit récupérer pour valider le fichier.

## Garde-fou côté daemon

Lorsque le daemon est configuré avec `[reporting] intent = "official"`, il refuse de démarrer si le TOML org-config est absent ou échoue le validator de champs statiques. Le message d'erreur liste tous les champs manquants ou invalides en un seul passage pour que l'opérateur corrige tout d'un coup.

```toml
[reporting]
intent = "official"
confidentiality_level = "public"
org_config_path = "/etc/perf-sentinel/org.toml"
disclose_output_path = "/var/lib/perf-sentinel/last-disclosure.json"
disclose_period = "calendar-quarter"
```

`intent = "internal"` (ou l'absence de section) laisse le daemon en mode monitoring sans la barrière de rapport publiable.

## Vérifier un rapport publié

Un tiers peut vérifier un fichier publié en trois commandes :

```bash
# 1. L'id de schéma sous notes.reference_urls.schema pointe vers un JSON
#    Schema v2020-12 publié dans le dépôt perf-sentinel.
jq -r '.notes.reference_urls.schema' perf-sentinel-report.json

# 2. content_hash est reproductible. Les octets canoniques sont produits
#    par perf-sentinel via le formatage des nombres serde_json (shortest
#    round-trip) plus un tri récursif des clés via BTreeMap. Un pipeline
#    jq ne peut pas reproduire ces octets à l'identique pour des valeurs
#    f64 arbitraires (jq émet la repr IEEE-754, serde_json émet le
#    shortest round-trip). L'implémentation de référence reproductible
#    est :
#       perf-sentinel verify-hash <chemin>
#    (livrable sprint 2, en attendant utiliser le binaire perf-sentinel
#    pour recompute, ou accepter le hash tel que livré).

# 3. binary_hash correspond au tag de release perf-sentinel listé dans
#    integrity.binary_verification_url. Télécharger l'artefact de release,
#    le SHA-256 en local, comparer.
jq -r '.integrity.binary_hash' perf-sentinel-report.json
```

## Erreurs courantes

- `Error: audited intent is not yet implemented` : basculer `--intent` sur `internal` ou `official`.
- `no archived reports fell within the requested period` : l'archive contient des lignes mais aucune ne correspond à la fenêtre `--from`/`--to`. Vérifier les timestamps, en particulier autour des changements DST et des frontières de fuseau (l'aggregator filtre sur dates UTC).
- `Error: report validation failed` suivi d'une liste à puces : chaque ligne nomme le champ fautif. Corriger dans le TOML org-config ou dans l'archive source.
- `strict_attribution` activé et une fenêtre sans offenders : retirer le flag ou corriger l'instrumentation par service qui masque les offenders.

## Portée et limites

Le rapport est une estimation directionnelle avec un intervalle d'incertitude multiplicatif `2x`. Il n'est pas de grade réglementaire et inadapté à un reporting CSRD ou GHG Protocol Scope 3. Voir `docs/FR/METHODOLOGY-FR.md` pour la chaîne de calcul complète et les sources de calibration qui resserrent l'intervalle (Scaphandre RAPL, cloud SPECpower, Electricity Maps).
