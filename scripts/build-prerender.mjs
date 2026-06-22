// Bakes a static SEO snapshot of the dc-runtime pages (home + guide) so crawlers
// see real content instead of {{ token }} placeholders. The dc-runtime reads its
// template from <x-dc> then replaces it with <div id="dc-root">; we hide <x-dc>,
// add a sibling #seo-prerender (the rendered HTML, scripts/iframes stripped) and a
// CSS rule that hides it once #dc-root is populated.
//
// Two outputs per page:
//   - EN: the source file in place, gets the EN prerender + reciprocal hreflang.
//   - FR: a localized copy under fr/ (lang=fr, FR <head>, FR prerender, defaultLang
//     flipped to fr, links made root-absolute since it sits one dir deep).
// The dc-runtime does NOT localize <title>/<meta>, so the FR head strings live in
// PAGES[].fr below, maintained next to the EN head.
//
// Self-contained: starts its own static server (extensionless routing) and resolves
// Playwright from local node_modules (CI) or the main repo (local dev). Idempotent.
// Run: node scripts/build-prerender.mjs   (CI runs it via .github/workflows/prerender.yml)
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync, existsSync, statSync, mkdirSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, join, extname } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
let chromium;
try { ({ chromium } = require('playwright')); }
catch { ({ chromium } = require('/Users/robintrassard/RustroverProjects/perf-sentinel/crates/sentinel-cli/tests/browser/node_modules/playwright')); }

const SITE = join(dirname(fileURLToPath(import.meta.url)), '..');
const PORT = 8799;
const ORIGIN = 'https://perf-sentinel.dev';
const PAGES = [
  {
    file: 'index.html', path: '/', frFile: 'fr/index.html',
    enUrl: ORIGIN + '/', frUrl: ORIGIN + '/fr',
    fr: {
      title: "perf-sentinel : détection d'anti-patterns d'I/O, chiffrée en carbone",
      desc: "Binaire auto-hébergeable qui détecte les anti-patterns d'I/O (N+1, appels redondants, requêtes lentes, fanout) dans vos traces OpenTelemetry et les chiffre en énergie et en CO₂.",
      ogDesc: "Repérez les I/O gaspillées (N+1, appels redondants, requêtes lentes, fanout) dans vos traces OpenTelemetry, chiffrées en énergie et en carbone.",
    },
  },
  {
    file: 'guide.html', path: '/guide', frFile: 'fr/guide.html',
    enUrl: ORIGIN + '/guide', frUrl: ORIGIN + '/fr/guide',
    fr: {
      title: "perf-sentinel : documentation",
      desc: "Docs perf-sentinel : démarrage rapide, installation, configuration, référence CLI et référence des métriques GreenOps. Auto-hébergé, OpenTelemetry, AGPL-3.0.",
      ogDesc: "Démarrage rapide, installation, configuration, référence CLI et métriques GreenOps pour perf-sentinel. Auto-hébergé, OpenTelemetry, AGPL-3.0.",
    },
  },
];

const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.json': 'application/json', '.png': 'image/png', '.gif': 'image/gif', '.ico': 'image/x-icon', '.woff2': 'font/woff2', '.woff': 'font/woff', '.cast': 'text/plain', '.xml': 'application/xml', '.txt': 'text/plain' };

