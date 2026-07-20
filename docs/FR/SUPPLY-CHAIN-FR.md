# Politique de pinning supply chain

Ce document décrit comment perf-sentinel maintient l'immutabilité de
ses entrées de build. L'objectif est simple : un checkout d'une
release taggée doit produire des runs CI et des binaires identiques
au bit près des semaines ou des années plus tard, et un upstream
compromis ne doit pas pouvoir échanger un tag dans notre dos.

La politique ci-dessous est déjà appliquée sur l'ensemble du dépôt.
Ce document la formalise pour que les futures contributions et les
relecteurs puissent appliquer les mêmes règles aux nouveaux
workflows, Dockerfile et dépendances.

## État

Compliance check au 2026-06-09 :

- **GitHub Actions** : 100 % des lignes `uses:` à travers les 11
  workflows de `.github/workflows/` sont pinnées à un commit SHA de
  40 caractères, avec le tag lisible en commentaire trailing.
- **Dockerfile** : l'image de production est `FROM scratch`, sans
  image de base externe à pinner. La seule action Docker invoquée
  depuis la CI (`zricethezav/gitleaks` dans `ci.yml`) est pinnée par
  digest.
- **Dépendances Cargo** : `Cargo.lock` est commité et tracké. Le
  workspace fait tourner `cargo audit` quotidiennement via
  `.github/workflows/security-audit.yml`. Les advisories acquittées
  avec analyse d'exposition documentée vivent dans `audit.toml`.
- **Permissions** : chaque workflow déclare `permissions:` au
  niveau job (par défaut `contents: read`), avec des scopes plus
  larges opt-in uniquement par job quand c'est nécessaire (release,
  packages, attestations).
- **Dependabot** : configuré pour `github-actions` dans
  `.github/dependabot.yml`, schedule hebdomadaire le lundi, groupé
  par owner upstream pour garder le diff cohérent.

## Règles de pinning

### GitHub Actions

Chaque ligne `uses:` dans un workflow doit référencer un commit SHA
de 40 caractères. Le tag semver va dans un commentaire trailing pour
que les relecteurs puissent lire la version d'un coup d'œil :

```yaml
- uses: actions/checkout@1af3b93b6815bc44a9784bd300feb67ff0d1eeb3  # v6.0.2
```

Pourquoi SHA et pas tags : les attaques supply chain récentes contre
`tj-actions/changed-files` (mars 2025) et incidents similaires ont
tous exploité le fait qu'un tag Git est un pointeur mutable. Un
mainteneur ou un attaquant peut déplacer `v6` vers un nouveau commit
à tout moment, et tous les workflows de la planète qui pinnaient
`@v6` exécutent immédiatement le nouveau code. Un SHA est
content-addressable : le réécrire nécessite une collision SHA-1, ce
qui n'est dans le scope d'aucun attaquant connu.

### Images Docker

Quand un Dockerfile ou un workflow référence une image externe, pin
le digest du contenu :

```dockerfile
FROM golang@sha256:abc...def  # 1.22-alpine
```

Le `Dockerfile` de production utilise `FROM scratch`, il n'y a donc
rien à pinner dans l'image elle-même. Le binaire copié dedans
(`build/linux-${TARGETARCH}/perf-sentinel`) est build depuis ce
dépôt même, avec `Cargo.lock` qui pilote la closure des dépendances.

### Dépendances Cargo

- `Cargo.toml` déclare des ranges semver comme d'habitude.
- `Cargo.lock` est commité et fait foi pour ce que le build compile
  réellement.
- `cargo audit` tourne quotidiennement et sur chaque PR.
- Les advisories acquittées vivent dans `audit.toml` avec un
  paragraphe expliquant pourquoi le code path affecté n'est pas
  exercé. Voir l'entrée `RUSTSEC-2026-0097` pour le format et la
  profondeur d'analyse attendus.

### Permissions des workflows

Le `GITHUB_TOKEN` par défaut a des permissions larges. Les workflows
les abaissent explicitement à `contents: read` au niveau job et
opt-in sur des scopes additionnels uniquement quand c'est nécessaire :

