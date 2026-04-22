# perf-sentinel dashboard browser tests

Playwright smoke suite for the single-file HTML dashboard emitted by
`perf-sentinel report`. Covers interactions that Rust-level tests
cannot reach: live DOM state, clipboard, keyboard, CSV blob content.

## Quickstart

```sh
cd crates/sentinel-cli/tests/browser
npm ci
npx playwright install chromium
npx playwright test
```

The suite's `global-setup.ts` step:

1. Builds the release binary with `cargo build --release --bin
   perf-sentinel` when `target/release/perf-sentinel` is missing.
2. Renders an HTML dashboard from
   `tests/fixtures/report_realistic.json` plus the pg_stat CSV
   fixture into `fixtures/dashboard.html`.
3. Spawns `http-server` on a free 127.0.0.1 port rooted at that
   directory. `http://` is required by `navigator.clipboard`, which
   refuses `file://` origins.

## Why an HTTP server

One spec (`9. Copy link button`) reads `navigator.clipboard` after a
user gesture. Chromium silently disables the Clipboard API on
`file://` pages even with the permission granted. `http-server`
supplies a tiny local HTTP origin that satisfies the API without
pulling in a heavy test framework.

## CI

Runs as a separate `browser-tests` job in `.github/workflows/ci.yml`
so the Rust-only `check` job isn't slowed by the Playwright install.
Uses `actions/setup-node@v6.4.0` with Node 24, installs Chromium via
`npx playwright install --with-deps chromium`, then runs this suite.
Uploads the HTML report as a retained artifact on failure.

## Dashboard demo GIFs and still frames

`npm run demo` regenerates three kinds of artefacts under
`docs/img/report/`:

- `dashboard_dark.gif` and `dashboard_light.gif`: the scripted tour
  recorded twice (one project per primary theme, ~28 s each,
  palette-optimised at 1000 px / 15 fps).
- `findings.png` + `findings-dark.png`, ..., `cheatsheet.png` +
  `cheatsheet-dark.png`: one light + one dark still per tab,
  captured at 1280 x 720 so the README's `<picture>` tags can serve
  the right variant via `prefers-color-scheme`.

```sh
cd crates/sentinel-cli/tests/browser
npm run demo
```

Requires ffmpeg on the PATH. Edit `demo/tour.spec.ts` for the GIF
scenario, `demo/stills.spec.ts` for the still frames, and
`demo/build-gif.sh` for the ffmpeg pipeline.

Each run overwrites every committed asset (~5 MB total: 2 GIFs + 12
PNGs), so each invocation creates new git blobs. Re-run only when
the dashboard surface changes meaningfully (new tab, layout shift,
shortcut rebinding) rather than on every doc tweak, otherwise the
repo accumulates stale large objects.