function startServer() {
  const server = createServer(async (req, res) => {
    let p = decodeURIComponent(req.url.split('?')[0]);
    if (p.endsWith('/')) p += 'index.html';
    let fp = join(SITE, p);
    if (existsSync(fp) && statSync(fp).isDirectory()) fp = join(fp, 'index.html');
    if (!existsSync(fp) && !extname(fp) && existsSync(fp + '.html')) fp += '.html';
    if (!existsSync(fp)) { res.writeHead(404); res.end('not found'); return; }
    try {
      const buf = await readFile(fp);
      res.writeHead(200, { 'Content-Type': MIME[extname(fp)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
      res.end(buf);
    } catch { res.writeHead(500); res.end('error'); }
  });
  return new Promise((resolve) => server.listen(PORT, '127.0.0.1', () => resolve(server)));
}

// #seo-prerender sits BEFORE <x-dc> so non-JS readers (LLM fetchers, naive
// readability) hit real text first, not the {{ token }} template. :has() hides
// it post-hydration regardless of order. ponytail: :has() => both blocks show
// on pre-2023 browsers, acceptable for a marketing page.
const CSS = '<style id="seo-prerender-css">x-dc{display:none}body:has(#dc-root:not(:empty)) #seo-prerender{display:none}</style>';
const START = '<!--seo-prerender-start-->';
const END = '<!--seo-prerender-end-->';
const HL_START = '<!--hreflang-start-->';
const HL_END = '<!--hreflang-end-->';

const esc = (t) => t.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

// Render a page with the dc-runtime and return the hydrated #dc-root innerHTML,
// stripped of scripts/iframes/ids so it can't shadow the live DOM. lang=null keeps
// the default (en); lang='fr' forces the FR dictionary via localStorage.
async function renderPrerender(browser, path, lang) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  await page.goto(`http://127.0.0.1:${PORT}${path}`, { waitUntil: 'load' });
  await page.evaluate((l) => { try { localStorage.clear(); if (l) localStorage.setItem('ps-lang', l); } catch (e) {} }, lang);
  await page.reload({ waitUntil: 'load' });
  await page.waitForFunction(
    () => { const r = document.getElementById('dc-root'); return r && r.children.length && !/\{\{/.test(r.textContent); },
    { timeout: 30000 });
  await page.waitForTimeout(600);
  const html = await page.evaluate(() => {
    const clone = document.getElementById('dc-root').cloneNode(true);
    clone.querySelectorAll('script, iframe').forEach((e) => e.remove());
    clone.querySelectorAll('[id]').forEach((e) => e.removeAttribute('id'));
    clone.querySelectorAll('[data-ps-root]').forEach((e) => e.removeAttribute('data-ps-root'));
    return clone.innerHTML;
  });
  await page.close();
  return html;
}

function injectPrerender(s, html) {
  s = s.replace(/<style id="seo-prerender-css">[\s\S]*?<\/style>/g, '');
  s = s.replace(new RegExp(START + '[\\s\\S]*?' + END, 'g'), '');
  s = s.replace('</head>', CSS + '</head>');
  const block = START + '<div id="seo-prerender">' + html + '</div>' + END;
  // Inject before the <x-dc> open tag (function replacer: html may contain $&).
  return s.replace(/<x-dc(\s|>)/, (m) => block + m);
}

// Reciprocal hreflang set, identical on the EN and FR copies of a page.
function injectHreflang(s, p) {
  const block = HL_START
    + `<link rel="alternate" hreflang="en" href="${p.enUrl}">`
    + `<link rel="alternate" hreflang="fr" href="${p.frUrl}">`
    + `<link rel="alternate" hreflang="x-default" href="${p.enUrl}">`
    + HL_END;
  s = s.replace(new RegExp(HL_START + '[\\s\\S]*?' + HL_END, 'g'), '');
  return s.replace(/(<link rel="canonical"[^>]*>)/, (m) => m + block);
}

// Localize the <head> + dc-runtime default. The runtime does not touch the title
// or meta, so we swap them here; canonical/og:url point at the FR URL.
function localizeHead(s, p) {
  s = s.replace('<html lang="en">', '<html lang="fr">');
  s = s.replace(/<title>[\s\S]*?<\/title>/, `<title>${esc(p.fr.title)}</title>`);
  s = s.replace(/(<meta name="description" content=")[^"]*(">)/, (m, a, b) => a + esc(p.fr.desc) + b);
  s = s.replace(/(<meta property="og:title" content=")[^"]*(">)/, (m, a, b) => a + esc(p.fr.title) + b);
  s = s.replace(/(<meta property="og:description" content=")[^"]*(">)/, (m, a, b) => a + esc(p.fr.ogDesc) + b);
  s = s.replace(/(<link rel="canonical" href=")[^"]*(">)/, (m, a, b) => a + p.frUrl + b);
  s = s.replace(/(<meta property="og:url" content=")[^"]*(">)/, (m, a, b) => a + p.frUrl + b);
  s = s.replace(/"inLanguage":"en"/g, '"inLanguage":"fr"');
  // Flip the dc-runtime defaultLang (HTML-escaped JSON in data-props) en -> fr so a
  // FR landing with no stored choice renders FR without persisting over a user's pick.
  s = s.replace(/(&quot;defaultLang&quot;:\{[\s\S]*?&quot;default&quot;:&quot;)en(&quot;)/, (m, a, b) => a + 'fr' + b);
  return s;
}

// FR pages live one directory deep (fr/), so every relative href/src must resolve
// from the root. No href=/src= literals exist in the JS region, so this is safe.
function rootAbsoluteLinks(s) {
  s = s.replace(/(\b(?:href|src)=")\.\//g, '$1/');
  return s.replace(/(\b(?:href|src)=")(?!\/|#|https?:|mailto:|data:|\{\{)/g, '$1/');
}

const server = await startServer();
const browser = await chromium.launch({ args: ['--no-sandbox', '--disable-dev-shm-usage'] });
let changed = 0;
for (const p of PAGES) {
  const src = readFileSync(join(SITE, p.file), 'utf8');

  // EN: refresh prerender + hreflang in place.
  const enHtml = await renderPrerender(browser, p.path, null);
  let en = injectHreflang(src, p);
  en = injectPrerender(en, enHtml);
  if (en !== src) { writeFileSync(join(SITE, p.file), en); changed++; }
  console.log(`  ${p.file}: EN prerender ${Math.round(enHtml.length / 1024)} KB (${en !== src ? 'updated' : 'unchanged'})`);

  // FR: derive a localized copy under fr/.
  const frHtml = await renderPrerender(browser, p.path, 'fr');
  let fr = localizeHead(src, p);
  fr = injectHreflang(fr, p);
  fr = injectPrerender(fr, frHtml);
  fr = rootAbsoluteLinks(fr);
  const frPath = join(SITE, p.frFile);
  const frBefore = existsSync(frPath) ? readFileSync(frPath, 'utf8') : '';
  if (fr !== frBefore) { mkdirSync(dirname(frPath), { recursive: true }); writeFileSync(frPath, fr); changed++; }
  console.log(`  ${p.frFile}: FR prerender ${Math.round(frHtml.length / 1024)} KB (${fr !== frBefore ? 'updated' : 'unchanged'})`);
}
await browser.close();
server.close();
console.log(`OK prerender (${changed} file(s) updated)`);