```yaml
jobs:
  build:
    permissions:
      contents: read
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@1af3b93b6815bc44a9784bd300feb67ff0d1eeb3
```

Les jobs de release qui poussent vers GHCR ou créent une release
ajoutent `packages: write`, `contents: write` ou
`attestations: write` selon les besoins. Il n'y a aucun
`permissions: write-all` au top-level dans le dépôt.

## Configuration Dependabot

L'extrait pertinent de `.github/dependabot.yml` :

```yaml
version: 2
updates:
  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
      day: "monday"
    open-pull-requests-limit: 5
    groups:
      ci-actions:
        patterns: ["actions/*", "taiki-e/*", "actions-rust-lang/*"]
      docker-actions:
        patterns: ["docker/*"]
      security-actions:
        patterns: ["github/codeql-action", "github/codeql-action/*"]
```

Les dépendances Cargo sont délibérément exclues de Dependabot : la
combinaison `Cargo.lock` plus `cargo audit` quotidien couvre déjà
l'angle sécurité, et le volume de patch bumps que Dependabot
générerait sur un workspace de 200+ crates est mal rentabilisé pour
un projet de cette taille. Les updates Cargo sont gérées
manuellement via `cargo update` quand c'est nécessaire.

## Commandes de vérification

Ces commandes auditent la posture de pinning du dépôt à tout moment :

```bash
# 1. Trouver toute GitHub Action dont la ref n'est PAS un SHA 40-char. Attendu : 0 hit.
#    Matche tout ce qui suit `@` et qui n'est pas 40 caractères hex : tags semver,
#    noms de branches, `latest`, `HEAD`, refs custom comme `release-1.2`.
grep -rnE 'uses:[[:space:]]+[^@]+@[^[:space:]#]+' .github/workflows/ \
  | grep -vE 'uses:[[:space:]]+[^@]+@[a-f0-9]{40}([[:space:]]|$)'

# 2. Trouver toute ligne FROM dans un Dockerfile qui n'est pas pinnée par digest.
#    Attendu : seulement `FROM scratch` et des digests explicites.
grep -rnE '^FROM[[:space:]]+[^@]+:[^@]+$' \
  Dockerfile* charts/*/Dockerfile* 2>/dev/null

# 3. Lancer cargo audit. Attendu : seuls les ignores documentés sortent.
cargo audit

# 4. Inspecter les permissions actions du repo. Attendu : enabled et
#    `selected` (pas `all`). Nécessite gh CLI authentifié.
gh api repos/robintra/perf-sentinel/actions/permissions
```

## Bumper un pin manuellement

Dependabot gère les bumps de routine. Pour un bump à la main (security
update hors cycle hebdomadaire, ou nouvelle action que Dependabot n'a
pas encore prise en charge), résolvez le SHA via l'API GitHub :

```bash
# Résoudre le SHA pour un tag semver d'une action publiée.
TAG="v6.0.2"
gh api repos/actions/checkout/git/ref/tags/${TAG} --jq '.object.sha'
```

Puis mettez à jour le workflow :

```yaml
- uses: actions/checkout@<le-sha-resolu>  # v6.0.2
```

Mettez toujours à jour le commentaire trailing pour qu'il corresponde
au nouveau tag. Un SHA avec un commentaire périmé est pire que pas
de commentaire.

Pour les images Docker, résolvez le digest avec `docker buildx` :

```bash
docker buildx imagetools inspect <image>:<tag> --format '{{.Manifest.Digest}}'
```

## Procédure de réponse CVE

1. **Détection** : `cargo audit` tourne quotidiennement et poste sur
   les PR. GitHub Security Advisories surface les mêmes données plus
   les alertes ecosystem-specific. Dependabot ouvre des PRs de
   sécurité automatiquement quand un fix est disponible.

2. **Triage** : lisez l'advisory, lancez `cargo tree -i <crate>` pour
   confirmer si la version affectée est réellement compilée dans le
   binaire (le paragraphe `RUSTSEC-2026-0097` dans `audit.toml` est
   l'exemple canonique de la profondeur d'analyse attendue).

