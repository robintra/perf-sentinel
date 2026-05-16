# Procédure de release de perf-sentinel

Ce document décrit la procédure de release de bout en bout pour `perf-sentinel`, applicable à partir de 0.7.0. La procédure inclut un gate de validation obligatoire sur le simulation lab qui bloque le tag d'une version qui n'a pas été éprouvée de bout en bout sur un cluster k3d réel.

Le gate est volontairement pre-flight et opérateur, pas un job CI. Il s'exécute contre un ledger append-only (`release-gate/lab-validations.txt`) qui enregistre chaque validation lab et son verdict. La CI ne peut pas reproduire un run de lab, donc automatiser le gate dans le workflow de release reviendrait à le vider de sa substance.

## Prérequis

- Checkout local du repo `perf-sentinel-simulation-lab` sur un commit récent de `main`.
- Un environnement `k3d` + Docker fonctionnel pour le lab (voir `docs/QUICKSTART.md` du lab).
- Push access sur le repo `perf-sentinel` et une identité de signature de tag. La procédure utilise `git tag -s`, qui passe par GPG par défaut (requiert `user.signingkey` configuré). La signature SSH fonctionne aussi via `git config gpg.format ssh` avec une clé enregistrée comme signataire.
- `gh` CLI authentifié quand vous avez besoin de requêter l'API REST GHCR.

## Procédure

### 1. Ouvrir une branche release

```bash
git checkout main && git pull
git checkout -b release/X.Y.Z
```

La branche est préservée après merge pour la traçabilité des commits qui constituent la release. Ne pas squasher au merge. Convention de nommage : la **branche** est `release/X.Y.Z` (sans `v` initial), le **tag** qui ship plus tard est `vX.Y.Z` (avec `v` initial). `scripts/check-tag-version.sh` accepte les deux formes en entrée.

### 2. Code, tests, bumps de version

Appliquer le travail de feature, fix ou refactor pour la release. Puis bumper toutes les références de version en lockstep.

**Vérifié par `scripts/check-tag-version.sh`** (la CI le lance comme premier job de `release.yml`, lançable aussi en local) :

- `Cargo.toml` workspace `[workspace.package].version`
- Chaque `crates/*/Cargo.toml` : soit `version.workspace = true` (résout vers la version du workspace), soit une version explicite qui doit matcher le tag. Le pin intra-workspace `perf-sentinel-core = { version = "X.Y.Z", path = "..." }` dans `crates/sentinel-cli/Cargo.toml` est aussi vérifié ici.

**Pris en charge par l'opérateur** (pas de gate CI, à auditer à la main avec `grep -RIn "<version_précédente>" docs/ charts/ CHANGELOG.md`) :

- `docs/ci-templates/*` : la constante `PERF_SENTINEL_VERSION` dans `github-actions-baseline.yml`, `github-actions-report-cleanup.yml`, `github-actions.yml`, `gitlab-ci.yml`, et `jenkinsfile.groovy`.
- `docs/CI.md` et `docs/FR/CI-FR.md` : snippets d'exemple qui affichent `perf-sentinel@vX.Y.Z`.
- `docs/schemas/examples/*.json` : uniquement le champ `binary_verification_url` (qui pointe toujours vers la dernière release). Les autres champs de version (`perf_sentinel_version`, `binary_version`, `binary_versions`) sont volontairement figés à la baseline historique de l'exemple.
- `CHANGELOG.md` : déplacer le contenu de `[Unreleased]` sous un nouveau titre `[X.Y.Z]` daté du jour.

Lancer les gates locaux :

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --features daemon -- -D warnings
cargo clippy --workspace --no-default-features -- -D warnings
cargo test --workspace
scripts/check-tag-version.sh vX.Y.Z
```

Les deux invocations de clippy couvrent le feature set par défaut et le build no-default-features, puisque plusieurs modules sont derrière `#[cfg(feature = "...")]`. La CI exécute la même matrice.

### 2.5 Fraîcheur des données de référence GreenOps

C'est un audit opérateur, aucun script ne le vérifie automatiquement. Les données de référence embarquées alimentent le pipeline de scoring carbone et sont livrées comme source Rust (donc couvertes par `cargo test --workspace` en step 2). Les tests garantissent la correction, pas la fraîcheur. Avant de taguer, confirmer les millésimes déclarés dans :

