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

Compliance check au 2026-05-03 :

- **GitHub Actions** : 100 % des lignes `uses:` à travers les 9
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
        patterns: ["actions/*", "dtolnay/*", "Swatinem/*", "taiki-e/*", "actions-rust-lang/*"]
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

Depuis v0.7.0, chaque binaire de release officiel perf-sentinel porte
une attestation SLSA niveau L2. L'attestation est générée par GitHub
Actions via `slsa-framework/slsa-github-generator` (pinné par SHA,
tag en commentaire) et publiée comme asset du Release GitHub à
côté du binaire sous le nom `multiple.intoto.jsonl`.

Pourquoi L2 et pas L3 : le générateur reusable peut atteindre L3
s'il tourne sur un runner durci et audité. perf-sentinel utilise
les runners partagés `ubuntu-latest` avec des permissions réduites
au niveau du job, pas un runner isolé. L2 est la revendication
honnête, et c'est la valeur portée par
`integrity.binary_attestation.slsa_level` dans chaque rapport
périodique produit par un tel binaire.

Vérifier un binaire téléchargé :

```bash
slsa-verifier verify-artifact \
  --provenance-path multiple.intoto.jsonl \
  --source-uri github.com/robintra/perf-sentinel \
  --source-tag v0.7.0 \
  perf-sentinel-linux-amd64
```

Une vérification réussie confirme que le binaire vient du tag
spécifié de ce repo, construit par GitHub Actions, pas par un
tiers. Combiner avec la subcommand `verify-hash` sur un rapport
périodique pour vérifier la chaîne complète :
`source -> SLSA -> binaire -> rapport -> signature Sigstore`.

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
