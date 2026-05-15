# Rapport public périodique

`perf-sentinel disclose` produit un document JSON unique qui agrège les findings collectés sur une période calendaire (typiquement un trimestre) dans une forme adaptée à la transparence publique. La sortie est vérifiable par hash, versionnée par schéma, et distincte du JSON `Report` par batch consommé par le dashboard HTML.

La subcommand est ajoutée en v0.6.x et remplace les recettes de disclosure ad hoc antérieures.

## Quel intent choisir

| intent     | validation | publiable  | usage typique                             |
|------------|------------|------------|-------------------------------------------|
| `internal` | aucune     | non        | brouillons de dev, tests à blanc          |
| `official` | stricte    | oui        | publication trimestrielle de transparence |
| `audited`  | réservé    | pas encore | réservé pour une release future           |

`audited` est réservé pour une release future. Le schéma JSON accepte la valeur pour la compatibilité ascendante, mais la CLI sort avec le code 2 ("audited intent is reserved for a future release, use 'internal' or 'official' instead") et le daemon refuse de démarrer avec `intent = "audited"` configuré.

Pour l'intent `official`, le validator refuse également les rapports sous 75% de couverture runtime-calibrated. Le dénominateur est `runtime_windows_count + fallback_windows_count` : chaque fenêtre de scoring archivée par le daemon dans la période demandée est classée runtime (attribution énergie per-service présente) ou fallback (proxy I/O share comme substitut). Une couverture sous 75% signifie qu'au-delà du quart des fenêtres de la période ne portait pas d'attribution per-service, donc la part proxy commence à dominer les totaux et la revendication "official" perd une couverture per-service significative. La justification empirique du seuil exact 75% (versus 50% ou 90%) est documentée dans [docs/FR/design/08-PERIODIC-DISCLOSURE-FR.md](design/08-PERIODIC-DISCLOSURE-FR.md#le-seuil-de-75-de-calibration-runtime).

## Granularité

perf-sentinel publie les rapports à deux niveaux de granularité, contrôlés par `--confidentiality`. Le validator refuse de publier un rapport `confidentiality = public` qui contiendrait des entrées G1, et inversement.