- `crates/sentinel-core/src/score/cloud_energy/table.rs` : deux constantes `_VINTAGE` coexistent dans ce fichier après le refresh CCF du 2026-04-24, toutes deux pointant vers le même snapshot `ccf-coefficients` 2026-04-24. `CCF_LEGACY_VINTAGE` suit les coefficients par architecture importés de `coefficients-{aws,gcp,azure}-use.csv`. `SPECPOWER_VINTAGE` suit les entrées modernes conservées sur leur calcul `SPECpower_ssj 2008` direct (dans les 5 pour cent de CCF ou absentes du CSV du fournisseur). Bumper `CCF_LEGACY_VINTAGE` quand le repo CCF publie un nouveau snapshot daté. Bumper `SPECPOWER_VINTAGE` quand la fenêtre trimestrielle SPECpower est étendue ou que plus de 50 pour cent des entrées modernes sont ré-alignées sur un nouveau snapshot CCF.
- `crates/sentinel-core/src/score/carbon_profiles.rs` : profils horaires de réseau ENTSO-E / EIA / AEMO / Electricity Maps, rafraîchis au moins annuellement. Millésime exposé via `CARBON_PROFILES_VINTAGE`.
- `crates/sentinel-core/src/score/carbon.rs` : constantes PUE par fournisseur (AWS, GCP, Azure, générique), rafraîchies quand un fournisseur publie un nouveau rapport sustainability. Millésime exposé via `PUE_VINTAGE`.

Afficher les trois millésimes en une commande :

```bash
grep -rn 'VINTAGE' crates/sentinel-core/src/score/
```

