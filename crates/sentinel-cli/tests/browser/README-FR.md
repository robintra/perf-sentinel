# Tests navigateur du tableau de bord perf-sentinel

Suite Playwright ciblée sur le tableau de bord HTML mono-fichier
émis par `perf-sentinel report`. Couvre les interactions que les
tests Rust ne peuvent pas atteindre : état DOM en direct, presse-papiers,
clavier, contenu du blob CSV.

## Démarrage rapide

```sh
cd crates/sentinel-cli/tests/browser
npm ci
npx playwright install chromium
npx playwright test
```

L'étape `global-setup.ts` de la suite :

1. Construit le binaire release via `cargo build --release --bin
   perf-sentinel`, à chaque exécution, même quand le binaire existe
   déjà. La suite teste ce que le template embarqué produit, donc
   un binaire périmé la ferait passer sur du code qui n'existe plus.
   Quand rien n'a changé, Cargo rend la main en quelques secondes.
2. Rend un tableau de bord HTML à partir de
   `tests/fixtures/report_realistic.json` et du fichier pg_stat CSV
   vers `fixtures/dashboard.html`.
3. Lance `http-server` sur un port libre de 127.0.0.1 avec ce
   répertoire comme racine. Le protocole `http://` est exigé par
   `navigator.clipboard`, qui refuse les origines `file://`.

## Pourquoi un serveur HTTP

Le test `9. Copy link button` lit `navigator.clipboard` après un
geste utilisateur. Chromium désactive silencieusement l'API
Clipboard sur les pages `file://` même lorsque la permission est
accordée. `http-server` fournit une petite origine HTTP locale qui
satisfait l'API sans embarquer un framework lourd.

## CI

Exécutée comme job `browser-tests` séparé dans `.github/workflows/ci.yml`
pour ne pas ralentir le job `check` purement Rust avec l'installation
de Playwright. Utilise `actions/setup-node@v6.4.0` avec Node 24,
installe Chromium via `npx playwright install --with-deps chromium`,
puis lance la suite. Le rapport HTML est conservé en artefact en cas
d'échec.

## GIFs de démo et captures du tableau de bord

`npm run demo` régénère trois types d'artefacts dans
`docs/img/report/` :

- `dashboard_dark.gif` et `dashboard_light.gif` : le parcours scripté
  enregistré deux fois (un projet par thème primaire, ~28 s chacun,
  palette optimisée en 1000 px / 15 fps).
- `findings.png` + `findings-dark.png`, ..., `greenops.png` +
  `greenops-dark.png`, `cheatsheet.png` + `cheatsheet-dark.png` :
  une capture en thème clair + une en thème sombre par onglet (sept
  onglets au total), prises en 1280 x 720 pour que les balises
  `<picture>` du README servent la bonne variante via
  `prefers-color-scheme`.

```sh
cd crates/sentinel-cli/tests/browser
npm run demo
```

Nécessite ffmpeg dans le PATH. Éditer `demo/tour.spec.ts` pour le
scénario des GIFs, `demo/stills.spec.ts` pour les captures, et
`demo/build-gif.sh` pour le pipeline ffmpeg.

Chaque exécution écrase toutes les images versionnées (~5 Mo au total :
2 GIFs + 12 PNGs), donc chaque invocation crée de nouveaux blobs git.
Régénérer uniquement quand la surface du dashboard change
significativement (nouvel onglet, refonte de la mise en page,
réaffectation de raccourcis) plutôt qu'à chaque retouche de doc, sinon
le dépôt accumule des objets volumineux périmés.
