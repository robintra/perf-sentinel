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
   perf-sentinel` lorsque `target/release/perf-sentinel` est absent.
2. Rend un tableau de bord HTML à partir de
   `tests/fixtures/report_realistic.json` et du fichier pg_stat CSV
   vers `fixtures/dashboard.html`.
3. Lance `http-server` sur un port libre de 127.0.0.1 avec ce
   répertoire comme racine. Le protocole `http://` est exigé par
   `navigator.clipboard`, qui refuse les origines `file://`.

## Pourquoi un serveur HTTP

Le spec `9. Copy link button` lit `navigator.clipboard` après un
geste utilisateur. Chromium désactive silencieusement l'API
Clipboard sur les pages `file://` même lorsque la permission est
accordée. `http-server` fournit une petite origine HTTP locale qui
satisfait l'API sans embarquer un framework lourd.

## CI

Exécutée dans un job `browser-tests` séparé de `.github/workflows/ci.yml`
pour ne pas ralentir le job `check` purement Rust avec l'installation
de Playwright. Utilise `actions/setup-node@v6.4.0` avec Node 20,
installe Chromium via `npx playwright install --with-deps chromium`,
puis lance la suite. Le rapport HTML est conservé en artefact en cas
d'échec.
