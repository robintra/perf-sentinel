# Rapport public périodique

`perf-sentinel disclose` produit un document JSON unique qui agrège les findings collectés sur une période calendaire (typiquement un trimestre) dans une forme adaptée à la transparence publique. La sortie est vérifiable par hash, versionnée par schéma, et distincte du JSON `Report` par batch consommé par le dashboard HTML.

La subcommand est ajoutée en v0.6.x et remplace les recettes de disclosure ad hoc antérieures.

## Quel intent choisir

| intent     | validation | publiable  | usage typique                             |
|------------|------------|------------|-------------------------------------------|
| `internal` | aucune     | non        | brouillons de dev, tests à blanc          |
| `official` | stricte    | oui        | publication trimestrielle de transparence |
| `audited`  | réservé    | pas encore | révision future                           |

`audited` est accepté par le schéma JSON pour la compatibilité ascendante, mais la CLI retourne `Error: audited intent is not yet implemented` et sort avec le code 2.

Pour l'intent `official`, le validator refuse également les rapports sous 75% de couverture runtime-calibrated (voir [docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md](design/08-PERIODIC-DISCLOSURE-FR.md#le-seuil-de-75-de-calibration-runtime) pour la justification).

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

## Signer votre divulgation

Les divulgations `intent = "official"` doivent être signées via
Sigstore pour qu'un consommateur puisse vérifier que le fichier a
été publié par votre organisation et n'a pas été modifié. Le
pipeline est opt-in : passer `--emit-attestation <chemin>` à
`disclose` pour obtenir un statement in-toto v1 sidecar, puis
signer ce statement avec `cosign`.

```bash
# 1. Produire le rapport et l'attestation in-toto.
perf-sentinel disclose \
    --intent official \
    --confidentiality public \
    --period-type calendar-quarter \
    --from 2026-01-01 --to 2026-03-31 \
    --input archive/2026Q1/*.ndjson \
    --output report.json \
    --emit-attestation attestation.intoto.jsonl \
    --org-config org.toml

# 2. Signer l'attestation avec cosign contre Sigstore public.
#    L'issuer OIDC (flow navigateur ou token GitHub Actions)
#    enregistre l'identité signataire. Le bundle inclut la preuve
#    d'inclusion Rekor.
cosign attest-blob \
    --type custom \
    --predicate attestation.intoto.jsonl \
    --bundle bundle.sig \
    report.json

# 3. Ajouter le locator signature dans integrity.signature de
#    report.json pour que les vérifieurs trouvent le bundle et
#    l'entrée Rekor, puis bumper report_metadata.integrity_level
#    de "hash-only" à "signed" (ou "signed-with-attestation" si le
#    binaire producteur porte une provenance SLSA). Une future
#    subcommand `perf-sentinel sign` automatisera cette étape.

# 4. Publier report.json, attestation.intoto.jsonl, bundle.sig à
#    votre URL de transparence.
```

Les opérateurs qui font tourner une instance Rekor privée fixent
`[reporting.sigstore] rekor_url = "..."` dans leur config
perf-sentinel et passent la même URL à `cosign --rekor-url`.
Rapports produits sans `--no-tlog-upload` uniquement :
`verify-hash` refuse les bundles sans preuve d'inclusion Rekor.

`verify-hash` lit lui-même `integrity.signature.rekor_url` dans
le rapport vérifié, donc un consommateur qui télécharge une
divulgation publique n'a besoin d'aucune config locale : l'URL
voyage avec le rapport. Pour forcer un Rekor différent au moment
de la vérification (par exemple cross-check une revendication
Rekor public contre une archive privée), invoquer cosign
directement avec son propre flag `--rekor-url` plutôt que via
`verify-hash`. Le rapport reste la source de vérité unique pour
le journal de transparence qui l'a signé.

Voir `docs/FR/design/10-SIGSTORE-ATTESTATION-FR.md` pour la
méthodologie complète, les modes d'échec, et les considérations
privacy sur Rekor public.

## Vérifier un rapport publié

Un tiers vérifie un fichier publié en une commande :

```bash
# Mode local : les trois fichiers sont déjà téléchargés.
perf-sentinel verify-hash \
    --report report.json \
    --attestation attestation.intoto.jsonl \
    --bundle bundle.sig

# Mode distant : fetch le rapport et les sidecars par convention HTTPS.
perf-sentinel verify-hash --url https://example.fr/perf-sentinel-report.json
```

`verify-hash` chaîne trois vérifications : recompute déterministe
du content hash (Rust pur, toujours lancé), signature Sigstore
(`cosign verify-blob-attestation`), et provenance SLSA du binaire
(résumé métadonnée plus une commande `slsa-verifier` qui pointe
vers le binaire dans `integrity.binary_verification_url`). Codes
de sortie : `0` TRUSTED (content hash matché ET signature
vérifiée ok), `1` tout le reste y compris PARTIAL (signature
skippée parce que `cosign` est absent ou les sidecars n'ont pas
été fournis), `2` erreur fichier, `3` erreur réseau. Un gate
scripté `verify-hash && deploy` exige donc les deux couches, pas
juste le hash local.

## Erreurs courantes

- `Error: audited intent is not yet implemented` : basculer `--intent` sur `internal` ou `official`.
- `no archived reports fell within the requested period` : l'archive contient des lignes mais aucune ne correspond à la fenêtre `--from`/`--to`. Vérifier les timestamps, en particulier autour des changements DST et des frontières de fuseau (l'aggregator filtre sur dates UTC).
- `Error: report validation failed` suivi d'une liste à puces : chaque ligne nomme le champ fautif. Corriger dans le TOML org-config ou dans l'archive source.
- `strict_attribution` activé et une fenêtre sans offenders : retirer le flag ou corriger l'instrumentation par service qui masque les offenders.

## Portée et limites

Le rapport est une estimation directionnelle avec un intervalle d'incertitude multiplicatif `2x`. Il n'est pas de grade réglementaire et inadapté à un reporting CSRD ou GHG Protocol Scope 3. Voir `docs/FR/METHODOLOGY-FR.md` pour la chaîne de calcul complète et les sources de calibration qui resserrent l'intervalle (Scaphandre RAPL, cloud SPECpower, Electricity Maps).
