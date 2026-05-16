# Guide CI perf-sentinel

Côté CI : comment exécuter perf-sentinel en mode batch contre un fixture de traces produit par votre stage de tests d'intégration, et faire remonter les findings sur chaque pull request. Pour les topologies, voir [`INTEGRATION-FR.md`](./INTEGRATION-FR.md). Pour l'instrumentation côté application, voir [`INSTRUMENTATION-FR.md`](./INSTRUMENTATION-FR.md).

## Sommaire

- [Mode CI (analyse batch)](#mode-ci-analyse-batch) : l'invocation CLI sous-jacente et la sémantique des codes de sortie derrière chaque recette ci-dessous.
- [Recettes d'intégration CI](#recettes-dintégration-ci) : templates copier-coller pour GitHub Actions, GitLab CI et Jenkins, plus la philosophie du quality gate et le chemin du rapport HTML interactif pour chaque provider.
- [Détection de régressions sur PR (sous-commande `diff`)](#détection-de-régressions-sur-pr-sous-commande-diff) : compare un set de traces de PR à un set de traces baseline pour signaler les régressions.

## Mode CI (analyse batch)

Pour les pipelines CI, utilisez le mode batch au lieu du mode daemon :

```bash
perf-sentinel analyze --ci --input traces.json
```

Le code de sortie est non-zero si le quality gate echoue. Configurez les seuils dans `.perf-sentinel.toml` :

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30
```

---

## Recettes d'intégration CI

Des templates prêts à copier pour les trois principaux fournisseurs CI sont
disponibles dans [`docs/ci-templates/`](../ci-templates/). Choisissez celui
qui correspond à votre fournisseur, déposez-le dans votre dépôt, adaptez
les trois variables identifiées dans le bloc de commentaire en tête du
template (version pinnée, chemin du fichier de traces, chemin de la
config) et c'est terminé.

| Fournisseur    | Template                                                   | Ce qui apparaît                                                 |
|----------------|------------------------------------------------------------|-----------------------------------------------------------------|
| GitHub Actions | [`github-actions.yml`](../ci-templates/github-actions.yml) | SARIF dans GitHub Code Scanning + commentaire sticky sur la PR  |
| GitLab CI      | [`gitlab-ci.yml`](../ci-templates/gitlab-ci.yml)           | Artifact SARIF + widget Code Quality sur la MR                  |
| Jenkins        | [`jenkinsfile.groovy`](../ci-templates/jenkinsfile.groovy) | Arbre de findings Warnings Next Generation + courbe de tendance |

### Philosophie du quality gate

Les trois templates exécutent `perf-sentinel analyze --ci` comme étape
de gating. Le flag `--ci` ne fait qu'une seule chose : si l'un des
seuils définis dans la section `[thresholds]` de `.perf-sentinel.toml`
est dépassé, le processus sort avec le code `1`. Les trois templates
traduisent ensuite ce code de sortie en un résultat de build qui
dépend du **déclencheur** du run :

- **Sur une pull request** : le gate bloque. Un build rouge sur une PR
  est le signal dont l'auteur a besoin pour corriger le finding avant
  le merge. C'est le moment du workflow où le coût de correction est
  le plus faible, l'auteur est encore dans le contexte et le code
  n'est pas encore sur trunk.
- **Sur un push vers trunk (branche par défaut)** : le gate est
  informatif seulement. Le SARIF est toujours remonté vers Code
  Scanning, le widget Code Quality ou Warnings NG, donc les dashboards
  et l'analyse de tendance continuent de fonctionner, mais le build
  reste vert. Une fois la PR acceptée et mergée, c'est le développeur
  qui décide quand et quoi envoyer en production. perf-sentinel ne
  doit pas se mettre entre un commit mergé et une release.

Ce split évite le mode d'échec classique des gates PR qui enforcent
aussi sur trunk : main reste rouge plus longtemps que prévu,
l'équipe contourne, et l'outil finit par être désactivé.

La configuration recommandée exécute perf-sentinel **deux fois** dans
le même job : une fois sans `--ci` pour toujours produire un artifact
SARIF (les reviewers peuvent inspecter les findings même quand le
gate échoue), et une fois avec `--ci` pour appliquer le gate. Les
templates Jenkins et GitLab font cela explicitement. Le template
GitHub utilise `continue-on-error` pour obtenir le même effet en une
seule invocation.

Mécaniques par fournisseur pour le split PR vs trunk :

- **GitHub Actions** découpe l'enforcement en deux steps. Le step PR
  tourne quand `github.event_name == 'pull_request'` et appelle
  `exit 1` sur breach. Le step trunk tourne sur le trigger push et
  émet une annotation `::warning::` sans faire fail le job.
- **GitLab CI** utilise `allow_failure: true` sur la règle
  `$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH`. Le job tourne toujours
  et retourne toujours exit code 1 sur breach, mais le badge de
  pipeline reste vert et le job apparaît avec une icône
  d'avertissement jaune.
- **Jenkins** utilise un garde `when { expression { env.CHANGE_ID !=
  null } }` sur le stage `Quality gate (PR only)`. `CHANGE_ID` est
  renseigné par MultiBranch Pipeline uniquement sur les builds de
  pull request. Sur les builds de branche, le stage est sauté
  entièrement. Le `qualityGates` de Warnings NG est aussi rendu
  conditionnel sur `CHANGE_ID` pour que le bloc post ne réintroduise
  pas le blocage sur trunk.

### Rapport interactif via GitHub Pages

Le sticky comment de PR (bloc markdown avec comptage des findings et
statut du quality gate) donne aux reviewers une vue d'ensemble
immédiate. Pour une inspection plus approfondie (arbre des spans avec
les N+1 surlignés, suggested fix framework-specific, drill-down
pg_stat, Diff complet contre trunk), le template GitHub Actions publie
optionnellement un **dashboard HTML complet** sur GitHub Pages à
chaque PR, lié depuis le sticky comment sous la forme :

> 📊 **Rapport interactif (vue Diff)** → `https://<owner>.github.io/<repo>/perf-sentinel-reports/pr-<N>/index.html#diff`

Cliquer sur le lien ouvre le rapport sur la tab Diff, qui est la vue
naturelle pour un reviewer : nouveaux findings introduits par la PR,
findings résolus (régressions corrigées), changements de sévérité et
deltas des métriques I/O par endpoint. Les autres tabs (Findings,
Explain, pg_stat, Correlations, GreenOps) sont à un clic via la barre
d'onglets.

Les rapports sont des HTML single-file auto-contenus avec routing par
hash, donc partager un finding précis revient à copier l'URL depuis
la barre d'adresse.

**Tier GitHub Pages requis**. Sur un compte GitHub Free personnel,
Pages n'est disponible que pour les dépôts publics. Les dépôts privés
nécessitent GitHub Pro, Team ou Enterprise Cloud. Voir
[les plans GitHub](https://docs.github.com/en/get-started/learning-about-github/githubs-products)
pour la liste à jour. Activer Pages sur un dépôt privé avec un compte
Free laisse le push de branche réussir, mais Pages sert du 404 en
permanence sans erreur dans le log Actions. Il faut soit upgrader le
compte, soit rendre le dépôt public, soit sauter le bloc Pages et
rester sur le mode SARIF + sticky comment markdown.

**Mise en place** (opt-in, nécessite GitHub Pages sur le dépôt) :

1. Créer une branche `gh-pages` vide dans le dépôt (bootstrap standard
   GitHub Pages, à faire une seule fois).
2. Activer GitHub Pages dans `Settings -> Pages`, source = branche
   `gh-pages`, dossier = `/ (root)`.
3. Copier le workflow baseline companion depuis
   [`docs/ci-templates/github-actions-baseline.yml`](../ci-templates/github-actions-baseline.yml)
   vers `.github/workflows/perf-sentinel-baseline.yml`. Il tourne sur
   chaque push vers `main` et stocke le rapport baseline sous
   `gh-pages/perf-sentinel-reports/baseline.json`.
4. Copier le workflow de cleanup depuis
   [`docs/ci-templates/github-actions-report-cleanup.yml`](../ci-templates/github-actions-report-cleanup.yml)
   vers `.github/workflows/perf-sentinel-report-cleanup.yml`. Il
   tourne à la fermeture de PR et supprime le répertoire par-PR.
5. Décommenter les blocs `Download baseline from gh-pages`, `Generate
   interactive HTML report`, `Checkout gh-pages worktree` et `Publish
   report to gh-pages` dans votre workflow principal (le commentaire
   d'en-tête dans
   [`docs/ci-templates/github-actions.yml`](../ci-templates/github-actions.yml)
   les localise).

Une fois les trois workflows en place, chaque PR obtient son propre
rapport interactif à une URL stable :

```
https://<owner>.github.io/<repo>/perf-sentinel-reports/pr-<N>/
```

Le baseline est rafraîchi à chaque push vers `main`, donc la tab Diff
compare toujours les traces de la PR contre le dernier état mergé.

Si GitHub Pages n'est pas activé, le template retombe sur le sticky
comment markdown seulement. Aucun changement de comportement pour les
adoptants existants.

**Limitations des PRs fork**. Le step `Post PR comment` est marqué
`continue-on-error: true` parce que les PRs fork reçoivent un
`GITHUB_TOKEN` read-only quelles que soient les `permissions:`
déclarées au niveau workflow. Sans la tolérance, chaque PR fork
ferait passer le CI en rouge au step sticky-comment même quand le
reste de la pipeline a réussi. Avec la tolérance en place, les PRs
fork uploadent quand même leur SARIF dans l'onglet Security et l'UI
Checks montre le résultat du quality gate, mais aucun sticky comment
n'apparaît dans la conversation de la PR. Les PRs internes au même
dépôt (contributeurs internes, même org) gardent l'expérience
complète, sticky comment inclus. Les projets pour qui le sticky
comment sur PRs fork est un requis dur doivent migrer vers le
pattern `pull_request_target` + `workflow_run` documenté par
[GitHub Security Lab](https://securitylab.github.com/research/github-actions-preventing-pwn-requests/).
Ce pattern sépare la pipeline en un workflow read-only qui build et
upload des artefacts, et un workflow write-enabled déclenché par
`workflow_run` qui download ces artefacts et poste le commentaire.
Il n'est pas le défaut du template parce qu'il double la surface YAML
et demande un passage d'artefacts soigné, pas proportionné pour un
template starter.

**Trade-off de concurrency**. Le guard `concurrency.group:
gh-pages-deploy` sérialise les runs de ce workflow avec les workflows
baseline et cleanup, pour que trois PRs fermées dans la même minute
ne se marchent pas dessus sur gh-pages. Comme le guard est déclaré
au niveau workflow, il sérialise aussi les runs qui ne toucheraient
pas Pages (par exemple quand les blocs Pages sont commentés). Les
dépôts à fort débit de PRs peuvent splitter les étapes Pages dans un
job dédié et restreindre la concurrency à ce job. Sauté ici pour
garder le template compact.

**Dépendances**. Le deploy utilise du `git` en clair contre la branche
`gh-pages`, authentifié par le `GITHUB_TOKEN` intégré et la permission
`contents: write` déclarée au niveau workflow. Aucune action tierce
de deploy n'est requise, ce qui garde le template exempt de surface
supply-chain pour le chemin d'upload. Seule `actions/checkout`
(pinnée) est réutilisée dans les trois workflows.

**Empreinte de stockage**. Un rapport typique fait 80 à 150 Ko. Avec
la rétention gérée par le workflow de cleanup, la branche gh-pages ne
porte que les rapports des PRs ouvertes plus l'unique
`baseline.json`. Pas de croissance illimitée.

**Autres fournisseurs**. Voir "Rapport interactif via GitLab Pages"
et "Rapport interactif via Jenkins HTML Publisher" ci-dessous.

### Rapport interactif via GitLab Pages

Équivalent du chemin GitHub Pages ci-dessus, adapté à la surface de
deployment native de GitLab. Deux blocs de template sont fournis dans
[`docs/ci-templates/gitlab-ci.yml`](../ci-templates/gitlab-ci.yml),
choisir celui qui correspond au tier GitLab.

**Note sur le tier**. Le mode deployment par MR (`pages.path_prefix`)
est documenté comme [Experiment, Tier: Premium ou
Ultimate](https://docs.gitlab.com/user/project/pages/#create-multiple-deployments),
et n'est pas disponible sur gitlab.com Free. En Free, le deployment
MR apparaît comme "Success" dans la liste des environments mais n'est
pas réellement servi. Un fallback compatible Free est fourni à côté.

| Bloc | Tier | Comportement |
| --- | --- | --- |
| `perf-sentinel-pages-simple` | Free | Un seul deployment sur la branche par défaut. Publie le snapshot trunk du rapport ET le baseline JSON à la racine des Pages du projet. Les reviewers de MR voient la vue trunk, pas l'analyse de leur MR. |
| `perf-sentinel-pages` | Premium ou Ultimate | Un deployment par MR sous le path prefix `mr-<IID>`, expiration auto 30 jours via `expire_in`. Baseline sur la branche par défaut à la racine Pages. Bouton natif "View deployment" sur l'UI MR. |

Choisir un bloc, pas les deux (ils se disputeraient le deployment racine).

**Mise en place** (opt-in, nécessite GitLab Pages activé sur le
projet) :

1. Activer GitLab Pages dans `Settings -> Pages` si ce n'est pas
   déjà fait.
2. Décommenter exactement un bloc dans
   [`docs/ci-templates/gitlab-ci.yml`](../ci-templates/gitlab-ci.yml).
   Les deux tournent dans le stage `perf-sentinel` et réutilisent
   `PERF_SENTINEL_VERSION / PERF_SENTINEL_TRACES / PERF_SENTINEL_CONFIG`
   déjà déclarées pour le job principal.
3. Pour `perf-sentinel-pages`, confirmer GitLab 17.9 ou plus récent.
   Non requis pour `perf-sentinel-pages-simple`.

**Comportement de `perf-sentinel-pages` (Premium ou Ultimate)**. Le
job différencie deux chemins de déclenchement via son bloc `rules:` :

- **Sur merge request** (`$CI_PIPELINE_SOURCE == "merge_request_event"`),
  fetch le baseline de trunk depuis la racine Pages du projet (strip
  le préfixe MR de `CI_PAGES_URL` via `${CI_PAGES_URL%/mr-[0-9]*}`,
  fallback 404 silencieux quand absent), produit `public/index.html`
  via `perf-sentinel report --output public/index.html`, déploie avec
  `path_prefix: "mr-${CI_MERGE_REQUEST_IID}"` et
  `pages.expire_in: 30 days`. `environment.url` pointe vers le
  `${CI_PAGES_URL}` actif, que GitLab résout vers l'URL de
  deployment MR-scoped au runtime.
- **Sur push vers la branche par défaut**, produit
  `public/perf-sentinel-reports/baseline.json` via
  `perf-sentinel analyze --format json`, déploie avec un
  `path_prefix` vide pour que le fichier atterrisse à la racine du
  site et que les deployments MR futurs puissent le fetcher.

**Comportement de `perf-sentinel-pages-simple` (Free)**. Tourne
uniquement sur la branche par défaut. Écrit à la fois
`public/index.html` (snapshot trunk interactif) et
`public/perf-sentinel-reports/baseline.json` en une passe, puis
déploie un seul site Pages à la racine du projet.

**Rétention**. `perf-sentinel-pages` délègue la rétention à GitLab.
Les deployments parallèles sont supprimés immédiatement quand la MR
est fermée ou mergée. Le `pages.expire_in: 30 days` du template sert
de filet pour les MRs ouvertes qui stagnent (le défaut GitLab est de
24 heures quand non renseigné, nous l'élargissons pour qu'une MR
longue garde son rapport en ligne). Mettre `expire_in: never`
désactive l'expiration temporelle et ne s'appuie que sur les
événements de close/merge. N'utiliser `never` que si l'équipe ferme
ou merge ses MRs de façon fiable, sinon les MRs abandonnées
s'accumulent jusqu'à saturer le quota. `perf-sentinel-pages-simple`
n'a pas de question de rétention, il garde un seul deployment écrasé
à chaque push sur la branche par défaut.

**Quota**. gitlab.com autorise jusqu'à 100 deployments parallèles
supplémentaires sur Premium et 500 sur Ultimate, par namespace en
plus du deployment principal. Les instances self-managed exposent la
limite via la configuration admin. `perf-sentinel-pages-simple` étant
un deployment unique, il n'est pas concerné. Pour les projets
proches du plafond sur `perf-sentinel-pages`, `expire_in` peut être
réduit, ou les MRs doivent être fermées/mergées rapidement pour
libérer des slots.

**Empreinte de stockage**. Un rapport typique fait 80 à 150 Ko et
un baseline JSON 10 à 50 Ko. Avec la rétention active sur le chemin
Premium, seules les MRs ouvertes plus le baseline courant consomment
de l'espace. Le chemin Free stocke un seul deployment.

**Dépendances**. Aucun composant GitLab CI tiers. Le job utilise
`curl` pour installer le binaire perf-sentinel pinné et le keyword
natif `pages:` pour le deployment. Aucun deploy token ou runner
token au-delà du `CI_JOB_TOKEN` par défaut n'est requis.

### Rapport interactif via Jenkins HTML Publisher

Équivalent des chemins GitHub et GitLab ci-dessus, adapté au
[plugin HTML Publisher](https://plugins.jenkins.io/htmlpublisher/)
pré-installé sur la plupart des Jenkins entreprise. Le plugin
expose le rapport à une URL stable `${BUILD_URL}perf-sentinel/` et
ajoute un lien "perf-sentinel" dans la sidebar du build, à côté du
rapport Warnings NG déjà configuré par le template.

Ouvrir ce lien pose le reviewer sur la tab Findings (vue de landing
par défaut quand aucun baseline n'est branché, voir la note Diff
ci-dessous). Les cinq autres tabs (Explain, pg_stat, Correlations,
GreenOps et une tab Diff grisée) sont à un clic via la barre
d'onglets.

**Prérequis du pipeline Jenkins** :

- Utiliser un **MultiBranch Pipeline** avec un plugin de
  branch-source installé (GitHub Branch Source, Bitbucket Branch
  Source, GitLab Branch Source ou Gitea Branch Source). Le test
  `env.CHANGE_ID` qui garde le stage de quality gate sur les builds
  PR n'est positionné que par ces plugins. Dans un Pipeline
  classique single-branch, `CHANGE_ID` est toujours null et le
  quality gate ne bloque jamais.
- Utiliser un **agent Linux** (ou un controller sans agent sur un
  hôte Linux). Le template s'appuie sur `sh`, `curl`, `sha256sum`,
  `chmod`, dont aucun n'est disponible par défaut sur les agents
  Windows.

**Mise en place** (opt-in, nécessite le plugin HTML Publisher sur
le controller) :

1. Vérifier que le plugin HTML Publisher (>= 1.10 pour la
   compatibilité CSP) est installé. Manage Jenkins -> Plugins ->
   Installed plugins, rechercher "HTML Publisher". Si absent,
   installer puis redémarrer le controller. Le plugin Warnings Next
   Generation utilisé par le reste du template doit être en
   >= 9.11.0 pour le tool SARIF.
2. Décommenter le stage `Generate interactive HTML report` dans
   [`docs/ci-templates/jenkinsfile.groovy`](../ci-templates/jenkinsfile.groovy),
   placé juste avant le stage `Quality gate (PR only)`.
3. Décommenter le bloc `publishHTML([...])` dans la section
   `post { always }` du même fichier. Il est apparié au stage
   ci-dessus, donc les deux doivent être activés ensemble pour que
   le lien apparaisse.

Une fois activé, chaque build (branch ou pull request) produit un
rapport disponible à
`${JENKINS_URL}/job/<job-name>/<build-number>/perf-sentinel/`. La
sidebar du build porte un lien "perf-sentinel" qui pointe toujours
vers le rapport du dernier build via `alwaysLinkToLastBuild: true`.
L'option `keepAll: true` retient les rapports par build, les
anciens builds restent donc navigables.

Si le rapport apparaît sans style avec une navigation par onglets
cassée, voir **Configurer Jenkins pour rendre le rapport
interactif** ci-dessous. Jenkins applique par défaut une Content
Security Policy stricte qui bloque le CSS et le JavaScript inline,
ce qui est la cause la plus fréquente d'une page sidebar
perf-sentinel sans style.

**Configurer Jenkins pour rendre le rapport interactif**.

Jenkins applique par défaut une
[Content Security Policy](https://www.jenkins.io/doc/book/security/configuring-content-security-policy/)
stricte au contenu servi depuis les workspaces de build. Le rapport
HTML perf-sentinel embarque CSS et JavaScript inline dans un seul
fichier autonome, ce que le CSP par défaut bloque. Sans relâcher la
policy ou utiliser une Resource Root URL, cliquer sur le lien
sidebar `${BUILD_URL}perf-sentinel/` affiche une page HTML sans
style avec une navigation par onglets cassée, et aucun message dans
le log du build.

Deux options pour corriger, par ordre de préférence :

**Option A : configurer une Resource Root URL** (Jenkins 2.200+,
recommandée). Sert le contenu utilisateur depuis un domaine séparé,
ce qui fait que le CSP de l'instance principale ne s'applique plus.
Définir l'URL dans `Manage Jenkins > System > Resource Root URL`.
Voir l'[aide intégrée](https://www.jenkins.io/doc/book/security/user-content/#resource-root-url)
pour les détails. Aucun changement de template requis, tous les
rapports de tous les jobs en bénéficient immédiatement.

**Option B : relâcher le CSP** (legacy, portée plus large). Définir
la propriété système Java suivante au démarrage du controller
Jenkins (ou la lancer une fois via la Script Console pour un test à
portée de session) :

```groovy
System.setProperty(
    "hudson.model.DirectoryBrowserSupport.CSP",
    "sandbox allow-scripts; default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline';"
)
```

Compromis :

- Affecte tout le contenu HTML servi par tous les jobs de
  l'instance, pas seulement les rapports perf-sentinel.
- Ajoute `'unsafe-inline'` pour les styles et les scripts.
  Acceptable sur une instance Jenkins où vous faites confiance aux
  jobs exécutés, risqué sur une instance multi-tenant avec des
  contributeurs non fiables.
- Revient au défaut au redémarrage de Jenkins, sauf si persisté via
  les options de démarrage (`JAVA_OPTS`, `jenkins.xml` ou unit
  systemd).

Une future release perf-sentinel pourrait produire un rapport
CSP-friendly (CSS et JavaScript dans des fichiers voisins) qui
fonctionnerait avec le CSP Jenkins par défaut. Pas de date
engagée.

**Tab Diff absente par défaut**. Contrairement à GitHub Actions et
GitLab CI où un workflow baseline companion rafraîchit
`baseline.json` à chaque push sur la branche par défaut, ce
template ne branche pas de baseline Jenkins. La tab Diff du rapport
est donc vide. Les utilisateurs qui veulent la tab Diff peuvent
étendre le template avec le
[plugin Copy Artifact](https://plugins.jenkins.io/copyartifact/)
pour tirer un `baseline.json` depuis le dernier build réussi du
job sur la branche par défaut, puis le passer à
`perf-sentinel report --before baseline.json`. Cette évolution est
hors scope pour ce template.

**Pas de posting automatique de PR comment**. Jenkins n'a pas de
mécanisme natif de commentaire de pull request équivalent au sticky
comment GitHub ou au widget Code Quality GitLab. Les reviewers qui
suivent un build Jenkins consultent la page du build directement,
comme pour les findings Warnings NG. Les équipes qui veulent un PR
comment peuvent brancher la CLI `gh` ou une API REST spécifique
depuis le pipeline, mais cela nécessite de gérer un token forge
dans les credentials Jenkins et reste hors scope pour ce template.

**Empreinte de stockage** par-build et retenue indéfiniment
(`keepAll: true`). Un rapport typique fait 80 à 150 Ko. Pour des
controllers Jenkins long-lived avec gros volume de builds, appairer
`publishHTML keepAll: true` avec le build discarder dans la config
du job (par exemple garder les N derniers builds) pour plafonner
l'empreinte.

### Où SARIF apparaît selon le fournisseur

- **GitHub Code Scanning** liste chaque finding dans l'onglet Security du
  dépôt, avec des annotations en ligne sur le diff de la PR quand le champ
  `code_location` est présent. Nécessite `permissions.security-events:
  write` sur le workflow.
- **Le widget Code Quality de GitLab** apparaît sur la page de merge
  request, avec des couleurs de sévérité dérivées du champ `severity` de
  perf-sentinel (`critical -> critical`, `warning -> major`, `info ->
  info`).
- **Jenkins Warnings Next Generation** publie un arbre de findings
  structuré avec une courbe de tendance par build. Le plugin comprend
  nativement SARIF v2.1.0 et supporte sa propre déclaration `qualityGates`
  comme défense en profondeur en plus du code de sortie `--ci` de
  perf-sentinel.

---

## Détection de régressions sur PR (sous-commande `diff`)

La sous-commande `diff` compare deux jeux de traces et émet un rapport delta qui liste les findings nouveaux, les findings résolus, les changements de sévérité et les deltas de comptage I/O par endpoint. L'usage naturel est un check PR qui compare les traces de la branche PR à celles de la branche de base.

```yaml
# .github/workflows/perf-sentinel-diff.yml
name: perf-sentinel diff

on:
  pull_request:
    branches: [main]

permissions:
  contents: read
  pull-requests: write

jobs:
  diff:
    runs-on: ubuntu-latest
    env:
      PERF_SENTINEL_VERSION: "0.7.3"
    steps:
      - uses: actions/checkout@b4ffde65f46336ab88eb53be808477a3936bae11 # v4.1.1
        with:
          fetch-depth: 0

      - name: Installer perf-sentinel
        run: |
          set -euo pipefail
          BASE_URL="https://github.com/robintra/perf-sentinel/releases/download/v${PERF_SENTINEL_VERSION}"
          curl -sSLf -o perf-sentinel-linux-amd64 "${BASE_URL}/perf-sentinel-linux-amd64"
          curl -sSLf -o SHA256SUMS.txt            "${BASE_URL}/SHA256SUMS.txt"
          grep 'perf-sentinel-linux-amd64' SHA256SUMS.txt | sha256sum -c -
          mkdir -p "${GITHUB_WORKSPACE}/bin"
          install -m 0755 perf-sentinel-linux-amd64 "${GITHUB_WORKSPACE}/bin/perf-sentinel"
          echo "${GITHUB_WORKSPACE}/bin" >> "${GITHUB_PATH}"

      # Lancer les tests d'intégration sur la branche PR et capturer les traces.
      - name: Collecter les traces de la branche PR
        run: ./scripts/run-integration-tests.sh
        env:
          OTEL_EXPORTER_OTLP_FILE_PATH: pr-traces.json

      # Re-jouer sur la branche de base.
      - name: Collecter les traces de la branche de base
        run: |
          git checkout ${{ github.event.pull_request.base.sha }} -- .
          ./scripts/run-integration-tests.sh
        env:
          OTEL_EXPORTER_OTLP_FILE_PATH: base-traces.json

      - name: Diff
        run: |
          perf-sentinel diff \
            --before base-traces.json \
            --after pr-traces.json \
            --config .perf-sentinel.toml \
            --format json \
            --output diff.json
          # SARIF pour GitHub Code Scanning (uniquement les nouveaux findings).
          perf-sentinel diff \
            --before base-traces.json \
            --after pr-traces.json \
            --config .perf-sentinel.toml \
            --format sarif \
            --output diff.sarif

      - name: Uploader le SARIF
        if: hashFiles('diff.sarif') != ''
        uses: github/codeql-action/upload-sarif@95e58e9a2cdfd71adc6e0353d5c52f41a045d225 # v4.35.2
        with:
          sarif_file: diff.sarif
          category: perf-sentinel-diff

      - name: Commenter le résumé de régression sur la PR
        run: |
          NEW=$(jq '.new_findings | length' diff.json)
          RESOLVED=$(jq '.resolved_findings | length' diff.json)
          REGRESSIONS=$(jq '[.severity_changes[] | select(.after_severity == "critical" or (.after_severity == "warning" and .before_severity == "info"))] | length' diff.json)
          {
            echo "## diff perf-sentinel vs base"
            echo
            echo "- $NEW finding(s) nouveau(x)"
            echo "- $RESOLVED finding(s) résolu(s)"
            echo "- $REGRESSIONS régression(s) de sévérité"
          } > pr-comment.md

      - uses: marocchino/sticky-pull-request-comment@0ea0beb66eb9baf113663a64ec522f60e49231c0 # v3.0.4
        with:
          header: perf-sentinel-diff
          path: pr-comment.md

      - name: Échouer sur régression
        run: |
          NEW=$(jq '.new_findings | length' diff.json)
          REGRESSIONS=$(jq '[.severity_changes[] | select(.after_severity == "critical")] | length' diff.json)
          if [ "$NEW" -gt 0 ] || [ "$REGRESSIONS" -gt 0 ]; then
            echo "::error::le diff introduit $NEW finding(s) nouveau(x) et $REGRESSIONS régression(s) critique(s)"
            exit 1
          fi
```

Ajustez la logique de seuil de la dernière étape selon la politique de votre équipe. Certaines équipes gatent sur tout nouveau finding, d'autres tolèrent les nouveaux findings Info et n'échouent que sur des régressions Warning ou Critical.

---