3. **Remédiation** : bumpez la dépendance dans `Cargo.toml` si le fix
   est upstream, lancez `cargo update -p <crate>`, vérifiez avec
   `cargo audit`, ouvrez une PR avec préfixe `chore(deps)`.

4. **Acquittement** : si le code path affecté n'est pas exercé,
   ajoutez une entrée dans `audit.toml` avec un paragraphe expliquant
   l'analyse d'exposition et les conditions qui doivent déclencher
   un re-examen. Ne pas ignorer silencieusement.

5. **Divulgation** : voir `SECURITY.md` pour le processus complet de
   divulgation coordonnée et la matrice des versions supportées.

## Provenance SLSA des binaires

### Introduction à Sigstore

Si vous n'avez jamais utilisé Sigstore, cette introduction courte est un préalable pour les références à SLSA, Cosign, Rekor et in-toto qui suivent. Les autres docs perf-sentinel ramènent ici pour les définitions canoniques, voir [docs/FR/REPORTING-FR.md](REPORTING-FR.md#introduction-à-sigstore), [docs/FR/METHODOLOGY-FR.md](METHODOLOGY-FR.md#intégrité-cryptographique-070), [docs/FR/HELM-DEPLOYMENT-FR.md](HELM-DEPLOYMENT-FR.md#chaîne-dapprovisionnement-logicielle), [docs/FR/SCHEMA-FR.md](SCHEMA-FR.md#intégrité).