- **G1** (Granularity level 1, "détail interne"). Activé par `--confidentiality internal`. Chaque entrée `applications[*]` porte un tableau `anti_patterns: [...]` complet ventilant chaque type d'anti-pattern détecté sur ce service avec occurrences, énergie gaspillée estimée et carbone gaspillé. À utiliser pour les décisions d'optimisation internes, pas pour la publication publique : le détail par pattern expose des signaux de performance internes qu'un opérateur peut ne pas vouloir diffuser.
- **G2** (Granularity level 2, "agrégat public"). Activé par `--confidentiality public`. Chaque entrée `applications[*]` porte les mêmes totaux service-level (énergie, carbone, score d'efficacité) mais remplace le tableau par un seul entier `anti_patterns_detected_count`. Adapté à la publication sur l'URL de transparence d'une organisation.

## Flags CLI

`perf-sentinel disclose` accepte les flags suivants :

- `--intent <internal|official|audited>` (requis). `audited` est réservé pour une release future, la CLI le refuse aujourd'hui avec exit code 2.
- `--confidentiality <internal|public>` (requis). Pilote G1 vs G2 granularité, voir ci-dessus.
- `--period-type <calendar-quarter|calendar-month|calendar-year|custom>` (requis). Hint sur la sémantique période pour les consommateurs downstream. `custom` utilise `--from` et `--to` tel quel, choix correct pour des fenêtres non-alignées (par exemple un pilote de 6 semaines).
- `--from <YYYY-MM-DD>` et `--to <YYYY-MM-DD>` (requis, inclusifs). Dates calendaires UTC.
- `--input <PATH>` (requis, répétable). Chaque chemin peut être un fichier `.ndjson` unique, un répertoire dont les fichiers `*.ndjson` sont unionés (triés par nom), ou un glob expansé par le shell. perf-sentinel n'expanse pas les globs lui-même, donc `--input archive/2026Q1/*.ndjson` marche en shell mais échoue en `exec` direct sans expansion shell. Dans les runners CI qui execent le binaire directement, préférer un répertoire ou un fichier unique.
- `--output <PATH>` (requis). Où écrire `perf-sentinel-report.json`.
- `--org-config <PATH>` (requis pour `intent = "official"`). Le TOML statique organisation / méthodologie / scope décrit dans la section précédente.
- `--emit-attestation <PATH>` (optionnel). Quand fixé, écrit aussi le sidecar statement in-toto v1 à ce chemin. Nécessaire pour le workflow de signature.
- `--strict-attribution` (optionnel). Par défaut, perf-sentinel range les spans sans attribution `service.name` dans un service synthétique `_unattributed`. Ce bucket contribue aux totaux agrégés mais est exclu de la ventilation per-service. Avec `--strict-attribution`, l'appel disclose refuse de produire un rapport si une fenêtre porte des spans non-attribués, listant les timestamps offendants dans le message d'erreur. À utiliser pour une divulgation officielle quand on veut asserter que 100% des opérations mesurées ont été correctement attribuées.

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
# Réservé pour 0.8.0 (divulgations périodiques déclenchées par le
# daemon), actuellement un no-op. Renseigner ce champ aujourd'hui
# émet un warning au démarrage. Les rapports sont produits
# exclusivement via `perf-sentinel disclose --output`.
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

# 2. Signer l'attestation avec cosign contre Sigstore public. Le
#    fichier produit à l'étape 1 est déjà un Statement in-toto v1
#    complet, donc on le signe directement avec `cosign sign-blob`.
#    L'issuer OIDC (flow navigateur ou token GitHub Actions)
#    enregistre l'identité signataire. Le bundle inclut la preuve
#    d'inclusion Rekor.
#    Ne PAS utiliser `cosign attest-blob --predicate attestation.intoto.jsonl` :
#    cette commande traite son entrée comme un predicate brut et la
#    wrappe dans un nouveau Statement, produisant une entrée
#    double-wrappée permanente dans le journal Rekor public.
cosign sign-blob \
    --bundle bundle.sig \
    --new-bundle-format \
    attestation.intoto.jsonl

# 3. Patcher integrity.signature dans report.json pour que les
#    vérifieurs trouvent le bundle et l'entrée Rekor (voir
#    "Édition de integrity.signature" plus bas pour le schéma et
#    le helper jq). Puis bumper report_metadata.integrity_level
#    de "hash-only" à "signed" (ou "signed-with-attestation" si le
#    binaire producteur porte une provenance SLSA). Une future
#    subcommand `perf-sentinel sign` automatisera cette étape.

# 4. Publier report.json, attestation.intoto.jsonl, bundle.sig à
#    votre URL de transparence.
```

### Édition de integrity.signature

Après que l'étape 2 réussit, `report.json` a toujours
`integrity.signature = null`. Un consommateur qui lance
`verify-hash` verrait "Signature: not provided" et traiterait le
rapport comme PARTIAL. L'étape 3 remplit les champs de locator
pour que le consommateur trouve le bundle et le vérifie.

Les sept champs et la source de chaque valeur :

| Champ             | Où lire la valeur                                                                                                                                                                                 |
|-------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `format`          | constante `"sigstore-cosign-intoto-v1"` pour ce schéma                                                                                                                                            |
| `bundle_url`      | URL où vous publierez `bundle.sig` à l'étape 4                                                                                                                                                    |
| `signer_identity` | sortie cosign à l'étape 2, ligne `Successfully verified SCT...` ou `tlog entry... signed by`. Aussi lisible via `cosign verify-blob --certificate-identity-regexp '.*' ... 2>&1 \| grep identity` |
| `signer_issuer`   | même source que `signer_identity`, l'URL OIDC issuer enregistrée à côté                                                                                                                           |
| `rekor_url`       | l'instance Rekor utilisée (`https://rekor.sigstore.dev` pour Sigstore public, ou la valeur de `[reporting.sigstore] rekor_url` pour une instance privée)                                          |
| `rekor_log_index` | sortie cosign à l'étape 2, ligne `tlog entry created with index: X`. Ou via `curl <rekor_url>/api/v1/log/entries?logIndex=X` pour confirmer                                                       |
| `signed_at`       | timestamp de l'entrée Rekor, ISO 8601 UTC                                                                                                                                                         |

Exemple before / after sur une divulgation fraîche :

```json
// Avant l'étape 2 (état immédiatement après disclose --emit-attestation)
"integrity": {
  "content_hash": "sha256:abc123...",
  "binary_hash": "sha256:def456...",
  "binary_verification_url": "https://github.com/robintra/perf-sentinel/releases/tag/v0.7.0",
  "trace_integrity_chain": null,
  "signature": null,
  "binary_attestation": null
}
```

```json
// Après l'étape 3 (après cosign sign-blob réussi et locators collés)
"integrity": {
  "content_hash": "sha256:abc123...",
  "binary_hash": "sha256:def456...",
  "binary_verification_url": "https://github.com/robintra/perf-sentinel/releases/tag/v0.7.0",
  "trace_integrity_chain": null,
  "signature": {
    "format": "sigstore-cosign-intoto-v1",
    "bundle_url": "https://transparency.example.fr/bundle.sig",
    "signer_identity": "robin.trassard@example.fr",
    "signer_issuer": "https://accounts.google.com",
    "rekor_url": "https://rekor.sigstore.dev",
    "rekor_log_index": 123456789,
    "signed_at": "2026-05-15T09:00:00Z"
  },
  "binary_attestation": null
}
```

Et dans `report_metadata` :

```diff
-  "integrity_level": "hash-only"
+  "integrity_level": "signed"
```

(Utiliser `"signed-with-attestation"` au lieu de `"signed"` quand
le binaire producteur porte aussi une provenance SLSA.)

### Le content hash reste valide

Le `content_hash` n'a **pas** besoin d'être recalculé après
l'étape 3. La forme canonique utilisée par `compute_content_hash`
blank quatre champs avant le hash : `integrity.content_hash`,
`integrity.signature`, `integrity.binary_attestation`, et
`report_metadata.integrity_level`. La liste vit dans
`POST_SIGN_FIELDS`
(`crates/sentinel-core/src/report/periodic/hasher.rs`) et
l'invariance est garantie par le test
`hash_is_invariant_under_post_sign_locator_addition`. Donc un
consommateur qui recompute le hash sur le rapport post-étape-3
obtient la même valeur que l'opérateur à l'étape 1.

**Ne pas recompute** `content_hash` après édition. Le faire
produit un hash frais, casse la forme canonique, et un vérifieur
verra un mismatch.

### Helper jq

**Avertissement.** Le snippet ci-dessous est indicatif, pas canonique.
Il diverge du schéma de [Édition de integrity.signature](#édition-de-integritysignature)
sur quatre champs :

- `signer_identity` : parse `Successfully signed by ...` depuis la
  stdout cosign, qui est le wording émis par `cosign sign` pour les
  images conteneur mais pas toujours par `cosign sign-blob` (3.0+
  l'omet). La valeur canonique est le sujet OIDC embarqué dans le
  certificat de signature (lire via
  `cosign verify-blob --certificate-identity-regexp '.*'`, ou inspecter
  `bundle.sig` directement avec
  `jq -r '.verificationMaterial.certificate.rawBytes' bundle.sig | base64 -d | openssl x509 -inform DER -noout -ext subjectAltName`).
- `signer_issuer` : codé en dur à `https://accounts.google.com`. La
  valeur canonique est l'URL de l'issuer OIDC inscrite dans le
  certificat de signature, à aligner avec votre provider (Google,
  GitHub Actions, OIDC custom).
- `rekor_log_index` : parse `tlog entry created with index` depuis la
  stdout cosign, que `cosign sign-blob` 3.0+ n'émet plus. `LOG_INDEX`
  est donc vide et le snippet retombe sur `rekor_log_index: 0`. La
  valeur canonique est dans `bundle.sig` lui-même à
  `.verificationMaterial.tlogEntries[0].logIndex`, sans appel API.
- `signed_at` : rempli avec l'horloge locale au moment de l'exécution
  jq. La valeur canonique est l'`integratedTime` de l'entrée Rekor,
  obtenue via `https://rekor.sigstore.dev/api/v1/log/entries?logIndex=<idx>`
  et formatée en ISO 8601 UTC.

Pour une publication destinée à un audit tiers, privilégiez la lecture
de ces quatre champs depuis le bundle et depuis l'entrée Rekor plutôt
que depuis l'état local. Le helper reste utile pour un essai à blanc
interne où les quatre valeurs sont inspectées pour leur plausibilité,
pas leur provenance.

Le pattern est répétitif et facile à scripter. En attendant que
`perf-sentinel sign` arrive (prévu 0.7.x), ce workflow jq capture
les champs depuis la sortie cosign et patche le rapport en une
passe :

```bash
# Signer et capturer la sortie cosign pour parsing
cosign sign-blob \
    --bundle bundle.sig \
    --new-bundle-format \
    attestation.intoto.jsonl 2>&1 | tee cosign.log

# Extraire l'index tlog depuis la sortie cosign. Format :
# "tlog entry created with index: 123456789"
LOG_INDEX=$(grep "tlog entry created with index" cosign.log \
            | awk '{print $NF}')

# Extraire l'identité signataire depuis le log cosign. Format
# dépend de l'issuer : email pour OIDC Google, URL de workflow
# pour GitHub Actions.
SIGNER=$(grep "Successfully signed" cosign.log \
         | sed 's/.*by //' | tr -d '"')

# Choisir l'issuer qui matche votre provider OIDC.
ISSUER="https://accounts.google.com"  # ou token.actions.githubusercontent.com

# Patcher report.json avec les sept champs de locator et bumper
# integrity_level. Ajuster bundle_url à votre host de transparence.
jq --arg url "https://transparency.example.fr/bundle.sig" \
   --arg sig "$SIGNER" \
   --arg issuer "$ISSUER" \
   --arg idx "$LOG_INDEX" \
   --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
   '.integrity.signature = {
     format: "sigstore-cosign-intoto-v1",
     bundle_url: $url,
     signer_identity: $sig,
     signer_issuer: $issuer,
     rekor_url: "https://rekor.sigstore.dev",
     rekor_log_index: ($idx | tonumber),
     signed_at: $ts
   } | .report_metadata.integrity_level = "signed"' \
   report.json > report-signed.json && mv report-signed.json report.json
```

C'est un workaround intérimaire. `perf-sentinel sign` remplacera
la combinaison bash + jq par une seule subcommand quand elle
shippera.

Les opérateurs qui font tourner une instance Rekor privée fixent
`[reporting.sigstore] rekor_url = "..."` dans leur config
perf-sentinel et passent la même URL à `cosign --rekor-url`.
`verify-hash` refuse les bundles signés avec
`cosign sign-blob --no-tlog-upload`, parce que de tels bundles
n'ont pas de preuve d'inclusion Rekor. Toujours signer sans ce
flag pour les rapports destinés à la transparence publique.

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
    --bundle bundle.sig \
    --expected-identity release@example.fr \
    --expected-issuer https://accounts.google.com

# Mode distant : fetch le rapport et les sidecars par convention HTTPS.
perf-sentinel verify-hash \
    --url https://example.fr/perf-sentinel-report.json \
    --expected-identity release@example.fr \
    --expected-issuer https://accounts.google.com
```

Les deux exemples passent `--expected-identity` et `--expected-issuer`
parce que c'est le défaut sûr : sans ces flags, `verify-hash` refuse
d'invoquer cosign et retourne `Status::Fail` sur le slot signature.
Voir [Vérification d'identité](#vérification-didentité) plus bas pour
les trois modes et leur sémantique. Réservez `--no-identity-check` à
une auto-vérification interne avant publication.

`verify-hash` chaîne trois vérifications : recompute déterministe
du content hash (Rust pur, toujours lancé), signature Sigstore
(`cosign verify-blob`), et provenance SLSA du binaire
(résumé métadonnée plus une commande `gh attestation verify` qui
pointe vers le binaire dans `integrity.binary_verification_url`).

Codes de sortie :

| Code | Signification                                                                                                                                                       |
|------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `0`  | TRUSTED (content hash matché ET signature vérifiée ok)                                                                                                              |
| `1`  | UNTRUSTED (un check a retourné un échec dur : mismatch de hash, signature invalide, attestation invalide, identité non-conforme)                                    |
| `2`  | PARTIAL (pas d'échec dur mais au moins un check n'a pas pu se compléter : cosign absent, `gh` CLI absent, métadonnée de signature absente, sidecars manquants)      |
| `3`  | INPUT_ERROR (fichier rapport illisible, JSON invalide, ou `--report` / `--url` manquant)                                                                            |
| `4`  | NETWORK_ERROR (mode `--url` uniquement : fetch HTTP échoué, schéma refusé, body au-dessus du cap de taille)                                                         |

Un gate scripté `verify-hash && deploy` bloque sur tout code
non-zéro et rejette donc PARTIAL aussi. Une enveloppe qui
distingue PARTIAL (2) de UNTRUSTED (1) peut différencier un
outil manquant d'une tentative de tamper.

### Convention URL des sidecars en mode `--url`

`verify-hash --url <REPORT_URL>` fetch trois fichiers depuis le
même répertoire, avec des **noms fixes** :

```
https://example.fr/<nom-du-rapport>             (le rapport)
https://example.fr/attestation.intoto.jsonl     (sidecar statement in-toto)
https://example.fr/bundle.sig                   (sidecar bundle cosign)
```

Les noms de sidecars ne sont pas dérivés du nom de fichier du
rapport : ils sont littéralement `attestation.intoto.jsonl` et
`bundle.sig`. Un opérateur qui publie un rapport doit utiliser
ces noms exacts au même URL prefix pour que `verify-hash --url`
les trouve automatiquement. Une révision future pourrait surfacer
les URLs dans `integrity.signature.bundle_url` pour rendre la
convention explicite par rapport, mais ce n'est pas le
comportement actuel.

### Vérification d'identité

`verify-hash` exige du consommateur qu'il déclare quelle identité
aurait dû signer le rapport. Trois modes :

- `--expected-identity <ID> --expected-issuer <URL>` : cosign
  vérifie que le bundle a été émis par exactement cette identité
  OIDC. Les valeurs viennent de la connaissance préalable de
  l'auditeur de l'organisation publiante (le rapport déclare ces
  valeurs dans `integrity.signature.signer_identity` /
  `.signer_issuer` mais traiter ces déclarations comme
  authoritatives serait de l'autosigning : n'importe quel
  détenteur d'un compte GitHub ou Google peut publier un bundle
  revendiquant une identité).
- `--no-identity-check` : cosign vérifie l'intégrité
  cryptographique sans vérifier l'identité. Utile pour un
  self-check interne avant publication, mais explicitement loggé
  comme PARTIAL parce que le signataire n'est pas vérifié.
- Aucun flag passé : `verify-hash` refuse d'invoquer cosign et
  retourne `Status::Fail` sur le slot signature. C'est le défaut
  safe et force un consommateur externe à déclarer son intention.

### Provenance build du binaire

`integrity.binary_hash` est le SHA-256 du binaire perf-sentinel
qui a produit le rapport. Pour une divulgation officielle, la
valeur devrait matcher un binaire de release officiel publié sur
les GitHub releases du projet. Les opérateurs qui buildent
perf-sentinel depuis les sources peuvent quand même produire des
rapports officiels, mais leur `binary_hash` ne matchera aucune
release publiée. Dans ce cas `integrity.binary_attestation` est
absent (pas de provenance SLSA pour un build local) et
`verify-hash` reporte `[--] Binary attestation: not provided`.
L'`integrity_level` est `signed`, pas `signed-with-attestation`.
Pour un maximum de confiance sur une publication, utiliser le
binaire de release qui matche le tag déclaré dans
`integrity.binary_verification_url`.

## Calculer un content hash canonique avec `hash-bake` (0.7.2+)

Pour les fixtures de test et les workflows de debug où vous avez besoin d'un rapport dont le `content_hash` correspond déjà à ce que perf-sentinel produirait, utilisez `hash-bake` :

```bash
perf-sentinel hash-bake --report input.json --output output.json
```

`hash-bake` lit le rapport à `--report`, calcule le `content_hash` canonique (en appliquant le blanching `POST_SIGN_FIELDS` défini pour la version du schema), écrit le hash dans `integrity.content_hash`, et sauvegarde le résultat à `--output`. Le même chemin que `--report` est accepté pour un baking en place, avec un temp+rename atomique qui évite toute corruption partielle.

Cette commande est destinée à :

- Générer des fixtures de test avec un hash canonique valide (par exemple pour des suites qui exercent `verify-hash` en sortie TRUSTED ou PARTIAL).
- Déboguer un rapport dont le hash a divergé du canonique (typiquement après des édits manuels sur des champs hors `POST_SIGN_FIELDS`).

Les rapports signés (`integrity.signature` non-null) sont rejetés par défaut. Le re-baking n'invalide pas la signature, puisque la forme canonique blanchit la signature de toute façon, mais l'opérateur doit confirmer l'intention via `--allow-signed`.

`hash-bake` ne modifie pas `integrity.signature`, ne modifie pas `integrity.binary_attestation`, et ne modifie pas `report_metadata.integrity_level`. Il n'écrit que `integrity.content_hash`.

Codes de sortie :

| Code | Signification |
|------|---------------|
| 0 | Hash baked, fichier écrit. |
| 1 | Refusé : le rapport porte une signature et `--allow-signed` n'a pas été passé. Aucun fichier de sortie écrit. |
| 3 | Erreur d'entrée : rapport illisible, JSON invalide, ou écriture impossible. |

## Erreurs courantes

- `Error: audited intent is reserved for a future release, use 'internal' or 'official' instead` : basculer `--intent` sur `internal` ou `official`.
- `no archived reports fell within the requested period` : l'archive contient des lignes mais aucune ne correspond à la fenêtre `--from`/`--to`. Vérifier les timestamps, en particulier autour des changements DST et des frontières de fuseau (l'aggregator filtre sur dates UTC).
- `Error: report validation failed` suivi d'une liste à puces : chaque ligne nomme le champ fautif. Corriger dans le TOML org-config ou dans l'archive source.
- `strict_attribution` activé et une fenêtre sans offenders : retirer le flag ou corriger l'instrumentation par service qui masque les offenders.

## Portée et limites

Le rapport est une estimation directionnelle avec un intervalle d'incertitude multiplicatif `2x`. Il n'est pas de grade réglementaire et inadapté à un reporting CSRD ou GHG Protocol Scope 3. Voir `docs/FR/METHODOLOGY-FR.md` pour la chaîne de calcul complète et les sources de calibration qui resserrent l'intervalle (Scaphandre RAPL, cloud SPECpower, Electricity Maps).