Si la fenêtre de données ne couvre pas la date de release avec une marge confortable (typiquement : table SPECpower dans les 2 derniers trimestres, profils de réseau dans l'année en cours), soit rafraîchir les données dans cette release (en bumpant aussi la constante `_VINTAGE` correspondante), soit documenter le report dans `CHANGELOG.md` pour que la péremption soit explicite pour les utilisateurs en aval.

### 3. Bumper le chart Helm en lockstep

La version du chart et `appVersion` bougent avec la version applicative :

```bash
# charts/perf-sentinel/Chart.yaml
version: A.B.C        # à bumper à chaque changement du chart
appVersion: "X.Y.Z"   # suit la release perf-sentinel
```

`scripts/check-chart-version-bumped.sh` tourne dans la CI des PR et rejette tout changement de chart sans bump de version et sans entrée `CHANGELOG.md` sous `charts/perf-sentinel/`. `scripts/check-helm-tag-version.sh` validera le tag du chart au moment de la release.

### 4. Valider sur le simulation lab

Pousser la branche release pour préservation :

```bash
git push -u origin release/X.Y.Z
```

L'image Docker est publiée sur GHCR exclusivement par `release.yml` au push d'un tag `v*`, pas par `ci.yml`. Deux options pour obtenir une image dans le cluster lab :

- **Option A (recommandée pour une validation propre)** : builder l'image localement depuis le checkout de la branche release et l'importer dans le cluster k3d :
  ```bash
  docker build -t perf-sentinel:vX.Y.Z-rc .
  k3d image import perf-sentinel:vX.Y.Z-rc -c <cluster-name>
  ```
  Puis pinner les manifests du lab sur le tag chargé localement.
- **Option B** : pousser un pre-release tag (`vX.Y.Z-rc.1`) pour déclencher `release.yml` sur une image candidate. L'image devient disponible sur GHCR en environ 10 minutes. Pinner les manifests du lab sur le digest résultant (résolution via API REST GHCR, voir `docs/TROUBLESHOOTING.md` du lab).

Lancer le lab de bout en bout :

```bash
cd <path-to>/perf-sentinel-simulation-lab

make down
make up
make seed-services
make validate-findings        # attendu : 10/10 scénarios passent
make verify-all-scenarios     # attendu : 24/24 résultats détecteurs concordent
```

Si l'une des étapes échoue, ne pas enregistrer de PASS. Corriger le problème de fond dans `perf-sentinel`, rebuilder l'image, et relancer le lab.

Si tout passe, enregistrer la validation dans le ledger :

```bash
# Depuis le repo lab, produit une ligne tab-separated sur stdout.
scripts/record-validation.sh vX.Y.Z PASS

# Copier la ligne et l'ajouter à release-gate/lab-validations.txt dans
# le repo perf-sentinel. Le ledger est append-only. Ne jamais éditer
# les entrées existantes.
```

### 5. Pre-flight gate

De retour dans le checkout `perf-sentinel` :

```bash
release-gate/check-lab-validation.sh --version vX.Y.Z
```

L'argument `--version` accepte soit `vX.Y.Z` soit `X.Y.Z` (cette dernière forme est normalisée en `vX.Y.Z` en interne), suivant la convention de `scripts/check-tag-version.sh`. La colonne 1 du ledger doit toujours porter le `v` initial (par exemple `v0.7.2`). Sortie attendue en cas de succès :

```
release-gate: PASS for vX.Y.Z dated YYYY-MM-DD (lab commit <sha>, <N>d old, threshold 30d). OK to release.
```

Le gate a trois modes d'échec, chacun avec un remède actionnable :

| Échec                                     | Message                                                          | Remède                                                                                                            |
|-------------------------------------------|------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------|
| Version absente ou seulement entrées FAIL | `no PASS entry for vX.Y.Z in ...`                                | Relancer le lab et ajouter une ligne PASS.                                                                        |
| Dernière entrée PASS trop ancienne        | `latest PASS for vX.Y.Z is N days old ... Threshold is 30 days.` | Relancer le lab sur la branche actuelle et ajouter une entrée PASS fraîche.                                       |
| Fichier ledger absent                     | `ledger ... not found.`                                          | S'assurer que `release-gate/lab-validations.txt` est présent à côté du script, ou définir `LEDGER=/path/to/file`. |

Le seuil d'âge est configurable pour les scénarios de backfill ou d'audit. `--max-age-days 365` accepte des entrées vieilles d'un an. La valeur par défaut de 30 jours est la valeur de travail, ne pas la modifier pour des releases normales.

### 6. Merge et tag

Après que le gate passe :

```bash
git checkout main
git merge release/X.Y.Z --no-ff -m "Merge release/X.Y.Z"
git tag -s vX.Y.Z -m "vX.Y.Z"
git push origin main vX.Y.Z
```

Le push du tag déclenche `.github/workflows/release.yml`. Son premier job relance `scripts/check-tag-version.sh` comme gate de sanité, puis la matrice de build produit les binaires, le job de publish pousse vers crates.io strictement (pas de fallback souple en cas de rate-limit), et le job docker scanne l'image avec Trivy (exit dur sur HIGH ou CRITICAL) avant de pousser le manifest multi-arch sur GHCR et Docker Hub.

La provenance de chaque binaire de release est attestée par `actions/attest-build-provenance` (Sigstore OIDC, keyless), ce qui produit des attestations SLSA Build L3 vérifiables via `gh attestation verify`. La migration depuis `slsa-framework/slsa-github-generator` vers `actions/attest-build-provenance` a atterri en 0.7.1.

### 7. Release du chart Helm

Attendre que l'image GHCR soit disponible (typiquement 5 à 10 minutes après le run du workflow), puis :

```bash
git tag chart-vA.B.C
git push origin chart-vA.B.C
```

Ça déclenche `.github/workflows/helm-release.yml`, qui valide le tag du chart contre `Chart.yaml` via `scripts/check-helm-tag-version.sh`, package le chart, et le publie sur le chart repository GitHub Pages.

### 8. Communication publique

Après que la page de release GitHub est générée et que le chart est live :

- Post LinkedIn avec lien vers les release notes
- Entrée de blog sur le site projet si la release introduit des capacités user-facing
- Reddit, Hacker News, canaux communautaires quand le changement est largement pertinent
- Contacts institutionnels (collaborateurs académiques, clients sous accord de disclosure) pour les releases qui touchent leurs cas d'usage

## Ce que fait le workflow de release

À titre de référence, voici ce que `release.yml` exécute à chaque push de tag `v*` :

1. **check-versions** : `scripts/check-tag-version.sh "${GITHUB_REF_NAME}"` rejette tout mismatch entre le tag et les fichiers de version du workspace (Cargo.toml uniquement, voir le header du script pour le scope exact).
2. **build** (matrice) : cross-compile `perf-sentinel` pour `linux-amd64-gnu`, `linux-amd64-musl`, `linux-arm64-musl`, `macos-arm64`, `windows-amd64`. Les variantes musl utilisent `mimalloc` comme allocateur global (voir `docs/design/07-CLI-CONFIG-RELEASE.md`).
3. **release** : rassemble les artefacts, calcule les checksums SHA-256, atteste la provenance de build via Sigstore (OIDC keyless), et crée la release GitHub avec tous les assets et les notes tirées de `CHANGELOG.md`.
4. **publish-crate** : publie `perf-sentinel-core` puis `perf-sentinel` sur crates.io, attend que l'index se mette à jour, échoue strictement sur timeout au lieu d'émettre un simple avertissement.
5. **docker** : build l'image multi-arch, la scanne avec Trivy (`exit-code: 1` sur CVE HIGH ou CRITICAL), upload le SARIF, puis push sur GHCR et Docker Hub.

Le release gate n'est **jamais** invoqué depuis ce workflow par design. Si une PR ajoute une étape gate à `release.yml`, la rejeter. Le gate valide contre un cluster k3d réel que la CI ne peut pas reproduire, et un check automatisé vide dégraderait silencieusement la garantie du gate.

## Dépannage

**Le gate échoue avec "no PASS entry" alors que le lab vient de tourner.** La ligne issue de `record-validation.sh` est imprimée sur stdout, pas ajoutée automatiquement. Ouvrir `release-gate/lab-validations.txt` et coller la ligne à la main. Vérifier que les séparateurs sont des tabulations, pas des espaces, avec `cat -t release-gate/lab-validations.txt` (sur macOS) ou `cat -A` (sur GNU).

**Le gate échoue avec "is N days old".** Une validation périmée signifie typiquement que la branche release a accumulé des commits depuis le run lab. Relancer le lab sur le dernier commit, ajouter une ligne PASS fraîche, et retenter le gate.

**Le gate imprime `warning: ignoring malformed line N`.** Un append antérieur a été corrompu (tabs cassés, mauvais nombre de colonnes). Ouvrir le ledger, trouver la ligne N, corriger le séparateur ou le nombre de colonnes. Le gate continue de traiter le reste du fichier.

**`check-tag-version.sh` échoue sur `crates/sentinel-cli/Cargo.toml`.** Le script vérifie à la fois le `version` du workspace et le pin intra-workspace sur `perf-sentinel-core`. Les deux doivent être bumpés ensemble.

**`publish-crate` time out en attendant l'index crates.io.** Le job échoue strictement pour éviter une release dans un état partiel. Attendre 5 minutes, puis relancer le job en échec depuis l'UI GitHub Actions. Si la crate est déjà sur l'index, le job le détectera et finira.

**Le scan Trivy flagge un CVE HIGH ou CRITICAL.** Le job `docker` bloque. Vérifier si un rebuild de l'image de base résout le problème (`docker pull` puis relancer le workflow). Si le CVE est dans une dépendance Cargo, bumper la dépendance dans une PR de suivi et couper une release patch. Ne pas contourner le scan.

## Référence du format de ledger

Le ledger à `release-gate/lab-validations.txt` est tab-separated, append-only, et ignore les lignes commençant par `#` ou vides. Chaque entrée a quatre colonnes :

```
<version>\t<lab_commit_sha>\t<YYYY-MM-DD>\t<PASS|FAIL>
```

- `version` : correspond à la forme du tag, avec le `v` initial (par exemple `v0.7.2`). Le gate compare cette colonne à son argument `--version` littéralement.
- `lab_commit_sha` : short SHA de la HEAD du repo lab au moment de la validation, utilisé pour reproduire l'état du lab si une question se pose plus tard.
- `date` : date calendaire UTC à laquelle la validation s'est terminée. Doit être strictement `YYYY-MM-DD`. Le gate rejette tout autre format (les strings floues comme `now` ne sont pas acceptées).
- `verdict` : `PASS` ou `FAIL`. Le gate n'accepte que `PASS`.

Les entrées FAIL ne sont pas strictement requises (le gate les traite comme une entrée manquante), mais les enregistrer dans le ledger préserve la mémoire institutionnelle des raisons pour lesquelles une version candidate a été retenue.