**Pourquoi Sigstore.** Sigstore est un toolkit open source hébergé par l'Open Source Security Foundation (OpenSSF), maintenu par Google, Red Hat, Chainguard, GitHub et la Linux Foundation. C'est le standard de facto pour les signatures d'artefacts vérifiables dans l'écosystème cloud-native (Kubernetes, Helm, la provenance npm, les attestations PyPI s'appuient toutes dessus). perf-sentinel l'utilise à trois endroits : signature des binaires de release officiels (attestation SLSA Build L3), signature du chart Helm (signature Cosign vérifiable via `cosign verify`), signature des rapports de divulgation périodiques (`integrity.signature` avec preuve d'inclusion Rekor). Trois propriétés motivent ce choix :

1. **Signature sans clé permanente**, aucune clé privée longue durée à gérer ou risquer de divulguer côté signataire.
2. **Un journal public infalsifiable** (Rekor), un tiers peut vérifier de façon indépendante qu'une signature existait à un instant donné.
3. **Libre, open source, auto-hébergeable**, pas de verrouillage propriétaire ni de facturation à la signature.

**Les trois composants.**

- **Cosign** est l'outil CLI exécuté localement (ou par GitHub Actions en CI). Il ouvre un flow OIDC dans le navigateur, signe l'artefact, et envoie la signature à Sigstore.
- **Fulcio** est l'autorité de certification. Il consomme le token OIDC obtenu par cosign (preuve d'identité : email, URL d'un workflow GitHub, ...) et émet un certificat X.509 à durée courte (10 minutes) lié à cette identité. Fulcio ne voit jamais la clé privée du signataire.
- **Rekor** est le journal de transparence public. Il enregistre la signature à côté du certificat Fulcio, retourne une preuve d'inclusion, et expose l'entrée à un log index stable. Les entrées passées ne peuvent pas être réécrites silencieusement.

**Qui signe avec quelle clé.** Cosign génère un nouveau couple de clés éphémère juste avant la signature. Fulcio émet un certificat de 10 minutes qui lie la moitié *publique* de ce couple à l'identité OIDC. Une fois la signature uploadée vers Rekor, le couple de clés est détruit. Il ne reste que la signature, le certificat et l'entrée Rekor, ce dont un vérifieur a exactement besoin.

**L'identité OIDC** est le sujet du certificat Fulcio, remonté comme `signer_identity` + `signer_issuer` dans tout document qui enregistre la signature. Pour un workflow GitHub Actions de release, l'identité est l'URL du workflow (`https://github.com/robintra/perf-sentinel/.github/workflows/release.yml@refs/tags/...`) et l'issuer est `https://token.actions.githubusercontent.com`. Pour un individu qui signe localement avec un compte Google, l'identité est l'email et l'issuer est `https://accounts.google.com`. Les consommateurs doivent épingler la regex d'identité attendue et l'issuer dans leur politique de vérification.

**Limite connue : migration de provider OIDC.** L'URL de l'issuer est inscrite dans le certificat, donc enregistrée dans Rekor. Si l'organisation productrice change plus tard de provider d'identité, les signatures passées restent valides mais les nouvelles signatures porteront une valeur `signer_issuer` différente. Les politiques de vérification qui épinglent un issuer spécifique devront être mises à jour, sinon elles rejetteront les nouvelles signatures comme non fiables. Anticiper la politique d'épinglage en prévision des migrations de provider.

**Termes connexes que vous rencontrerez dans les commandes supply-chain de perf-sentinel.** Des one-liners seulement, les définitions complètes sont dans les specs liées.

- **OIDC (OpenID Connect)** est un protocole d'identité posé sur OAuth 2.0. Dans ce workflow, c'est la manière dont cosign prouve "ce signataire est `user@example.org`" (ou "c'est le workflow release perf-sentinel sur le tag v0.7.1") à Fulcio. [Spec](https://openid.net/specs/openid-connect-core-1_0.html).
- **in-toto v1 statement** est une spécification OpenSSF ouverte pour les attestations de chaîne d'approvisionnement logicielle. Une enveloppe JSON qui apparie le hash d'un artefact avec une *claim* typée sur celui-ci. La provenance SLSA et l'attestation de divulgation périodique sont toutes deux des statements in-toto en interne. Cosign signe le statement, pas l'artefact brut, ce qui permet aux vérifieurs de chaîner la confiance depuis le hash de l'artefact vers le statement in-toto, puis vers la signature cosign, puis vers le certificat Fulcio. [Spec](https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md).
- **Bundle (`bundle.sig`)** est le fichier JSON que cosign écrit au moment de la signature. Il rassemble la signature, le certificat Fulcio et la preuve d'inclusion Rekor dans un seul artefact, ce qui permet une vérification totalement hors ligne plus tard (un consommateur valide contre la clé publique Rekor sans avoir à réinterroger Rekor en direct).
- **SLSA (Supply-chain Levels for Software Artifacts)** est un framework OpenSSF séparé qui décrit *comment* un artefact a été construit (commit source, builder, workflow). Les binaires et charts Helm perf-sentinel portent des attestations SLSA Build L3 produites par `actions/attest-build-provenance`. Le niveau L3 demande une signature Sigstore OIDC plus une isolation du builder, deux propriétés qu'un runner GitHub-hosted fournit. [Spec](https://slsa.dev/spec/v1.0/).
- **SBOM (Software Bill of Materials)** est un inventaire structuré des dépendances d'un artefact. perf-sentinel publie un SBOM au format SPDX attesté sous le prédicat in-toto SPDX, donc les consommateurs le vérifient de la même manière qu'ils vérifient la signature Cosign. [Spec SPDX](https://spdx.dev/specifications/), [prédicat in-toto SPDX](https://github.com/in-toto/attestation/blob/main/spec/predicates/spdx.md).
- **CT log (Certificate Transparency)** est le pattern plus large dont Rekor s'inspire. L'instance Rekor publique Sigstore est sur `rekor.sigstore.dev`. Les opérateurs aux exigences plus strictes peuvent faire tourner une instance privée.

### Workflow

Depuis v0.7.1, chaque binaire de release officiel perf-sentinel porte
une attestation SLSA Build L3. L'attestation est générée par GitHub
Actions via `actions/attest-build-provenance` (maintenu sous l'org
GitHub `actions/`) et stockée dans l'API attestations GitHub
associée à ce dépôt. Elle **n'est plus** publiée comme asset du
Release GitHub.

La 0.7.1 migre depuis l'outillage précédent,
`slsa-framework/slsa-github-generator@v2.1.0`, en maintenance de facto
depuis le 24 février 2025 (15 mois sans release au moment de la
migration, toutes les actions internes encore sur Node.js 20 alors
que les runners GitHub-hosted basculent sur Node 24 par défaut le
2 juin 2026). La nouvelle pipeline préserve le contrat SLSA Build
Provenance, supprime l'asset release `multiple.intoto.jsonl` (les
attestations vivent désormais dans l'API attestations), et fait
passer le niveau revendiqué de L2 à L3, puisque
`actions/attest-build-provenance` produit une attestation niveau 3
par construction (provenance signée via OIDC Sigstore, isolation du
builder sur runner GitHub-hosted).

Vérifier un binaire téléchargé :

```bash
gh attestation verify perf-sentinel-linux-amd64 \
  --owner robintra \
  --repo perf-sentinel
```

Une vérification réussie confirme que le binaire vient d'un tag de
release de ce dépôt, construit par GitHub Actions, pas par un tiers.
Combiner avec la subcommand `verify-hash` sur un rapport périodique
pour vérifier la chaîne complète :
`source -> SLSA -> binaire -> rapport -> signature Sigstore`.

**Prérequis** : `gh` CLI 2.49+ côté consommateur (les versions
antérieures n'implémentent pas `gh attestation verify`). La même
vérification peut être réalisée via les SDK clients Sigstore
directement contre l'API attestations GitHub pour les outils qui
ne peuvent pas dépendre de `gh`.

**Note de migration consommateur** : un binaire 0.6.x ou 0.7.0
embarque toujours le `multiple.intoto.jsonl` legacy et se vérifie
via `slsa-verifier verify-artifact`. Le chemin de vérification
legacy est conservé sur ces tags existants, seul 0.7.1+ requiert la
nouvelle commande.

## SBOM des binaires et données d'audit embarquées

Au-delà de la provenance SLSA, chaque release de binaires porte deux
artefacts supplémentaires, dans la même forme que le SBOM du chart Helm.

**Données `cargo-auditable` embarquées.** Chaque binaire de release est
construit avec `cargo auditable build`, donc sa liste de dépendances résolue
est embarquée dans le binaire lui-même. Auditez l'artefact livré directement,
pas seulement le `Cargo.lock` du dépôt :

```bash
cargo audit bin perf-sentinel-linux-amd64
```

**SBOM SPDX.** Chaque release publie `perf-sentinel-sbom.spdx.json`, un SBOM
SPDX que Syft dérive de ces données embarquées et atteste sous le prédicat
SPDX (`https://spdx.dev/Document/v2.3`) contre le binaire Linux amd64, le même
prédicat que le chart. Vérifiez l'attestation contre le binaire (le SBOM est
le prédicat de l'attestation, le binaire en est le sujet) :

```bash
gh attestation verify perf-sentinel-linux-amd64 \
  --repo robintra/perf-sentinel \
  --predicate-type https://spdx.dev/Document/v2.3
```

Le SBOM est dérivé du binaire Linux amd64. Les quatre binaires de release
partagent le même ensemble de dépendances Rust à quelques crates spécifiques
à la plateforme près, donc il documente la release dans son ensemble.

## Checklist de relecture PR

Lors de la relecture d'une PR qui touche l'infrastructure CI :

- Nouvelle ligne `uses:` ? Doit pinner un SHA de 40 caractères, tag
  en commentaire trailing.
- Nouvelle ligne `FROM` dans un Dockerfile ? Doit pinner
  `image@sha256:<digest>`, sauf si `FROM scratch`.
- Nouvelle dépendance Cargo ? `cargo audit` doit passer sur la PR.
  Si un nouvel advisory est inévitable, le contributeur doit
  ajouter une entrée `audit.toml` avec la même profondeur d'analyse
  que les entrées existantes.
- Nouveau workflow ? Bloc `permissions:` au niveau job, par défaut
  `contents: read`, opt-in sur des scopes plus larges uniquement
  quand c'est nécessaire.
- `permissions: write-all` au top-level ? À rejeter. Utiliser des
  scopes job-level à la place.

Les commandes de vérification ci-dessus peuvent tourner localement
avant de push pour s'assurer que la PR est clean.
